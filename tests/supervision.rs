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

/// After SIGKILLing the supervised child, the daemon's supervision thread reaps
/// the exit and respawns the server, because the child is still tracked in the
/// supervisor table (it was not stopped via `Down`). The pid in ports.json changes.
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
    // Supervision tick is 500 ms + python startup ≈ 1–2 s total.
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

/// A supervised child that is SIGKILLed restarts even while clients are pruning
/// the registry. The supervisor table — not the registry row — decides crash vs.
/// stop, so a concurrent `Snapshot` (which drops the dead-pid row inside the
/// daemon) cannot make a crash look like an intentional stop.
#[test]
fn restart_survives_concurrent_snapshot() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

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
        logfile: h.home.join("snap.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(v) if v.first().map(|(_, r)| *r) == Some(true)),
        "supervise did not become ready: {resp:?}"
    );

    let pid1 =
        pid_in_ports_json(&h.ports_json(), "api").expect("no pid in ports.json after supervise");

    // Hammer Snapshot from independent connections: each snapshot prunes the
    // dead-pid row inside the daemon, reproducing the prune that races the restart.
    let sock = h.socket();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let hammer = std::thread::spawn(move || {
        use devkit_ports::daemon::proto;
        use devkit_ports::daemon::transport;
        use interprocess::local_socket::Stream;
        use interprocess::local_socket::traits::Stream as _;
        use std::io::{BufReader, BufWriter};
        while !stop_thread.load(Ordering::Relaxed) {
            if let Ok(name) = transport::socket_name(&sock)
                && let Ok(stream) = Stream::connect(name)
            {
                let (recv, send) = stream.split();
                let mut writer = BufWriter::new(send);
                let mut reader = BufReader::new(recv);
                if proto::send(&mut writer, &Request::Snapshot).is_ok() {
                    let _ = proto::recv::<_, Response>(&mut reader);
                }
            }
        }
    });

    // SIGKILL the child to simulate a crash.
    kill(Pid::from_raw(pid1 as i32), Signal::SIGKILL).expect("SIGKILL failed");

    // The daemon must restart it despite the concurrent pruning. Poll for a new pid.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut pid2: Option<u32> = None;
    loop {
        std::thread::sleep(Duration::from_millis(150));
        if let Some(p) = pid_in_ports_json(&h.ports_json(), "api")
            && p != pid1
        {
            pid2 = Some(p);
            break;
        }
        if Instant::now() >= deadline {
            break;
        }
    }

    stop.store(true, Ordering::Relaxed);
    let _ = hammer.join();

    assert!(
        pid2.is_some(),
        "daemon did not restart the killed server under concurrent snapshots (pid1={pid1})"
    );
    assert_ne!(pid2.unwrap(), pid1, "pid did not change after respawn");

    h.request(&Request::Down { holder, role: None });
    h.shutdown();
}

/// After a clean `Down`, the supervision thread must NOT restart the server —
/// `Down` removes the key from the supervisor table before stopping the child,
/// so the exit is never reaped as a crash.
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
