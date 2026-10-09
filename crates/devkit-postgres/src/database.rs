use std::{
    fmt,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use devkit_common::tls::Trust;
use tokio::runtime::Runtime;
use tokio_postgres::{Client, Config, config::SslMode};
use tokio_postgres_rustls::MakeRustlsConnect;

/// Runs `work` on this process's Postgres runtime, started on first use.
fn block_on<F: Future>(work: F) -> Result<F::Output> {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    let runtime = match RUNTIME.get() {
        Some(runtime) => runtime,
        None => {
            let built = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("starting the Postgres runtime")?;
            RUNTIME.get_or_init(|| built)
        }
    };
    Ok(runtime.block_on(work))
}

enum State {
    Closed,
    Open(Client),
    /// The last attempt failed to connect or got no answer in time, and
    /// attempts wait out `wait` before trying again, so a process that finds
    /// the database unreachable or stalled pays its wait once.
    Failed {
        at: Instant,
        reason: String,
        unreachable: bool,
    },
}

/// One database, connected on first use and shared by everything built on
/// it. Each operation, connecting included, gives up after `wait`, or sooner
/// at a deadline [`Database::finish_by`] sets.
///
/// No statement is named and no state outlives a transaction, so the
/// database may sit behind a transaction-mode pooler.
pub struct Database {
    config: Option<Config>,
    /// What the connection trusts, read when it first connects, so a CA
    /// file that is slow or never ends to read counts against that wait.
    trust: Trust,
    /// The TLS setup built from `trust` by the first connect that needed it.
    tls: OnceLock<MakeRustlsConnect>,
    wait: Duration,
    /// Names the database in errors, such as "rules database".
    label: &'static str,
    /// When set, every operation gives up by then, whatever its wait.
    deadline: Mutex<Option<Instant>>,
    /// Called each time connecting fails, refused or rejected alike.
    on_connect_failure: OnceLock<Box<dyn Fn() + Send + Sync>>,
    state: Mutex<State>,
}

impl fmt::Debug for Database {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Database")
            .field("label", &self.label)
            .field("target", &self.target())
            .field("wait", &self.wait)
            .finish()
    }
}

impl Database {
    /// The database `url` names, a `postgres://` URL or libpq's `key=value`
    /// form, not connected yet. The connection always uses TLS and verifies
    /// the server's certificate against the default roots and `trust`; a
    /// server that offers no TLS is refused. `sslmode=disable` is the one way
    /// to connect in plaintext. An error never repeats the URL, which
    /// carries the password.
    pub fn new(url: &str, wait: Duration, trust: &Trust, label: &'static str) -> Result<Database> {
        let config = crate::config(url).map_err(|_| anyhow!("the {label} URL does not parse"))?;
        Ok(Database {
            config: Some(config),
            trust: trust.clone(),
            tls: OnceLock::new(),
            wait,
            label,
            deadline: Mutex::new(None),
            on_connect_failure: OnceLock::new(),
            state: Mutex::new(State::Closed),
        })
    }

    /// A database every operation on fails with `reason`, for a URL that is
    /// missing or does not parse.
    pub fn unusable(reason: impl fmt::Display) -> Database {
        Database {
            config: None,
            trust: Trust::default(),
            tls: OnceLock::new(),
            wait: Duration::ZERO,
            label: "database",
            deadline: Mutex::new(None),
            on_connect_failure: OnceLock::new(),
            state: Mutex::new(State::Failed {
                at: Instant::now(),
                reason: reason.to_string(),
                unreachable: false,
            }),
        }
    }

    /// Makes every operation from now on give up by `at`, so a caller with
    /// one budget for all of its database work stays inside it.
    pub fn finish_by(&self, at: Instant) {
        *self.deadline.lock().unwrap_or_else(|e| e.into_inner()) = Some(at);
    }

    /// Runs `f` each time connecting fails, for a caller whose URL may have
    /// gone stale.
    pub fn on_connect_failure(&self, f: impl Fn() + Send + Sync + 'static) {
        let _ = self.on_connect_failure.set(Box::new(f));
    }

    /// How long the next operation may take: its wait, cut short by the
    /// deadline.
    fn budget(&self) -> Duration {
        match *self.deadline.lock().unwrap_or_else(|e| e.into_inner()) {
            Some(at) => self.wait.min(at.saturating_duration_since(Instant::now())),
            None => self.wait,
        }
    }

    /// `host:port/dbname`, without the user or password.
    pub fn target(&self) -> String {
        match &self.config {
            Some(config) => crate::target(config),
            None => "no database".to_string(),
        }
    }

    /// Runs `op` on the connection within the wait. A connection that times
    /// out or closes is dropped, so the server rolls back what it left open,
    /// and a failure to connect or a timeout fails the calls after it, at
    /// once, until the wait has passed again. Errors from the network, the
    /// server or the wait name the database; the caller's own refusals read
    /// as they are.
    pub fn run<T>(&self, op: impl AsyncFn(&mut Client) -> Result<T>) -> Result<T> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let attempt = async {
            let client = self.client(&mut state).await?;
            op(client).await
        };
        let budget = self.budget();
        let (out, timed_out) =
            match block_on(async { tokio::time::timeout(budget, attempt).await })? {
                Ok(out) => (out, false),
                Err(_) => (Err(anyhow::Error::new(NoAnswer(budget))), true),
            };
        let out = out.map_err(|e| match timed_out || e.is::<tokio_postgres::Error>() {
            true => e.context(format!("{} {}", self.label, self.target())),
            false => e,
        });
        let connect_failed = matches!(*state, State::Closed) && out.is_err();
        if connect_failed && let Some(f) = self.on_connect_failure.get() {
            f();
        }
        let failed = match &*state {
            State::Closed => out.is_err(),
            State::Open(_) => timed_out,
            State::Failed { .. } => false,
        };
        match (&*state, &out) {
            (_, Err(e)) if failed => {
                *state = State::Failed {
                    at: Instant::now(),
                    reason: format!("{e:#}"),
                    unreachable: is_unreachable(e),
                }
            }
            (State::Open(client), _) if client.is_closed() => *state = State::Closed,
            _ => {}
        }
        out
    }

    async fn client<'a>(&self, state: &'a mut State) -> Result<&'a mut Client> {
        if let State::Failed {
            at,
            reason,
            unreachable,
        } = &*state
        {
            if self.config.is_none() || at.elapsed() < self.wait {
                return Err(anyhow::Error::new(Replayed {
                    reason: reason.clone(),
                    unreachable: *unreachable,
                }));
            }
            *state = State::Closed;
        }
        if let State::Open(client) = &*state
            && client.is_closed()
        {
            *state = State::Closed;
        }
        if let State::Closed = state {
            let config = self.config.as_ref().ok_or_else(|| anyhow!("no database"))?;
            let tls = match config.get_ssl_mode() {
                SslMode::Disable => None,
                _ => Some(self.tls().await?),
            };
            *state = State::Open(crate::connect(config, tls).await?);
        }
        match state {
            State::Open(client) => Ok(client),
            _ => unreachable!("connected above"),
        }
    }

    /// The TLS setup, built on a blocking thread the first time a connect
    /// needs it, so the caller's timeout covers reading the CA file.
    async fn tls(&self) -> Result<MakeRustlsConnect> {
        if let Some(tls) = self.tls.get() {
            return Ok(tls.clone());
        }
        let tls = crate::tls(&self.trust)
            .await
            .with_context(|| format!("setting up TLS for the {}", self.label))?;
        Ok(self.tls.get_or_init(|| tls).clone())
    }
}

/// An operation that ran out of its wait.
#[derive(Debug)]
struct NoAnswer(Duration);

impl fmt::Display for NoAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "no answer within {:?}", self.0)
    }
}

impl std::error::Error for NoAnswer {}

/// The failure an earlier operation met, repeated for one that came before
/// the wait passed.
#[derive(Debug)]
struct Replayed {
    reason: String,
    unreachable: bool,
}

impl fmt::Display for Replayed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for Replayed {}

/// Whether `e` is an operation that got no answer from the server: it could
/// not connect, timed out, or lost the connection. A refused login, a
/// caller's own refusal, a missing URL or a TLS setup that failed on this
/// machine is not.
pub fn is_unreachable(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        cause.downcast_ref::<NoAnswer>().is_some()
            || cause
                .downcast_ref::<Replayed>()
                .is_some_and(|r| r.unreachable)
            || cause
                .downcast_ref::<tokio_postgres::Error>()
                .is_some_and(|e| e.as_db_error().is_none())
    })
}
