mod common;

use common::Harness;
use devkit_locks::daemon::proto::{Request, Response};
use std::time::Duration;

/// A lock acquired through the daemon is held in memory and visible to a later
/// `check` from a different holder.
#[test]
fn acquire_through_daemon_is_visible_to_check() {
    let mut h = Harness::start();
    h.wait_for_lock_socket(Duration::from_secs(5));

    let acq = h.lock_request(&Request::Acquire {
        root: "/repo".into(),
        holder: "alice".into(),
        paths: vec!["scenes".into()],
        pid: None,
        note: Some("refactor".into()),
        ttl: 1800,
    });
    assert!(
        matches!(&acq, Response::Acquired(o) if o.acquired.len() == 1 && o.conflicts.is_empty()),
        "expected one acquired lock, got {acq:?}"
    );

    let chk = h.lock_request(&Request::Check {
        root: "/repo".into(),
        holder: "bob".into(),
        paths: vec!["scenes/player.tscn".into()],
    });
    match chk {
        Response::Conflicts(c) => {
            assert_eq!(c.len(), 1);
            assert_eq!(c[0].held_by, "alice");
        }
        other => panic!("expected a conflict held by alice, got {other:?}"),
    }
    h.shutdown();
}

/// A write decision through the daemon acquires a free file, then denies a
/// non-ancestor holder; a prefix release frees the subtree, written through to file.
#[test]
fn write_decide_and_release_prefix_through_daemon() {
    let mut h = Harness::start();
    h.wait_for_lock_socket(Duration::from_secs(5));

    let acq = h.lock_request(&Request::WriteDecide {
        root: "/repo".into(),
        holder: "S".into(),
        path: "src/a.rs".into(),
        pid: None,
        note: Some("write-harness".into()),
        ttl: 0,
    });
    assert!(
        matches!(
            acq,
            Response::WriteDecided(devkit_locks::model::WriteDecision::Acquired)
        ),
        "expected Acquired, got {acq:?}"
    );

    let denied = h.lock_request(&Request::WriteDecide {
        root: "/repo".into(),
        holder: "T".into(),
        path: "src/a.rs".into(),
        pid: None,
        note: None,
        ttl: 0,
    });
    assert!(
        matches!(
            denied,
            Response::WriteDecided(devkit_locks::model::WriteDecision::Denied(_))
        ),
        "expected Denied, got {denied:?}"
    );

    let freed = h.lock_request(&Request::ReleasePrefix {
        root: "/repo".into(),
        prefix: "S".into(),
    });
    assert!(
        matches!(freed, Response::Freed(ref v) if v.len() == 1),
        "expected one freed, got {freed:?}"
    );
    h.shutdown();
}

/// A lock acquired through the daemon is written through to locks.json, so after
/// the daemon exits the flock fallback still sees it.
#[test]
fn acquired_lock_persists_to_file_after_daemon_exits() {
    let mut h = Harness::start();
    h.wait_for_lock_socket(Duration::from_secs(5));
    h.lock_request(&Request::Acquire {
        root: "/repo".into(),
        holder: "alice".into(),
        paths: vec!["scenes".into()],
        pid: None,
        note: None,
        ttl: 0, // no expiry, so it can't be pruned out before we read the file
    });
    h.shutdown(); // daemon exits; in-memory state is gone, the file must remain
    let body = h.locks_json();
    assert!(
        body.contains("\"holder\": \"alice\""),
        "written-through locks.json missing the lock: {body}"
    );
    assert!(body.contains("scenes"), "lock path not persisted: {body}");
}
