// The supervised-child test drives a real daemon with a TCP readiness probe,
// and the fake daemon binds unix sockets; both stay Unix-only like the
// supervision tests.
#![cfg(unix)]

mod common;

use std::{
    collections::BTreeMap,
    net::TcpListener,
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use common::Harness;
use devkit_ports::{
    daemon::{
        proto::{Request, Response},
        transport,
    },
    registry::{self, Role},
};
use interprocess::local_socket::{ListenerOptions, traits::ListenerExt as _};

/// Bind a fake daemon at `sock` that drops every connection at once, and
/// count the connections it accepts.
fn fake_daemon(sock: &Path) -> Arc<AtomicUsize> {
    let listener = ListenerOptions::new()
        .name(transport::socket_name(sock).unwrap())
        .create_sync()
        .unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = accepted.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            count.fetch_add(1, Ordering::SeqCst);
            drop(conn);
        }
    });
    accepted
}

/// A process that marks itself as the daemon resolves port and lock facade
/// calls locally, even with a daemon listening on both sockets.
#[test]
fn daemon_facade_calls_resolve_locally() {
    if let Ok(holder) = std::env::var("DEVKIT_TEST_MARKED_DAEMON") {
        devkit_common::daemon::mark_in_daemon();
        registry::alloc(&holder, &[("probe".into(), 9300)], Role::Workspace).unwrap();
        devkit_locks::check_resolved(&holder, &holder, &["a.rs".into()]).unwrap();
        std::process::exit(0);
    }

    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let devkit_state = state.join("devkit");
    std::fs::create_dir_all(&devkit_state).unwrap();
    let port_conns = fake_daemon(&devkit_state.join("ports.sock"));
    let lock_conns = fake_daemon(&devkit_state.join("locks.sock"));

    let out = Command::new(std::env::current_exe().unwrap())
        .env("HOME", tmp.path())
        .env("XDG_STATE_HOME", &state)
        .env("DEVKIT_TEST_MARKED_DAEMON", tmp.path())
        .args([
            "--exact",
            "daemon_facade_calls_resolve_locally",
            "--nocapture",
        ])
        .output()
        .unwrap();

    assert!(out.status.success(), "marked child failed: {out:?}");
    assert_eq!(
        port_conns.load(Ordering::SeqCst),
        0,
        "ports facade connected"
    );
    assert_eq!(
        lock_conns.load(Ordering::SeqCst),
        0,
        "locks facade connected"
    );
    let ports = std::fs::read_to_string(devkit_state.join("ports.json")).unwrap();
    assert!(
        ports.contains("\"probe\""),
        "alloc did not land locally: {ports}"
    );
}

/// A process the daemon supervises allocates through the daemon, not behind
/// it: the allocation shows in the daemon's own snapshot.
#[test]
fn supervised_child_reaches_the_daemon() {
    if let Ok(holder) = std::env::var("DEVKIT_TEST_SUPERVISED_CHILD") {
        let port = std::env::var("DEVKIT_TEST_READY_PORT").unwrap();
        let _ready = TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
        if let Err(e) = registry::alloc(&holder, &[("probe".into(), 9300)], Role::Workspace) {
            eprintln!("alloc failed: {e:#}");
        }
        std::thread::sleep(Duration::from_secs(60));
        std::process::exit(0);
    }

    let mut h = Harness::start();
    let port = common::free_port();
    let holder = h.home.path().to_str().unwrap().to_string();
    let env = BTreeMap::from([
        ("DEVKIT_TEST_SUPERVISED_CHILD".to_string(), holder.clone()),
        ("DEVKIT_TEST_READY_PORT".to_string(), port.to_string()),
    ]);

    let resp = h.request(&Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Workspace,
        argv: vec![
            std::env::current_exe()
                .unwrap()
                .to_str()
                .unwrap()
                .to_string(),
            "--exact".into(),
            "supervised_child_reaches_the_daemon".into(),
            "--nocapture".into(),
        ],
        cwd: ".".into(),
        env,
        logfile: h.home.path().join("child.log"),
        base_port: port,
        origin: None,
    });
    assert!(
        matches!(&resp, Response::Supervised(v) if v.first().map(|(_, r)| *r) == Some(true)),
        "supervise did not become ready: {resp:?}"
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = h.request(&Request::Snapshot);
        let Response::Snapshot(data) = &snapshot else {
            panic!("expected a snapshot, got {snapshot:?}");
        };
        if data.entries.values().any(|e| e.app == "probe") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the child's alloc did not reach the daemon: {data:?}\nchild log:\n{}",
            std::fs::read_to_string(h.home.path().join("child.log")).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    h.request(&Request::Down { holder, role: None });
    h.shutdown();
}
