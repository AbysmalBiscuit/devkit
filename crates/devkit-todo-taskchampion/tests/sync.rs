use std::path::Path;

use devkit_common::forge::rest::stub::{self, Route, Stub};
use devkit_todo::{Edit, Filter, Holder, NewTodo, Status, StatusKind, TodoStore};
use devkit_todo_taskchampion::{SyncTarget, TaskchampionStore, Uuid};

fn replica(dir: &Path, name: &str) -> TaskchampionStore {
    TaskchampionStore::at(dir.join(name)).with_target(SyncTarget::Dir(dir.join("server")))
}

fn add(store: &TaskchampionStore, description: &str) -> String {
    store
        .add(NewTodo {
            project: Some("proj.main".into()),
            description: description.into(),
            parent: None,
            order: None,
        })
        .unwrap()
}

#[test]
fn two_replicas_meet_through_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (replica(dir.path(), "a"), replica(dir.path(), "b"));
    let id = add(&a, "ship it");
    a.sync_once().unwrap();
    assert!(b.list(&Filter::all()).unwrap().is_empty());
    b.sync_once().unwrap();
    let seen = b.get(&id).unwrap().expect("synced");
    assert_eq!(seen.description, "ship it");
    assert_eq!(seen.project.as_deref(), Some("proj.main"));
}

#[test]
fn claims_cross_replicas() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (replica(dir.path(), "a"), replica(dir.path(), "b"));
    let id = add(&a, "claimed");
    a.apply(&Edit::SetStatus {
        id: id.clone(),
        to: StatusKind::InProgress,
        actor: Holder::new("S"),
    })
    .unwrap();
    a.sync_once().unwrap();
    b.sync_once().unwrap();
    assert_eq!(b.get(&id).unwrap().unwrap().status, Status::InProgress {
        by: Holder::new("S")
    });
}

#[test]
fn a_replica_with_no_target_syncs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let local = TaskchampionStore::at(dir.path().join("a"));
    add(&local, "kept local");
    local.sync_once().unwrap();
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

/// A taskchampion sync server with no versions yet, accepting the first one
/// pushed to it.
fn sync_server() -> Stub {
    stub::serve(vec![
        Route::new("GET", "/v1/client/get-child-version/", 404, ""),
        Route::new("POST", "/v1/client/add-version/", 200, "")
            .header("X-Version-Id", &Uuid::new_v4().to_string()),
    ])
}

fn pushes(server: &Stub) -> usize {
    server
        .requests()
        .iter()
        .filter(|r| r.method == "POST" && r.path.starts_with("/v1/client/add-version/"))
        .count()
}

#[test]
fn a_replica_pushes_each_sync_to_a_sync_server() {
    let dir = tempfile::tempdir().unwrap();
    let server = sync_server();
    let store = TaskchampionStore::at(dir.path().join("a")).with_target(SyncTarget::Server {
        url: server.url(),
        client_id: Uuid::new_v4(),
        secret: b"secret".to_vec(),
    });
    add(&store, "pushed");
    store.sync_once().unwrap();
    add(&store, "pushed by a second sync");
    store.sync_once().unwrap();
    assert_eq!(pushes(&server), 2, "{:?}", server.requests());
}
