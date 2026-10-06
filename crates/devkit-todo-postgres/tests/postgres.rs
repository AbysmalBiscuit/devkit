//! The Postgres store against a real database: `DEVKIT_TEST_POSTGRES_URL`
//! names one reached directly, and `DEVKIT_TEST_POOLER_URL` the same database
//! behind a transaction-mode pooler that keeps no prepared statement.
//! `testdb/up.sh` starts both. A test whose variable is unset returns early.

use std::{
    sync::{
        Barrier,
        atomic::{AtomicU32, Ordering},
    },
    thread,
    time::Duration,
};

use devkit_todo::{
    Claimed, Edit, Filter, Holder, NewTodo, Status, StatusChange, StatusKind, TodoStore, transition,
};
use devkit_todo_postgres::{Database, PostgresStore, Trust};
use tokio_postgres::types::Type;

const DIRECT: &str = "DEVKIT_TEST_POSTGRES_URL";
const POOLER: &str = "DEVKIT_TEST_POOLER_URL";

fn url(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|url| !url.trim().is_empty())
}

/// A root no other test shares, so each store starts empty.
fn fresh_root() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(
        "test-{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn store(var: &str) -> Option<((), PostgresStore)> {
    let db = Database::new(&url(var)?, Duration::from_secs(10), &Trust::default()).unwrap();
    Some(((), PostgresStore::new(db, fresh_root())))
}

mod direct {
    devkit_todo::contract_tests!(
        skip_unless || super::store(super::DIRECT),
        activity = |s: &devkit_todo_postgres::PostgresStore| s.activity()
    );
}

mod pooled {
    devkit_todo::contract_tests!(
        skip_unless || super::store(super::POOLER),
        activity = |s: &devkit_todo_postgres::PostgresStore| s.activity()
    );
}

/// The pooled run proves something only if the pooler keeps no prepared
/// statement between transactions, as Supabase's transaction mode does.
#[test]
fn the_pooler_keeps_no_prepared_statement() {
    let Some(url) = url(POOLER) else {
        return;
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(connection);
        let statement = client.prepare("SELECT 1").await.unwrap();
        let err = client.query(&statement, &[]).await.unwrap_err();
        let err = err.as_db_error().map(|e| e.message().to_string());
        assert!(
            err.as_deref().is_some_and(|e| e.contains("does not exist")),
            "{err:?}"
        );
    });
}

/// Runs `sql` on the database `url` names, outside any transaction.
fn admin(url: &str, sql: &str) {
    connected(url, async |client| client.batch_execute(sql).await.unwrap());
}

#[test]
fn a_role_that_cannot_create_the_schema_is_told_why() {
    let Some(direct) = url(DIRECT) else {
        return;
    };
    let name = fresh_root().replace('-', "_");
    admin(&direct, &format!("CREATE DATABASE {name}"));
    admin(&direct, &format!("CREATE ROLE {name} LOGIN PASSWORD 'pw'"));
    let (_, host_and_db) = direct.rsplit_once('@').unwrap();
    let (host, _) = host_and_db.rsplit_once('/').unwrap();
    let limited = format!("postgres://{name}:pw@{host}/{name}?sslmode=disable");
    let db = Database::new(&limited, Duration::from_secs(10), &Trust::default()).unwrap();
    let err = PostgresStore::new(db, "r")
        .list(&devkit_todo::Filter::all())
        .unwrap_err();
    let shown = format!("{err:#}");
    assert!(shown.contains("permission denied"), "{shown}");
    admin(&direct, &format!("DROP DATABASE {name} WITH (FORCE)"));
    admin(&direct, &format!("DROP ROLE {name}"));
}

const TLS: &str = "DEVKIT_TEST_POSTGRES_TLS_URL";
const CA: &str = "DEVKIT_TEST_POSTGRES_CA";

#[test]
fn a_certificate_no_trusted_root_vouches_for_is_refused() {
    let Some(tls) = url(TLS) else {
        return;
    };
    let db = Database::new(&tls, Duration::from_secs(10), &Trust::default()).unwrap();
    let err = format!("{:#}", db.check().unwrap_err());
    assert!(err.contains("certificate"), "{err}");
}

#[test]
fn a_ca_file_lets_the_certificate_verify() {
    let (Some(tls), Some(ca)) = (url(TLS), url(CA)) else {
        return;
    };
    let trust = Trust {
        ca_file: Some(ca.into()),
    };
    let db = Database::new(
        &format!("{tls}?sslmode=require"),
        Duration::from_secs(10),
        &trust,
    )
    .unwrap();
    db.check().unwrap();
}

#[test]
fn a_ca_file_that_cannot_be_read_is_named() {
    let trust = Trust {
        ca_file: Some("/nonexistent/ca.crt".into()),
    };
    let db = Database::new("postgres://127.0.0.1:1/db", Duration::from_secs(1), &trust).unwrap();
    let err = db.check().unwrap_err();
    assert!(
        format!("{err:#}").contains("/nonexistent/ca.crt"),
        "{err:#}"
    );
    assert!(!devkit_todo_postgres::is_unreachable(&err), "{err:#}");
}

/// `url` without its query, so its `sslmode` falls back to the default.
fn without_query(url: &str) -> &str {
    url.split_once('?').map_or(url, |(base, _)| base)
}

#[test]
fn a_server_without_tls_is_refused_unless_the_url_disables_it() {
    let Some(direct) = url(DIRECT) else {
        return;
    };
    let base = without_query(&direct);
    for refused in [base.to_string(), format!("{base}?sslmode=prefer")] {
        let db = Database::new(&refused, Duration::from_secs(10), &Trust::default()).unwrap();
        let err = format!("{:#}", db.check().unwrap_err());
        assert!(err.contains("TLS"), "{refused}: {err}");
    }
    let plain = format!("{base}?sslmode=disable");
    Database::new(&plain, Duration::from_secs(10), &Trust::default())
        .unwrap()
        .check()
        .unwrap();
}

#[test]
fn a_plaintext_url_ignores_the_ca_file() {
    let Some(direct) = url(DIRECT) else {
        return;
    };
    let plain = format!("{}?sslmode=disable", without_query(&direct));
    let trust = Trust {
        ca_file: Some("/nonexistent/ca.crt".into()),
    };
    Database::new(&plain, Duration::from_secs(10), &trust)
        .unwrap()
        .check()
        .unwrap();
}

/// Runs `work` on a connection of its own to the database `url` names.
fn connected<T>(url: &str, work: impl AsyncFnOnce(&tokio_postgres::Client) -> T) -> T {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(connection);
        work(&client).await
    })
}

/// A database of its own on the server `direct` names, dropped with it.
struct Scratch {
    direct: String,
    name: String,
    url: String,
}

impl Scratch {
    fn new(direct: &str) -> Self {
        let name = fresh_root().replace('-', "_");
        admin(direct, &format!("CREATE DATABASE {name}"));
        let (base, query) = direct.split_once('?').unwrap_or((direct, ""));
        let (server, _) = base.rsplit_once('/').unwrap();
        Self {
            direct: direct.to_string(),
            url: format!("{server}/{name}?{query}"),
            name,
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        admin(
            &self.direct,
            &format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.name),
        );
    }
}

fn open(url: &str, root: &str) -> PostgresStore {
    let db = Database::new(url, Duration::from_secs(30), &Trust::default()).unwrap();
    PostgresStore::new(db, root)
}

fn new_todo(node: Option<&str>, description: &str) -> NewTodo {
    NewTodo {
        project: node.map(str::to_string),
        description: description.to_string(),
        parent: None,
        order: None,
    }
}

fn claim(id: &str, actor: &str) -> Edit {
    Edit::SetStatus {
        id: id.to_string(),
        to: StatusKind::InProgress,
        actor: Holder::new(actor),
    }
}

fn claimant(err: &anyhow::Error) -> Option<String> {
    err.chain()
        .find_map(|e| e.downcast_ref::<Claimed>())
        .map(|claimed| claimed.by.to_string())
}

/// However many agents claim one todo at once, each on a connection of its
/// own, exactly one gets it and every other is refused naming that one.
#[test]
fn of_agents_claiming_one_todo_at_once_exactly_one_gets_it() {
    const AGENTS: usize = 8;
    for var in [DIRECT, POOLER] {
        let Some(url) = url(var) else {
            continue;
        };
        let root = fresh_root();
        let id = open(&url, &root).add(new_todo(None, "contested")).unwrap();
        let start = Barrier::new(AGENTS);
        let outcomes: Vec<(String, anyhow::Result<Vec<StatusChange>>)> = thread::scope(|s| {
            let agents: Vec<_> = (0..AGENTS)
                .map(|i| {
                    let (store, start, id) = (open(&url, &root), &start, &id);
                    s.spawn(move || {
                        // Connected before the barrier, so the claims are
                        // what race.
                        store.get(id).unwrap();
                        start.wait();
                        let actor = format!("S{i}");
                        let out = store.apply(&claim(id, &actor));
                        (actor, out)
                    })
                })
                .collect();
            agents.into_iter().map(|a| a.join().unwrap()).collect()
        });
        let winners: Vec<&str> = outcomes
            .iter()
            .filter(|(_, out)| out.is_ok())
            .map(|(actor, _)| actor.as_str())
            .collect();
        assert_eq!(winners.len(), 1, "{var}: {outcomes:?}");
        let winner = winners[0];
        for (actor, out) in &outcomes {
            match out {
                Ok(changes) => assert_eq!(changes.len(), 1, "{var}: {changes:?}"),
                Err(e) => assert_eq!(claimant(e).as_deref(), Some(winner), "{var} {actor}: {e:#}"),
            }
        }
        assert_eq!(
            open(&url, &root).get(&id).unwrap().unwrap().status,
            Status::InProgress {
                by: Holder::new(winner)
            }
        );
    }
}

fn kind_name(kind: StatusKind) -> &'static str {
    match kind {
        StatusKind::Pending => "pending",
        StatusKind::InProgress => "in_progress",
        StatusKind::Completed => "completed",
        StatusKind::Cancelled => "cancelled",
    }
}

fn stored(status: &Status) -> (String, Option<String>) {
    let by = match status {
        Status::Pending => None,
        Status::InProgress { by } => Some(by.to_string()),
        Status::Completed { by } | Status::Cancelled { by } => by.as_ref().map(Holder::to_string),
    };
    (kind_name(status.kind()).to_string(), by)
}

/// The claim rule the database applies is the one `transition` states, for
/// every status, target and actor among a session, its sub-agents, another
/// session and a person.
#[test]
fn the_databases_claim_rule_is_transition() {
    let Some(direct) = url(DIRECT) else {
        return;
    };
    open(&direct, &fresh_root())
        .add(new_todo(None, "x"))
        .unwrap();
    let holders = ["S", "S/a1", "S/a2", "S2", "human"].map(Holder::new);
    let mut statuses = vec![
        Status::Pending,
        Status::Completed { by: None },
        Status::Cancelled { by: None },
    ];
    for by in &holders {
        statuses.push(Status::InProgress { by: by.clone() });
        statuses.push(Status::Completed {
            by: Some(by.clone()),
        });
        statuses.push(Status::Cancelled {
            by: Some(by.clone()),
        });
    }
    let kinds = [
        StatusKind::Pending,
        StatusKind::InProgress,
        StatusKind::Completed,
        StatusKind::Cancelled,
    ];
    connected(&direct, async |client| {
        for from in &statuses {
            let (status, holder) = stored(from);
            for to in kinds {
                for actor in &holders {
                    let actor_text: &str = actor;
                    let asked = client
                        .query_typed_one(
                            "SELECT next_status, next_holder
                             FROM devkit.todo_transition($1, $2, $3, $4)",
                            &[
                                (&status, Type::TEXT),
                                (&holder, Type::TEXT),
                                (&kind_name(to), Type::TEXT),
                                (&actor_text, Type::TEXT),
                            ],
                        )
                        .await;
                    let database = match asked {
                        Ok(row) => Ok(row
                            .get::<_, Option<String>>(0)
                            .map(|status| (status, row.get::<_, Option<String>>(1)))),
                        Err(e) => Err(e
                            .as_db_error()
                            .filter(|e| e.code().code() == "DK001")
                            .and_then(|e| e.detail())
                            .unwrap_or_else(|| panic!("{from:?} -> {to:?} by {actor}: {e}"))
                            .to_string()),
                    };
                    let rule = transition(from, to, actor)
                        .map(|next| next.as_ref().map(stored))
                        .map_err(|claimed| claimed.by.to_string());
                    assert_eq!(database, rule, "{from:?} -> {to:?} by {actor}");
                }
            }
        }
    });
}

/// The schema devkit created before its writes ran as functions.
const EARLIER_SCHEMA: &str = "
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

/// A database an earlier devkit created gains the functions the first time
/// several processes write to it at once, and keeps the todos it had.
#[test]
fn a_database_from_before_the_functions_gains_them_and_keeps_its_todos() {
    const PROCESSES: usize = 4;
    let Some(direct) = url(DIRECT) else {
        return;
    };
    let scratch = Scratch::new(&direct);
    admin(&scratch.url, EARLIER_SCHEMA);
    let kept = "0123abcd-0000-4000-8000-000000000001";
    admin(
        &scratch.url,
        &format!(
            "INSERT INTO devkit.todos (id, root, node, description, status, ord)
             VALUES ('{kept}', 'r', 'n', 'kept', 'pending', 1024)"
        ),
    );
    let start = Barrier::new(PROCESSES);
    thread::scope(|s| {
        let writers: Vec<_> = (0..PROCESSES)
            .map(|i| {
                let (store, start) = (open(&scratch.url, "r"), &start);
                s.spawn(move || {
                    store.get(kept).unwrap();
                    start.wait();
                    store.add(new_todo(Some("n"), &format!("added {i}")))
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap().unwrap();
        }
    });
    let store = open(&scratch.url, "r");
    store.apply(&claim(kept, "S")).unwrap();
    let todo = store.get(kept).unwrap().unwrap();
    assert_eq!(todo.description, "kept");
    assert_eq!(todo.project.as_deref(), Some("n"));
    assert_eq!(todo.status, Status::InProgress {
        by: Holder::new("S")
    });
    assert_eq!(
        store.list(&Filter::all()).unwrap().len(),
        PROCESSES + 1,
        "every todo, old and new"
    );
    let functions: Vec<String> = connected(&scratch.url, async |client| {
        client
            .query(
                "SELECT proname::text FROM pg_proc
                 WHERE pronamespace = 'devkit'::regnamespace ORDER BY 1",
                &[],
            )
            .await
            .unwrap()
            .iter()
            .map(|row| row.get(0))
            .collect()
    });
    for name in ["todo_add", "todo_set_status", "todo_release_all"] {
        assert!(functions.iter().any(|f| f == name), "{name}: {functions:?}");
    }
}
