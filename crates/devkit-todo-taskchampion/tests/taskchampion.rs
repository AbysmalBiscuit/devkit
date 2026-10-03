use std::{path::Path, time::Duration};

use devkit_todo::{Filter, NewTodo, TodoStore};
use devkit_todo_taskchampion::{SyncTarget, TaskchampionStore};
use taskchampion::{Operations, Replica, Status, StorageConfig, Uuid, storage::AccessMode};

devkit_todo::contract_tests!(|| {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskchampionStore::at(dir.path().join("tc"));
    (dir, store)
});

fn new(project: Option<&str>, description: &str) -> NewTodo {
    NewTodo {
        project: project.map(Into::into),
        description: description.into(),
        parent: None,
        order: None,
    }
}

fn replica(data_dir: &Path) -> Replica {
    Replica::new(
        StorageConfig::OnDisk {
            taskdb_dir: data_dir.to_path_buf(),
            create_if_missing: false,
            access_mode: AccessMode::ReadWrite,
        }
        .into_storage()
        .unwrap(),
    )
}

#[test]
fn global_todos_are_filed_on_the_root() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("tc");
    let store = TaskchampionStore::at(data.clone());
    let global = store.add(new(None, "g")).unwrap();
    let filed = store.add(new(Some("r.main"), "f")).unwrap();
    let mut raw = replica(&data);
    let mut project = |id: &str| {
        raw.get_task(Uuid::parse_str(id).unwrap())
            .unwrap()
            .unwrap()
            .get_value("project")
            .map(str::to_string)
    };
    assert_eq!(project(&global).as_deref(), Some("devkit"));
    assert_eq!(project(&filed).as_deref(), Some("devkit.r.main"));
}

#[test]
fn a_task_outside_the_root_never_lists() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("tc");
    let store = TaskchampionStore::at(data.clone());
    store.add(new(None, "make the replica")).unwrap();
    let mut raw = replica(&data);
    let mut ops = Operations::new();
    for project in [None, Some("other"), Some("devkitx.r")] {
        let mut task = raw.create_task(Uuid::new_v4(), &mut ops).unwrap();
        task.set_description("a person's own".into(), &mut ops)
            .unwrap();
        task.set_status(Status::Pending, &mut ops).unwrap();
        task.set_value("project", project.map(str::to_string), &mut ops)
            .unwrap();
    }
    raw.commit_operations(ops).unwrap();
    let listed = store.list(&Filter::all()).unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].description, "make the replica");
}

#[test]
fn entry_and_modified_are_rfc3339() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskchampionStore::at(dir.path().join("tc"));
    let id = store.add(new(Some("r.main"), "a")).unwrap();
    let todo = store.get(&id).unwrap().unwrap();
    for stamp in [todo.entry, todo.modified] {
        let stamp = stamp.expect("stamped");
        chrono::DateTime::parse_from_rfc3339(&stamp).unwrap();
    }
}

#[test]
fn ids_resolve_by_unique_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskchampionStore::at(dir.path().join("tc"));
    let id = store.add(new(Some("r.main"), "a")).unwrap();
    assert_eq!(id.len(), 36);
    let found = store.get(devkit_todo::short_id(&id)).unwrap().unwrap();
    assert_eq!(found.id, id);
    assert_eq!(store.get(&id[..7]).unwrap(), None);
}

#[test]
fn a_held_lock_fails_after_the_wait() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("tc");
    std::fs::create_dir_all(&data).unwrap();
    let lock = data.join("devkit.lock");
    let (held, release) = (
        std::sync::mpsc::channel::<()>(),
        std::sync::mpsc::channel::<()>(),
    );
    let holder = {
        let lock = lock.clone();
        let (held_tx, release_rx) = (held.0, release.1);
        std::thread::spawn(move || {
            devkit_common::store::with_file_lock(&lock, || {
                held_tx.send(()).unwrap();
                release_rx.recv().ok();
                Ok(())
            })
            .unwrap();
        })
    };
    held.1.recv().unwrap();
    let store = TaskchampionStore::at(data).with_lock_wait(Duration::from_millis(200));
    let started = std::time::Instant::now();
    let err = store.add(new(Some("r.main"), "a")).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(format!("{err:#}").contains("todo store busy"), "{err:#}");
    assert!(
        err.downcast_ref::<devkit_common::store::LockBusy>()
            .is_some()
    );
    release.0.send(()).unwrap();
    holder.join().unwrap();
}

#[test]
fn reads_never_wait_for_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("tc");
    let writer = TaskchampionStore::at(data.clone());
    let id = writer.add(new(Some("r.main"), "a")).unwrap();
    let lock = data.join("devkit.lock");
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        devkit_common::store::with_file_lock(&lock, || {
            held_tx.send(()).unwrap();
            release_rx.recv().ok();
            Ok(())
        })
        .unwrap();
    });
    held_rx.recv().unwrap();
    let reader = TaskchampionStore::at(data).with_lock_wait(Duration::from_millis(200));
    assert_eq!(reader.list(&Filter::all()).unwrap().len(), 1);
    assert!(reader.get(&id).unwrap().is_some());
    release_tx.send(()).unwrap();
    holder.join().unwrap();
}

#[test]
fn a_replica_not_created_yet_reads_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("tc");
    let store = TaskchampionStore::at(data.clone());
    assert!(store.list(&Filter::all()).unwrap().is_empty());
    assert!(!data.exists());
}

#[cfg(unix)]
#[test]
fn an_uncreatable_data_dir_names_the_path() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let locked = dir.path().join("ro");
    std::fs::create_dir(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
    let data = locked.join("tc");
    let err = TaskchampionStore::at(data.clone())
        .add(new(None, "a"))
        .unwrap_err();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        format!("{err:#}").contains(&data.display().to_string()),
        "{err:#}"
    );
}

#[test]
fn secret_is_redacted_in_debug() {
    let target = SyncTarget::Server {
        url: "https://sync.example".into(),
        client_id: Uuid::new_v4(),
        secret: b"hunter2-secret".to_vec(),
    };
    let shown = format!("{target:?}");
    assert!(shown.contains("https://sync.example"), "{shown}");
    assert!(!shown.contains("hunter2"), "{shown}");
    assert!(!shown.contains("104, 117"), "{shown}");
}
