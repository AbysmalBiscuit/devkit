use devkit_ports::registry::{self, MemoryStore, Role};
use std::sync::{Arc, Mutex};

#[test]
fn memory_store_serves_reads_from_memory_after_alloc() {
    let dir = std::env::temp_dir().join(format!("devkit-memstore-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let state = Arc::new(Mutex::new(registry::Data::default()));
    let store = MemoryStore::new(state.clone(), dir.join("ports.json"));

    // Use the temp dir itself as the holder so holder_alive() returns true.
    let holder = dir.to_string_lossy().into_owned();
    let out = registry::alloc_with(&store, &holder, &[("api".to_string(), 9100)], Role::Issue).unwrap();
    let (_, port) = out[0];

    // A snapshot reflects the alloc straight from memory (no file read needed).
    let snap = registry::snapshot_with(&store).unwrap();
    assert!(snap.entries.contains_key(&port));
    let _ = std::fs::remove_dir_all(&dir);
}
