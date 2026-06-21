use devkit_locks::store::{MemoryStore, acquire_with};
use devkit_locks::model::Data;
use std::sync::{Arc, Mutex};

fn tmp(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("devkit-lockmem-{}-{}", std::process::id(), tag));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn commit_writes_through_then_updates_memory() {
    let dir = tmp("ok");
    let state = Arc::new(Mutex::new(Data::default()));
    let store = MemoryStore::new(state.clone(), dir.join("locks.json"));
    acquire_with(&store, "/repo", "alice", &["scenes".into()], None, None, 1800, 100).unwrap();
    assert_eq!(state.lock().unwrap().locks.len(), 1, "memory updated");
    let on_disk: Data = devkit_common::store::load(&dir.join("locks.json"));
    assert_eq!(on_disk.locks.len(), 1, "file written through");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn commit_failure_leaves_memory_unchanged() {
    let dir = tmp("fail");
    let state = Arc::new(Mutex::new(Data::default()));
    // Point the data path at a directory so the file write fails.
    let bad = dir.join("as-dir");
    std::fs::create_dir_all(&bad).unwrap();
    let store = MemoryStore::new(state.clone(), bad);
    let r = acquire_with(&store, "/repo", "alice", &["scenes".into()], None, None, 1800, 100);
    assert!(r.is_err(), "write-through failure must error");
    assert!(state.lock().unwrap().locks.is_empty(), "memory unchanged on write failure");
    let _ = std::fs::remove_dir_all(&dir);
}
