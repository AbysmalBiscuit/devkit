use std::{
    fmt,
    sync::{Arc, LazyLock, Mutex, OnceLock},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use devkit_common::tls::Trust;
use devkit_todo::{ORDER_GAP, SHORT_ID};
use tokio::runtime::Runtime;
use tokio_postgres::{Client, Config, config::SslMode, error::SqlState};
use tokio_postgres_rustls::MakeRustlsConnect;

/// The SQLSTATE a todo function raises when another holder has the todo in
/// progress, with that holder as the error's detail.
pub(crate) const CLAIMED: &str = "DK001";
/// The SQLSTATE a todo function raises for an id that names no todo, or
/// more than one.
pub(crate) const UNKNOWN_TODO: &str = "DK002";

/// Everything devkit keeps, in a schema of its own so it stays out of the
/// tables an application, or Supabase's HTTP API, exposes. It runs as one
/// transaction, and every statement is idempotent.
///
/// Each todo and activity write is one function, so a client that sends every
/// request as a transaction of its own, as Supabase's HTTP API does, claims as
/// atomically as one that holds a connection. A database runs this only
/// once something it needs is missing, so a function whose behaviour
/// changes takes a new name.
static SCHEMA: LazyLock<String> = LazyLock::new(|| {
    format!(
        "
-- Processes creating the schema at once would deadlock on its tables and
-- functions, so they take turns.
SELECT pg_advisory_xact_lock(hashtext('devkit schema'));
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

-- Whether `actor` may act on what `other` holds, as Holder::covers states.
CREATE OR REPLACE FUNCTION devkit.todo_covers(actor text, other text) RETURNS boolean
LANGUAGE sql IMMUTABLE AS $$
    SELECT actor = 'human' OR actor = other OR starts_with(other, actor || '/')
$$;

-- The status and holder a todo moves to when `actor` asks for `target`, as
-- devkit_todo::transition states it; a null status when nothing changes.
CREATE OR REPLACE FUNCTION devkit.todo_transition(
    status text, holder text, target text, actor text,
    OUT next_status text, OUT next_holder text
) LANGUAGE plpgsql IMMUTABLE AS $$
DECLARE
    claimant text;
BEGIN
    IF status = 'in_progress' THEN
        IF target = 'in_progress' AND devkit.todo_covers(actor, holder) THEN
            RETURN;
        END IF;
        IF target = 'in_progress' AND holder <> 'human'
            AND devkit.todo_covers(holder, actor) THEN
            next_status := target;
            next_holder := actor;
            RETURN;
        END IF;
        IF NOT devkit.todo_covers(actor, holder) THEN
            RAISE EXCEPTION 'todo is in progress by %', holder
                USING ERRCODE = '{CLAIMED}', DETAIL = holder;
        END IF;
        claimant := holder;
    ELSIF status = target THEN
        RETURN;
    END IF;
    next_status := target;
    next_holder := CASE target
        WHEN 'pending' THEN NULL
        WHEN 'in_progress' THEN actor
        ELSE coalesce(claimant, actor)
    END;
END
$$;

-- The one todo under `p_root` whose id starts with `p_prefix`.
CREATE OR REPLACE FUNCTION devkit.todo_find(p_root text, p_prefix text) RETURNS uuid
LANGUAGE plpgsql STABLE AS $$
DECLARE
    matches uuid[];
BEGIN
    IF length(p_prefix) BETWEEN {SHORT_ID} AND 36 AND p_prefix ~ '^[0-9A-Fa-f-]+$' THEN
        SELECT array_agg(id ORDER BY id::text) INTO matches FROM devkit.todos
        WHERE root = p_root AND starts_with(id::text, lower(p_prefix));
    END IF;
    IF matches IS NULL THEN
        RAISE EXCEPTION 'no todo %', p_prefix USING ERRCODE = '{UNKNOWN_TODO}';
    END IF;
    IF cardinality(matches) > 1 THEN
        RAISE EXCEPTION 'todo id % is ambiguous: %', lower(p_prefix),
            array_to_string(matches, ', ') USING ERRCODE = '{UNKNOWN_TODO}';
    END IF;
    RETURN matches[1];
END
$$;

-- The todo `p_id` names, its row locked for the rest of the transaction.
CREATE OR REPLACE FUNCTION devkit.todo_lock(p_root text, p_id text) RETURNS devkit.todos
LANGUAGE plpgsql AS $$
DECLARE
    target uuid := devkit.todo_find(p_root, p_id);
    locked devkit.todos;
BEGIN
    SELECT * INTO locked FROM devkit.todos WHERE id = target FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'no todo %', p_id USING ERRCODE = '{UNKNOWN_TODO}';
    END IF;
    RETURN locked;
END
$$;

-- The order that places a todo after the last of its siblings: the todos on
-- `p_node` under `p_parent`, other than `p_skip`.
CREATE OR REPLACE FUNCTION devkit.todo_after_last(
    p_root text, p_node text, p_parent uuid, p_skip uuid
) RETURNS bigint
LANGUAGE sql STABLE AS $$
    SELECT coalesce(max(ord), 0) + {ORDER_GAP} FROM devkit.todos
    WHERE root = p_root AND node IS NOT DISTINCT FROM p_node
      AND parent IS NOT DISTINCT FROM p_parent AND id IS DISTINCT FROM p_skip
$$;

CREATE OR REPLACE FUNCTION devkit.todo_add(
    p_root text, p_node text, p_description text, p_parent text, p_ord bigint
) RETURNS uuid
LANGUAGE plpgsql AS $$
DECLARE
    parent_id uuid;
    added uuid;
BEGIN
    IF p_parent IS NOT NULL THEN
        parent_id := devkit.todo_find(p_root, p_parent);
    END IF;
    INSERT INTO devkit.todos (root, node, description, status, parent, ord)
    VALUES (p_root, p_node, p_description, 'pending', parent_id,
            coalesce(p_ord, devkit.todo_after_last(p_root, p_node, parent_id, NULL)))
    RETURNING id INTO added;
    RETURN added;
END
$$;

-- Moves the todo to `p_to` as todo_transition rules, returning the change
-- made, if any. This, todo_release_all and todo_purge return each change as
-- the status and holder before and after, the latter null for a purge.
CREATE OR REPLACE FUNCTION devkit.todo_set_status(
    p_root text, p_id text, p_to text, p_actor text
) RETURNS TABLE (
    todo uuid, node text, from_status text, from_holder text,
    to_status text, to_holder text, at timestamptz
)
LANGUAGE plpgsql AS $$
#variable_conflict use_column
DECLARE
    locked devkit.todos := devkit.todo_lock(p_root, p_id);
    step record;
BEGIN
    SELECT * INTO step
    FROM devkit.todo_transition(locked.status, locked.holder, p_to, p_actor);
    IF step.next_status IS NULL THEN
        RETURN;
    END IF;
    UPDATE devkit.todos
    SET status = step.next_status, holder = step.next_holder, modified = clock_timestamp()
    WHERE id = locked.id RETURNING modified INTO at;
    todo := locked.id;
    node := locked.node;
    from_status := locked.status;
    from_holder := locked.holder;
    to_status := step.next_status;
    to_holder := step.next_holder;
    RETURN NEXT;
END
$$;

-- Moves the todo to in progress by `p_actor` while `p_from` still holds it,
-- as devkit_todo::take_over states it, and otherwise as todo_set_status
-- moves it for `p_actor` asking for in progress.
CREATE OR REPLACE FUNCTION devkit.todo_take_over(
    p_root text, p_id text, p_from text, p_actor text
) RETURNS TABLE (
    todo uuid, node text, from_status text, from_holder text,
    to_status text, to_holder text, at timestamptz
)
LANGUAGE plpgsql AS $$
#variable_conflict use_column
DECLARE
    locked devkit.todos := devkit.todo_lock(p_root, p_id);
BEGIN
    IF locked.status <> 'in_progress' OR locked.holder <> p_from OR p_from = 'human'
        OR devkit.todo_covers(p_actor, p_from) THEN
        RETURN QUERY SELECT * FROM devkit.todo_set_status(p_root, p_id, 'in_progress', p_actor);
        RETURN;
    END IF;
    UPDATE devkit.todos
    SET holder = p_actor, modified = clock_timestamp()
    WHERE id = locked.id RETURNING modified INTO at;
    todo := locked.id;
    node := locked.node;
    from_status := locked.status;
    from_holder := locked.holder;
    to_status := locked.status;
    to_holder := p_actor;
    RETURN NEXT;
END
$$;

CREATE OR REPLACE FUNCTION devkit.todo_describe(
    p_root text, p_id text, p_description text
) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    locked devkit.todos := devkit.todo_lock(p_root, p_id);
BEGIN
    UPDATE devkit.todos SET description = p_description, modified = clock_timestamp()
    WHERE id = locked.id;
END
$$;

-- Nests the todo under `p_parent`, or at the top level when null, at
-- `p_ord` among its new siblings, or after the last of them when null.
CREATE OR REPLACE FUNCTION devkit.todo_move(
    p_root text, p_id text, p_parent text, p_ord bigint
) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    locked devkit.todos := devkit.todo_lock(p_root, p_id);
    parent_id uuid;
BEGIN
    IF p_parent IS NOT NULL THEN
        parent_id := devkit.todo_find(p_root, p_parent);
    END IF;
    UPDATE devkit.todos
    SET parent = parent_id,
        ord = coalesce(p_ord, devkit.todo_after_last(p_root, locked.node, parent_id, locked.id)),
        modified = clock_timestamp()
    WHERE id = locked.id;
END
$$;

CREATE OR REPLACE FUNCTION devkit.todo_reorder(p_root text, p_id text, p_ord bigint)
RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    locked devkit.todos := devkit.todo_lock(p_root, p_id);
BEGIN
    UPDATE devkit.todos SET ord = p_ord, modified = clock_timestamp() WHERE id = locked.id;
END
$$;

-- Moves the todo to node `p_node`, null being the global list.
CREATE OR REPLACE FUNCTION devkit.todo_relocate(p_root text, p_id text, p_node text)
RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    locked devkit.todos := devkit.todo_lock(p_root, p_id);
BEGIN
    UPDATE devkit.todos SET node = p_node, modified = clock_timestamp() WHERE id = locked.id;
END
$$;

-- Every todo in progress by a holder `p_holder` covers goes back to pending.
-- A person releases nothing.
CREATE OR REPLACE FUNCTION devkit.todo_release_all(p_root text, p_holder text)
RETURNS TABLE (
    todo uuid, node text, from_status text, from_holder text,
    to_status text, to_holder text, at timestamptz
)
LANGUAGE sql AS $$
    WITH held AS (
        SELECT id, holder FROM devkit.todos
        WHERE root = p_root AND status = 'in_progress' AND p_holder <> 'human'
          AND devkit.todo_covers(p_holder, holder)
        FOR UPDATE
    )
    UPDATE devkit.todos AS t
    SET status = 'pending', holder = NULL, modified = clock_timestamp()
    FROM held WHERE t.id = held.id
    RETURNING t.id, t.node, 'in_progress'::text, held.holder, 'pending'::text, NULL::text,
        t.modified
$$;

-- Removes the todo's record for good.
CREATE OR REPLACE FUNCTION devkit.todo_purge(p_root text, p_id text)
RETURNS TABLE (
    todo uuid, node text, from_status text, from_holder text,
    to_status text, to_holder text, at timestamptz
)
LANGUAGE plpgsql AS $$
#variable_conflict use_column
DECLARE
    locked devkit.todos := devkit.todo_lock(p_root, p_id);
BEGIN
    DELETE FROM devkit.todos WHERE id = locked.id;
    RETURN QUERY SELECT locked.id, locked.node, locked.status, locked.holder,
        NULL::text, NULL::text, clock_timestamp();
END
$$;

-- Appends each of `p_events`, in order: activity log lines, each stamped by
-- its `at`, or by the database's clock when it has none.
CREATE OR REPLACE FUNCTION devkit.activity_record(p_root text, p_events jsonb)
RETURNS void
LANGUAGE sql AS $$
    INSERT INTO devkit.activity (root, at, event)
    SELECT p_root, coalesce((e.line ->> 'at')::timestamptz, clock_timestamp()), e.line - 'at'
    FROM jsonb_array_elements(p_events) WITH ORDINALITY AS e(line, n)
    ORDER BY e.n
$$;

-- Notes that `p_agent` of `p_session` fired a hook at `p_at`, or now by the
-- database's clock when null.
CREATE OR REPLACE FUNCTION devkit.activity_seen(
    p_root text, p_session text, p_agent text, p_at timestamptz
) RETURNS void
LANGUAGE sql AS $$
    INSERT INTO devkit.seen (root, session, agent, at)
    VALUES (p_root, p_session, p_agent, coalesce(p_at, clock_timestamp()))
    ON CONFLICT (root, session, agent) DO UPDATE SET at = excluded.at
$$;

-- Drops the last-hook marks of `p_agent` of `p_session`, or of every agent of
-- `p_session` when null.
CREATE OR REPLACE FUNCTION devkit.activity_forget(
    p_root text, p_session text, p_agent text
) RETURNS void
LANGUAGE sql AS $$
    DELETE FROM devkit.seen
    WHERE root = p_root AND session = p_session AND (p_agent IS NULL OR agent = p_agent)
$$;

-- Every activity log line and last-hook mark under `p_root`, read from one
-- snapshot, as of `p_now`, or when null as of the call, which comes before
-- the snapshot.
CREATE OR REPLACE FUNCTION devkit.activity_read(p_root text, p_now timestamptz)
RETURNS jsonb
LANGUAGE sql STABLE AS $$
    SELECT jsonb_build_object(
        'now', coalesce(p_now, statement_timestamp()),
        'events', coalesce((
            SELECT jsonb_agg(event || jsonb_build_object('at', at) ORDER BY at, id)
            FROM devkit.activity WHERE root = p_root
        ), '[]'::jsonb),
        'seen', coalesce((
            SELECT jsonb_agg(jsonb_build_object('session', session, 'agent', agent, 'at', at))
            FROM devkit.seen WHERE root = p_root
        ), '[]'::jsonb)
    )
$$;
"
    )
});

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
        let config =
            devkit_postgres::config(url).context("the todo database URL does not parse")?;
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
        match &self.config {
            Some(config) => devkit_postgres::target(config),
            None => "no database".to_string(),
        }
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
                    let created = client.batch_execute(&SCHEMA).await;
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
            *state = State::Open(devkit_postgres::connect(config, tls).await?);
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
        let tls = devkit_postgres::tls(&self.trust).await.context(TlsSetup)?;
        Ok(self.tls.get_or_init(|| tls).clone())
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

/// Whether `e` is the server reporting that devkit's schema, or one of its
/// tables or functions, does not exist yet.
fn missing_schema(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        cause
            .downcast_ref::<tokio_postgres::Error>()
            .and_then(tokio_postgres::Error::code)
            .is_some_and(|code| {
                [
                    SqlState::UNDEFINED_TABLE,
                    SqlState::UNDEFINED_FUNCTION,
                    SqlState::INVALID_SCHEMA_NAME,
                ]
                .contains(code)
            })
    })
}
