//! The Supabase store against PostgREST serving a real database, as
//! [`testapi`] sets it up. A test returns early without one.

#[path = "common/testapi.rs"]
mod testapi;

use std::{
    sync::{
        Barrier,
        atomic::{AtomicU32, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use devkit_todo::{Claimed, Edit, Holder, NewTodo, Status, StatusKind, TodoStore};
use devkit_todo_supabase::{Api, SupabaseStore};
use testapi::api_url;

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
    devkit_todo::contract_tests!(
        skip_unless super::store,
        activity = |s: &devkit_todo_supabase::SupabaseStore| s.activity()
    );
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

/// A database that has not gained the blocking column still lists, and every
/// todo reads as having no edges. Dropping the column from the shared table
/// breaks every other todo test against it, so `.config/nextest.toml` runs
/// this test alone.
#[test]
fn reads_without_the_blocking_column_list_todos_without_edges() {
    let (Some(url), Some(direct)) = (api_url(), testapi::direct_url()) else {
        return;
    };
    let root = fresh_root();
    let id = open(&url, &root)
        .add(NewTodo {
            project: Some("proj".into()),
            description: "before blocking".into(),
            parent: None,
            order: None,
        })
        .unwrap();
    let _restore = RestoresBlockingColumn {
        url: &url,
        direct: &direct,
        root: &root,
    };
    execute(
        &direct,
        "ALTER TABLE devkit.todos DROP COLUMN blocked_by;
         NOTIFY pgrst, 'reload schema'",
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let todo = loop {
        match open(&url, &root).get(&id) {
            Ok(Some(todo)) => break todo,
            Ok(None) => panic!("the todo disappeared"),
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(100)),
            Err(e) => panic!("PostgREST never served the dropped column: {e:#}"),
        }
    };
    assert!(todo.blocked_by.is_empty(), "{todo:?}");
    assert_eq!(todo.description, "before blocking");
}

/// Puts the blocking column back when dropped, a failed assertion included,
/// and waits until PostgREST accepts an edge again.
struct RestoresBlockingColumn<'a> {
    url: &'a str,
    direct: &'a str,
    root: &'a str,
}

impl Drop for RestoresBlockingColumn<'_> {
    fn drop(&mut self) {
        if let Err(e) = execute(
            self.direct,
            "ALTER TABLE devkit.todos
                 ADD COLUMN IF NOT EXISTS blocked_by uuid[] NOT NULL DEFAULT '{}';
             NOTIFY pgrst, 'reload schema'",
        ) {
            eprintln!("restoring devkit.todos.blocked_by: {e:#}");
            return;
        }
        let store = open(self.url, self.root);
        let new = |description: &str| NewTodo {
            project: Some("proj".into()),
            description: description.into(),
            parent: None,
            order: None,
        };
        let (Ok(blocker), Ok(blocked)) = (store.add(new("blocker")), store.add(new("blocked")))
        else {
            eprintln!("adding the todos that probe the restored column failed");
            return;
        };
        let block = Edit::Block {
            id: blocked,
            by: vec![blocker],
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while let Err(e) = store.apply(&block) {
            if Instant::now() >= deadline {
                eprintln!("PostgREST never served the restored column: {e:#}");
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

fn execute(direct: &str, sql: &str) -> anyhow::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let (client, connection) =
                tokio_postgres::connect(direct, tokio_postgres::NoTls).await?;
            tokio::spawn(connection);
            client.batch_execute(sql).await?;
            Ok(())
        })
}

/// A todo added over the API is the same todo a direct connection reads, so
/// machines on either backend share one list.
#[test]
fn the_api_and_a_direct_connection_share_the_todos() {
    let (Some(url), Some(direct)) = (api_url(), testapi::direct_url()) else {
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
