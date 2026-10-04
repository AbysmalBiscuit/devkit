use std::{
    fmt,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use devkit_common::tls::Trust;
use tokio::runtime::Runtime;
use tokio_postgres::{Client, Config, NoTls, config::SslMode, error::SqlState};
use tokio_postgres_rustls::MakeRustlsConnect;

/// Everything devkit keeps, in a schema of its own so it stays out of the
/// tables an application, or Supabase's HTTP API, exposes. Every statement is
/// idempotent, so any number of processes may run it at once.
const SCHEMA: &str = "
CREATE SCHEMA IF NOT EXISTS devkit;
CREATE TABLE IF NOT EXISTS devkit.todos (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    root text NOT NULL,
    node text,
    description text NOT NULL,
    status text NOT NULL
        CHECK (status IN ('pending', 'in_progress', 'completed', 'cancelled')),
    holder text CHECK (status <> 'in_progress' OR holder IS NOT NULL),
    parent uuid,
    ord bigint NOT NULL,
    entry timestamptz NOT NULL DEFAULT clock_timestamp(),
    modified timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX IF NOT EXISTS todos_root_node ON devkit.todos (root, node);
CREATE TABLE IF NOT EXISTS devkit.activity (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    root text NOT NULL,
    at timestamptz NOT NULL,
    event jsonb NOT NULL
);
CREATE INDEX IF NOT EXISTS activity_root_at ON devkit.activity (root, at);
CREATE TABLE IF NOT EXISTS devkit.seen (
    root text NOT NULL,
    session text NOT NULL,
    agent text NOT NULL,
    at timestamptz NOT NULL,
    PRIMARY KEY (root, session, agent)
);
";

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
    },
}

/// One database, connected on first use and shared by the stores and logs
/// built on it. Each operation, connecting included, gives up after `wait`,
/// or sooner at a deadline [`Database::finish_by`] sets.
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
    /// When set, every operation gives up by then, whatever its wait.
    deadline: Mutex<Option<Instant>>,
    /// Called each time connecting fails, refused or rejected alike.
    on_connect_failure: OnceLock<Box<dyn Fn() + Send + Sync>>,
    state: Mutex<State>,
}

impl fmt::Debug for Database {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Database")
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
    /// to connect in plaintext.
    pub fn new(url: &str, wait: Duration, trust: &Trust) -> Result<Arc<Self>> {
        let mut config = url
            .parse::<Config>()
            .context("the todo database URL does not parse")?;
        // tokio-postgres's default, `prefer`, falls back to plaintext when
        // the server, or anyone between, declines TLS.
        if config.get_ssl_mode() == SslMode::Prefer {
            config.ssl_mode(SslMode::Require);
        }
        Ok(Arc::new(Self {
            config: Some(config),
            trust: trust.clone(),
            tls: OnceLock::new(),
            wait,
            deadline: Mutex::new(None),
            on_connect_failure: OnceLock::new(),
            state: Mutex::new(State::Closed),
        }))
    }

    /// A database every operation on fails with `reason`, for a URL that is
    /// missing or does not parse.
    pub fn unusable(reason: impl fmt::Display) -> Arc<Self> {
        Arc::new(Self {
            config: None,
            trust: Trust::default(),
            tls: OnceLock::new(),
            wait: Duration::ZERO,
            deadline: Mutex::new(None),
            on_connect_failure: OnceLock::new(),
            state: Mutex::new(State::Failed {
                at: Instant::now(),
                reason: reason.to_string(),
            }),
        })
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
        let Some(config) = &self.config else {
            return "no database".to_string();
        };
        let host = match config.get_hosts().first() {
            Some(tokio_postgres::config::Host::Tcp(host)) => host.clone(),
            #[cfg(unix)]
            Some(tokio_postgres::config::Host::Unix(path)) => path.display().to_string(),
            None => "localhost".to_string(),
        };
        let port = config.get_ports().first().copied().unwrap_or(5432);
        let db = config
            .get_dbname()
            .or(config.get_user())
            .unwrap_or("postgres");
        format!("{host}:{port}/{db}")
    }

    /// Connects, when not connected yet, and asks the server for one row.
    pub fn check(&self) -> Result<()> {
        self.run(async |client| {
            client.simple_query("SELECT 1").await?;
            Ok(())
        })
    }

    /// Runs `op` on the connection within the wait, creating a missing
    /// schema and running `op` once more. A connection that times out or
    /// closes is dropped, so the server rolls back what it left open, and a
    /// timeout fails the calls after it until the wait has passed again.
    pub(crate) fn run<T>(&self, op: impl AsyncFn(&mut Client) -> Result<T>) -> Result<T> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let attempt = async {
            let client = self.client(&mut state).await?;
            match op(client).await {
                Err(e) if missing_schema(&e) => {
                    // Another process creating the schema at the same moment
                    // can make this fail; the retry then finds it.
                    let created = client.batch_execute(SCHEMA).await;
                    match (op(client).await, created) {
                        (Err(e), Err(schema)) if missing_schema(&e) => Err(e.context(format!(
                            "creating devkit's schema failed: {:#}",
                            anyhow::Error::from(schema)
                        ))),
                        (out, _) => out,
                    }
                }
                out => out,
            }
        };
        let budget = self.budget();
        let (out, timed_out) =
            match block_on(async { tokio::time::timeout(budget, attempt).await })? {
                Ok(out) => (out, false),
                Err(_) => (Err(anyhow!("no answer within {budget:?}")), true),
            };
        // The store's own refusals, an unknown id or a claim, read as they
        // are; anything the database or the network said names the database.
        let out = out.map_err(|e| match timed_out || e.is::<tokio_postgres::Error>() {
            true => e.context(format!("todo database {}", self.target())),
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
                }
            }
            (State::Open(client), _) if client.is_closed() => *state = State::Closed,
            _ => {}
        }
        out
    }

    async fn client<'a>(&self, state: &'a mut State) -> Result<&'a mut Client> {
        if let State::Failed { at, reason } = &*state {
            if self.config.is_none() || at.elapsed() < self.wait {
                return Err(anyhow!("{reason}"));
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
            let client = match tls {
                Some(tls) => {
                    let (client, connection) = config.connect(tls.clone()).await?;
                    tokio::spawn(connection);
                    client
                }
                None => {
                    let (client, connection) = config.connect(NoTls).await?;
                    tokio::spawn(connection);
                    client
                }
            };
            *state = State::Open(client);
        }
        match state {
            State::Open(client) => Ok(client),
            _ => unreachable!("connected above"),
        }
    }
}

impl Database {
    /// The TLS setup, built on a blocking thread the first time a connect
    /// needs it, so the caller's timeout covers reading the CA file.
    async fn tls(&self) -> Result<MakeRustlsConnect> {
        if let Some(tls) = self.tls.get() {
            return Ok(tls.clone());
        }
        let trust = self.trust.clone();
        let config = tokio::task::spawn_blocking(move || trust.client_config())
            .await
            .map_err(|e| anyhow!("{e}"))
            .and_then(|config| config)
            .context(TlsSetup)?;
        Ok(self
            .tls
            .get_or_init(|| MakeRustlsConnect::new(config))
            .clone())
    }
}

/// Marks an error in setting up TLS, such as a CA file that cannot be read:
/// a fault on this machine, not an unreachable database.
#[derive(Debug)]
struct TlsSetup;

impl fmt::Display for TlsSetup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("setting up TLS for the todo database")
    }
}

/// Whether `e` is an operation that got no answer from the server: it could
/// not connect, timed out, or lost the connection.
pub fn is_unreachable(e: &anyhow::Error) -> bool {
    e.downcast_ref::<TlsSetup>().is_none()
        && !e.chain().any(|cause| {
            cause
                .downcast_ref::<tokio_postgres::Error>()
                .is_some_and(|e| e.as_db_error().is_some())
        })
}

/// Whether `e` is the server reporting that devkit's schema or one of its
/// tables does not exist yet.
fn missing_schema(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        cause
            .downcast_ref::<tokio_postgres::Error>()
            .and_then(tokio_postgres::Error::code)
            .is_some_and(|code| {
                *code == SqlState::UNDEFINED_TABLE || *code == SqlState::INVALID_SCHEMA_NAME
            })
    })
}
