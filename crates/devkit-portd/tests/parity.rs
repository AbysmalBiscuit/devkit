mod common;

use common::Harness;
use devkit_ports::daemon::proto::{Request, Response};
use devkit_ports::registry::Role;

/// Alloc through the daemon writes a row to ports.json that contains the holder
/// and app name.  Release removes it (returns Freed with at least one port).
///
/// The holder must be an existing directory: `dead_ports()` checks `holder_alive`
/// (i.e. whether the path exists) and prunes entries whose holder is gone.
#[test]
fn alloc_through_daemon_writes_registry() {
    let mut h = Harness::start();
    // Use the test's throwaway HOME as the holder (it exists on disk).
    let holder = h.home.to_str().unwrap().to_string();

    let resp = h.request(&Request::Alloc {
        holder: holder.clone(),
        reqs: vec![("api".into(), 19100)],
        role: Role::Issue,
    });

    let ports = match resp {
        Response::Ports(p) => p,
        other => panic!("expected Ports, got {other:?}"),
    };
    assert!(
        ports.iter().any(|(name, _)| name == "api"),
        "alloc did not return an 'api' port: {ports:?}"
    );

    // The daemon must have flushed the row to disk.
    let json = h.ports_json();
    assert!(json.contains("\"api\""), "ports.json missing 'api': {json}");
    assert!(
        json.contains(holder.as_str()),
        "ports.json missing holder '{holder}': {json}"
    );

    let resp2 = h.request(&Request::Release {
        holder: holder.clone(),
        role: None,
    });
    assert!(
        matches!(resp2, Response::Freed(ref v) if !v.is_empty()),
        "expected non-empty Freed, got {resp2:?}"
    );

    h.shutdown();
}

/// Snapshot returns the full registry, including an entry allocated via the daemon.
///
/// The holder must be an existing directory so it survives the liveness prune in
/// `snapshot_flock`.
#[test]
fn snapshot_roundtrips() {
    let mut h = Harness::start();
    let holder = h.home.to_str().unwrap().to_string();

    // Alloc a slot.
    h.request(&Request::Alloc {
        holder: holder.clone(),
        reqs: vec![("api".into(), 19200)],
        role: Role::Issue,
    });

    let snap = match h.request(&Request::Snapshot) {
        Response::Snapshot(d) => d,
        other => panic!("expected Snapshot, got {other:?}"),
    };
    let found = snap.entries.values().any(|e| e.holder == holder);
    assert!(found, "Snapshot did not contain entry for '{holder}': {snap:?}");

    h.request(&Request::Release { holder, role: None });
    h.shutdown();
}
