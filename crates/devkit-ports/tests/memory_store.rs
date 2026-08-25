use devkit_ports::registry::{self, MemoryStore, Role};
use std::sync::{Arc, Mutex};

#[test]
fn memory_store_serves_reads_from_memory_after_alloc() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(Mutex::new(registry::Data::default()));
    let store = MemoryStore::new(state.clone(), dir.path().join("ports.json"));

    // Use the temp dir itself as the holder so holder_alive() returns true.
    let holder = dir.path().to_string_lossy().into_owned();
    let out =
        registry::alloc_with(&store, &holder, &[("api".to_string(), 9100)], Role::Issue).unwrap();
    let (_, port) = out[0];

    // A snapshot reflects the alloc straight from memory (no file read needed).
    let snap = registry::snapshot_with(&store).unwrap();
    assert!(snap.entries.contains_key(&port));
}
