use std::{path::Path, time::Duration};

use devkit_todo::{Filter, NewTodo, TodoStore};
use devkit_todo_taskchampion::{SyncTarget, TaskchampionStore};
use taskchampion::{Operations, Replica, SqliteStorage, Status, Uuid, storage::AccessMode};

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

/// Runs `work` against the replica at `data_dir`, opened directly.
fn with_replica<T>(data_dir: &Path, work: impl AsyncFnOnce(&mut Replica<SqliteStorage>) -> T) -> T {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let storage = SqliteStorage::new(data_dir, AccessMode::ReadWrite, false)
                .await
                .unwrap();
            work(&mut Replica::new(storage)).await
        })
}

/// The advisory lock at a path, held by another thread until this drops.
struct HeldLock {
    release: Option<std::sync::mpsc::Sender<()>>,
    holder: Option<std::thread::JoinHandle<()>>,
}

impl HeldLock {
    fn at(path: std::path::PathBuf) -> Self {
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            devkit_common::store::with_file_lock(&path, || {
                held_tx.send(()).unwrap();
                let _ = release_rx.recv();
                Ok(())
            })
            .unwrap();
        });
        held_rx.recv().unwrap();
        Self {
            release: Some(release_tx),
            holder: Some(holder),
        }
    }
}

impl Drop for HeldLock {
    fn drop(&mut self) {
        drop(self.release.take());
        if let Some(holder) = self.holder.take() {
            let _ = holder.join();
        }
    }
}

#[test]
fn global_todos_are_filed_on_the_root() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("tc");
    let store = TaskchampionStore::at(data.clone());
    let global = store.add(new(None, "g")).unwrap();
    let filed = store.add(new(Some("r.main"), "f")).unwrap();
    let project = |id: &str| {
        with_replica(&data, async |raw| {
            raw.get_task(Uuid::parse_str(id).unwrap())
                .await
                .unwrap()
                .unwrap()
                .get_value("project")
                .map(str::to_string)
        })
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
    with_replica(&data, async |raw| {
        let mut ops = Operations::new();
        for project in [None, Some("other"), Some("devkitx.r")] {
            let mut task = raw.create_task(Uuid::new_v4(), &mut ops).await.unwrap();
            task.set_description("a person's own".into(), &mut ops)
                .unwrap();
            task.set_status(Status::Pending, &mut ops).unwrap();
            task.set_value("project", project.map(str::to_string), &mut ops)
                .unwrap();
        }
        raw.commit_operations(ops).await.unwrap();
    });
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
    let _held = HeldLock::at(data.join("devkit.lock"));
    let store = TaskchampionStore::at(data).with_lock_wait(Duration::from_millis(200));
    let started = std::time::Instant::now();
    let err = store.add(new(Some("r.main"), "a")).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(format!("{err:#}").contains("todo store busy"), "{err:#}");
    assert!(
        err.downcast_ref::<devkit_common::store::LockBusy>()
            .is_some()
    );
}

#[test]
fn reads_never_wait_for_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("tc");
    let writer = TaskchampionStore::at(data.clone());
    let id = writer.add(new(Some("r.main"), "a")).unwrap();
    let _held = HeldLock::at(data.join("devkit.lock"));
    let reader = TaskchampionStore::at(data).with_lock_wait(Duration::from_millis(200));
    assert_eq!(reader.list(&Filter::all()).unwrap().len(), 1);
    assert!(reader.get(&id).unwrap().is_some());
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
