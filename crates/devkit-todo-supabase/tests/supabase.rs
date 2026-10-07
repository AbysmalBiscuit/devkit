//! The Supabase store against PostgREST serving a real database:
//! `DEVKIT_TEST_SUPABASE_URL` names the API, behind `/rest/v1/` as on
//! Supabase, and `DEVKIT_TEST_POSTGRES_URL` the database it serves.
//! `crates/devkit-todo-postgres/testdb/up.sh` starts both. A test returns
//! early when either is unset.

use std::{
    sync::{
        Barrier, OnceLock,
        atomic::{AtomicU32, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use devkit_todo::{Claimed, Edit, Filter, Holder, NewTodo, Status, StatusKind, TodoStore};
use devkit_todo_supabase::{Api, SupabaseStore};

const API: &str = "DEVKIT_TEST_SUPABASE_URL";
const DIRECT: &str = "DEVKIT_TEST_POSTGRES_URL";

/// The role and grants the todo reference sets up, for the role PostgREST
/// serves a request that carries no key as. PostgREST here logs in as a
/// superuser, so the grant to Supabase's `authenticator` is left out.
const GRANTS: &str = "
DO $$ BEGIN
    CREATE ROLE devkit_agent NOLOGIN;
EXCEPTION WHEN duplicate_object THEN NULL;
END $$;
GRANT USAGE ON SCHEMA devkit TO devkit_agent;
GRANT SELECT, INSERT, UPDATE, DELETE ON devkit.todos TO devkit_agent;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA devkit TO devkit_agent;
ALTER DEFAULT PRIVILEGES IN SCHEMA devkit GRANT EXECUTE ON FUNCTIONS TO devkit_agent;
NOTIFY pgrst, 'reload schema';
";

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// The API's URL once the database holds devkit's schema, the role can
/// reach it, and PostgREST serves it; `None` without a test database.
fn api_url() -> Option<String> {
    static READY: OnceLock<Option<String>> = OnceLock::new();
    READY
        .get_or_init(|| {
            let (api, direct) = (var(API)?, var(DIRECT)?);
            prepare(&direct);
            let probe = Api::new(&api, None, Duration::from_secs(5)).unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            while let Err(e) = probe.check() {
                assert!(Instant::now() < deadline, "PostgREST never served: {e:#}");
                thread::sleep(Duration::from_millis(100));
            }
            Some(api)
        })
        .clone()
}

/// Creates devkit's schema through the postgres backend, then the role and
/// its grants.
fn prepare(direct: &str) {
    let db = devkit_todo_postgres::Database::new(
        direct,
        Duration::from_secs(10),
        &devkit_todo_postgres::Trust::default(),
    )
    .unwrap();
    devkit_todo_postgres::PostgresStore::new(db, "schema")
        .list(&Filter::all())
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (client, connection) = tokio_postgres::connect(direct, tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(connection);
        client.batch_execute(GRANTS).await.unwrap();
    });
}

/// A root no other test shares, so each store starts empty.
fn fresh_root() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(
        "sb-{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn open(url: &str, root: &str) -> SupabaseStore {
    SupabaseStore::new(
        Api::new(url, None, Duration::from_secs(10)).unwrap().into(),
        root,
    )
}

fn store() -> Option<((), SupabaseStore)> {
    Some(((), open(&api_url()?, &fresh_root())))
}

mod contract {
    devkit_todo::contract_tests!(skip_unless super::store);
}

fn claim(id: &str, actor: &str) -> Edit {
    Edit::SetStatus {
        id: id.to_string(),
        to: StatusKind::InProgress,
        actor: Holder::new(actor),
    }
}

/// However many agents claim one todo at once over the API, exactly one gets
/// it and every other is refused naming that one.
#[test]
fn of_agents_claiming_one_todo_at_once_exactly_one_gets_it() {
    const AGENTS: usize = 8;
    let Some(url) = api_url() else {
        return;
    };
    let root = fresh_root();
    let id = open(&url, &root)
        .add(NewTodo {
            project: None,
            description: "contested".into(),
            parent: None,
            order: None,
        })
        .unwrap();
    let start = Barrier::new(AGENTS);
    let outcomes: Vec<(String, anyhow::Result<_>)> = thread::scope(|s| {
        let agents: Vec<_> = (0..AGENTS)
            .map(|i| {
                let (store, start, id) = (open(&url, &root), &start, &id);
                s.spawn(move || {
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
    assert_eq!(winners.len(), 1, "{outcomes:?}");
    for (actor, out) in &outcomes {
        if let Err(e) = out {
            let by = e.chain().find_map(|e| e.downcast_ref::<Claimed>());
            assert_eq!(
                by.map(|c| c.by.to_string()).as_deref(),
                Some(winners[0]),
                "{actor}: {e:#}"
            );
        }
    }
    assert_eq!(
        open(&url, &root).get(&id).unwrap().unwrap().status,
        Status::InProgress {
            by: Holder::new(winners[0])
        }
    );
}

/// A todo added over the API is the same todo a direct connection reads, so
/// machines on either backend share one list.
#[test]
fn the_api_and_a_direct_connection_share_the_todos() {
    let (Some(url), Some(direct)) = (api_url(), var(DIRECT)) else {
        return;
    };
    let root = fresh_root();
    let id = open(&url, &root)
        .add(NewTodo {
            project: Some("proj".into()),
            description: "shared".into(),
            parent: None,
            order: None,
        })
        .unwrap();
    let db = devkit_todo_postgres::Database::new(
        &direct,
        Duration::from_secs(10),
        &devkit_todo_postgres::Trust::default(),
    )
    .unwrap();
    let pg = devkit_todo_postgres::PostgresStore::new(db, &root);
    pg.apply(&claim(&id, "S")).unwrap();
    assert_eq!(
        open(&url, &root).get(&id[..8]).unwrap().unwrap().status,
        Status::InProgress {
            by: Holder::new("S")
        }
    );
}
