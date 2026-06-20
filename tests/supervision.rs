// These tests drive the supervisor with POSIX signals (nix) and a `python3`
// http server; both are Unix-only here. Windows supervision coverage is separate.
#![cfg(unix)]

mod common;

use common::{Harness, pid_in_ports_json};
use devkit_ports::daemon::proto::{Request, Response};
use devkit_ports::registry::Role;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// A python3 http.server binds quickly on the allocated port; the daemon's
/// `Supervise` handler calls `wait_ready` and reports `ready=true`.
#[test]
fn supervised_python_server_becomes_ready() {
    let mut h = Harness::start();
    let port = common::free_port();
    // The holder must be an existing directory so liveness probes don't prune
    // the registry entry before we can observe it.
    let holder = h.home.to_str().unwrap().to_string();

    let resp = h.request(&Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Issue,
        argv: vec![
            "python3".into(),
            "-m".into(),
            "http.server".into(),
            port.to_string(),
        ],
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.join("sup.log"),
        base_port: port,
    });

    assert!(
        matches!(&resp, Response::Supervised(v) if v.first().map(|(_, r)| *r) == Some(true)),
        "expected Supervised([(_, true)]), got {resp:?}"
    );

    // Tear down the supervised server cleanly.
    let down = h.request(&Request::Down {
        holder: holder.clone(),
        role: None,
    });
    assert!(
        matches!(down, Response::Freed(_)),
        "expected Freed after Down, got {down:?}"
    );

    h.shutdown();
}

/// After SIGKILLing the supervised child, the daemon's supervision thread
/// detects the exit, debounces (200 ms), sees the row in ports.json (not a
/// clean `Down`), and respawns the server.  The pid in ports.json changes.
#[test]
fn restart_after_kill() {
    let mut h = Harness::start();
    let port = common::free_port();
    let holder = h.home.to_str().unwrap().to_string();

    let resp = h.request(&Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Issue,
        argv: vec![
            "python3".into(),
            "-m".into(),
            "http.server".into(),
            port.to_string(),
        ],
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.join("kill.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(v) if v.first().map(|(_, r)| *r) == Some(true)),
        "supervise did not become ready: {resp:?}"
    );

    // Capture the pid the daemon recorded for `api`.
    let pid1 =
        pid_in_ports_json(&h.ports_json(), "api").expect("no pid in ports.json after supervise");

    // SIGKILL the child to simulate a crash.
    kill(Pid::from_raw(pid1 as i32), Signal::SIGKILL).expect("SIGKILL failed");

    // Poll ports.json for up to 8 s until the pid changes (daemon restarted it).
    // Supervision tick is 500 ms + 200 ms debounce + python startup ≈ 1–2 s total.
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut pid2: Option<u32> = None;
    loop {
        std::thread::sleep(Duration::from_millis(150));
        let json = h.ports_json();
        if let Some(p) = pid_in_ports_json(&json, "api")
            && p != pid1
        {
            pid2 = Some(p);
            break;
        }
        if Instant::now() >= deadline {
            break;
        }
    }

    assert!(
        pid2.is_some(),
        "daemon did not restart the killed server within 8 s (pid1={pid1})"
    );
    assert_ne!(pid2.unwrap(), pid1, "pid did not change after respawn");

    // The row must still be present (not cleaned up).
    let json = h.ports_json();
    assert!(json.contains("\"api\""), "entry disappeared after respawn");

    // Clean up.
    h.request(&Request::Down { holder, role: None });
    h.shutdown();
}

/// After a clean `Down`, the supervision thread must NOT restart the server —
/// the row is gone from ports.json so the debounce path correctly treats the
/// exit as intentional.
#[test]
fn down_does_not_restart() {
    let mut h = Harness::start();
    let port = common::free_port();
    let holder = h.home.to_str().unwrap().to_string();

    let resp = h.request(&Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Issue,
        argv: vec![
            "python3".into(),
            "-m".into(),
            "http.server".into(),
            port.to_string(),
        ],
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.join("down.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(v) if v.first().map(|(_, r)| *r) == Some(true)),
        "supervise did not become ready: {resp:?}"
    );

    let down = h.request(&Request::Down {
        holder: holder.clone(),
        role: None,
    });
    assert!(
        matches!(down, Response::Freed(_)),
        "expected Freed from Down, got {down:?}"
    );

    // Wait a few supervision ticks (500 ms each) to confirm no restart happens.
    std::thread::sleep(Duration::from_millis(1500));

    let snap = match h.request(&Request::Snapshot) {
        Response::Snapshot(d) => d,
        other => panic!("expected Snapshot, got {other:?}"),
    };
    let still_there = snap.entries.values().any(|e| e.holder == holder);
    assert!(
        !still_there,
        "daemon restarted the server after a clean Down — entry should be gone: {snap:?}"
    );

    h.shutdown();
}
