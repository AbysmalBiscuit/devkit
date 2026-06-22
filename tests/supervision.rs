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

/// A server that was once ready but stops accepting connections is restarted by
/// the health probe: after K consecutive failed probes the daemon SIGTERMs the
/// hung (but still alive) process, and the reap path respawns it. The fixture
/// hangs only on its first run (guarded by a sentinel file it creates), so the
/// respawn is a healthy server and the pid in ports.json changes.
#[test]
fn health_probe_restarts_hung_server() {
    // Probe every 1 s; restart after 2 consecutive post-arming failures.
    let mut h = Harness::start_with_health(3600, 1, 2);
    let port = common::free_port();
    let holder = h.home.to_str().unwrap().to_string();
    let sentinel = h.home.join("hung-once");

    // Listen on `port`, accepting connections. On first run (sentinel absent),
    // serve ~3 s — long enough for the 1 s probe to arm — then stop accepting but
    // stay alive (hung). On later runs (sentinel present) serve forever (healthy).
    let script = r#"
import socket, os, sys, time
port = int(sys.argv[1]); sentinel = sys.argv[2]
srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", port)); srv.listen(16); srv.settimeout(0.2)
hang_at = None
if not os.path.exists(sentinel):
    open(sentinel, "w").close(); hang_at = time.time() + 3
while True:
    if hang_at is not None and time.time() >= hang_at:
        srv.close()
        while True:
            time.sleep(1)
    try:
        c, _ = srv.accept(); c.close()
    except socket.timeout:
        pass
"#;

    let resp = h.request(&Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Issue,
        argv: vec![
            "python3".into(),
            "-c".into(),
            script.into(),
            port.to_string(),
            sentinel.to_str().unwrap().to_string(),
        ],
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.join("hung.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(v) if v.first().map(|(_, r)| *r) == Some(true)),
        "supervise did not become ready: {resp:?}"
    );

    let pid1 =
        pid_in_ports_json(&h.ports_json(), "api").expect("no pid in ports.json after supervise");

    // The fixture serves ~3 s (probe arms), then hangs; 2 failed probes →
    // SIGTERM → respawn. Poll ports.json for the new pid.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut pid2: Option<u32> = None;
    loop {
        std::thread::sleep(Duration::from_millis(200));
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

    assert!(
        pid2.is_some(),
        "daemon did not restart the hung server within 15 s (pid1={pid1})"
    );
    assert_ne!(
        pid2.unwrap(),
        pid1,
        "pid did not change after health restart"
    );

    h.request(&Request::Down { holder, role: None });
    h.shutdown();
}

/// A server whose tree-RSS balloons past `memory_limit_mb` is restarted: after
/// `memory_limit_ticks` consecutive over-limit ticks the daemon SIGTERMs it and
/// the reap path respawns it. The fixture balloons only on its first run
/// (sentinel-guarded), so the respawn is small and the pid in ports.json changes
/// exactly once.
#[test]
fn memory_restart_over_limit_server() {
    // Act past 60 MB after 2 consecutive over-limit ticks; generous restart budget.
    let mut h = Harness::start_with_memory(3600, 60, 2, 5);
    let port = common::free_port();
    let holder = h.home.to_str().unwrap().to_string();
    let sentinel = h.home.join("ballooned-once");

    // Bind + accept so wait_ready succeeds. First run (sentinel absent): touch
    // ~120 MB resident, then keep serving (alive, over limit). Later runs: small.
    let script = r#"
import socket, os, sys
port = int(sys.argv[1]); sentinel = sys.argv[2]
srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", port)); srv.listen(16); srv.settimeout(0.2)
buf = None
if not os.path.exists(sentinel):
    open(sentinel, "w").close()
    buf = bytearray(120 * 1024 * 1024)
    for i in range(0, len(buf), 4096):
        buf[i] = 1  # fault in each page so RSS (not just VSZ) grows
while True:
    try:
        c, _ = srv.accept(); c.close()
    except socket.timeout:
        pass
"#;

    let resp = h.request(&Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Issue,
        argv: vec![
            "python3".into(),
            "-c".into(),
            script.into(),
            port.to_string(),
            sentinel.to_str().unwrap().to_string(),
        ],
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.join("balloon.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(v) if v.first().map(|(_, r)| *r) == Some(true)),
        "supervise did not become ready: {resp:?}"
    );

    let pid1 =
        pid_in_ports_json(&h.ports_json(), "api").expect("no pid in ports.json after supervise");

    // Poll up to 15 s for the pid to change (daemon restarted the over-limit server).
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut pid2: Option<u32> = None;
    loop {
        std::thread::sleep(Duration::from_millis(200));
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

    assert!(
        pid2.is_some(),
        "daemon did not restart the over-limit server within 15 s (pid1={pid1})"
    );
    assert_ne!(
        pid2.unwrap(),
        pid1,
        "pid did not change after memory restart"
    );

    h.request(&Request::Down { holder, role: None });
    h.shutdown();
}

/// A server that re-balloons on every start is restarted only within the
/// crash-loop budget; once exhausted the daemon leaves it alive (warns) rather
/// than dropping it — unlike a health-probe give-up. With max_restarts = 1 the
/// pid changes once, then the entry stays present.
#[test]
fn memory_restart_gives_up_within_budget() {
    // Act past 60 MB after 2 over-limit ticks; allow only ONE restart.
    let mut h = Harness::start_with_memory(3600, 60, 2, 1);
    let port = common::free_port();
    let holder = h.home.to_str().unwrap().to_string();

    // Balloon on every run (no sentinel): each respawn re-breaches the limit.
    let script = r#"
import socket, sys
port = int(sys.argv[1])
srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", port)); srv.listen(16); srv.settimeout(0.2)
buf = bytearray(120 * 1024 * 1024)
for i in range(0, len(buf), 4096):
    buf[i] = 1
while True:
    try:
        c, _ = srv.accept(); c.close()
    except socket.timeout:
        pass
"#;

    let resp = h.request(&Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Issue,
        argv: vec![
            "python3".into(),
            "-c".into(),
            script.into(),
            port.to_string(),
        ],
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.join("balloon-loop.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(v) if v.first().map(|(_, r)| *r) == Some(true)),
        "supervise did not become ready: {resp:?}"
    );

    let pid1 =
        pid_in_ports_json(&h.ports_json(), "api").expect("no pid in ports.json after supervise");

    // One restart is allowed: wait for the pid to change once.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut pid2: Option<u32> = None;
    loop {
        std::thread::sleep(Duration::from_millis(200));
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
    assert!(
        pid2.is_some(),
        "daemon did not perform the one allowed restart (pid1={pid1})"
    );

    // After the budget is exhausted the server must stay ALIVE (left running),
    // not dropped. Give it several seconds of further over-limit ticks, then
    // confirm the entry is still present.
    std::thread::sleep(Duration::from_secs(3));
    let json = h.ports_json();
    assert!(
        json.contains("\"api\""),
        "over-limit server was dropped after budget exhaustion — it should be left alive: {json}"
    );

    h.request(&Request::Down { holder, role: None });
    h.shutdown();
}

/// A writable cgroup-v2 leaf-capable base for this test, or None to skip.
#[cfg(target_os = "linux")]
fn test_cgroup_base() -> Option<std::path::PathBuf> {
    use std::fs;
    let candidates = [std::env::var_os("DEVKIT_TEST_CGROUP_ROOT").map(std::path::PathBuf::from)];
    candidates.into_iter().flatten().find(|base| {
        fs::create_dir_all(base.join("servers")).is_ok()
            && fs::write(base.join("cgroup.subtree_control"), "+memory\n").is_ok()
    })
}

/// A `memory.max` breach OOM-kills the supervised leaf; the daemon reaps the
/// dead child as a crash and respawns it through the existing crash path. The
/// pid in ports.json changes — kernel enforcement, not a soft poll restart.
#[cfg(target_os = "linux")]
#[test]
fn cgroup_cap_oom_kills_and_respawns() {
    let Some(base) = test_cgroup_base() else {
        eprintln!(
            "skipping cgroup_cap_oom_kills_and_respawns: no writable delegated cgroup-v2 base (set DEVKIT_TEST_CGROUP_ROOT)"
        );
        return;
    };

    let mut h = Harness::start_with_cgroup_cap(3600, base.to_str().unwrap(), 64);
    let port = common::free_port();
    let holder = h.home.to_str().unwrap().to_string();

    // Bind the port so wait_ready succeeds, then balloon past the 64 MB cap.
    // Imports trimmed to exactly what this fixture uses: socket, sys.
    let script = r#"
import socket, sys
port = int(sys.argv[1])
s = socket.socket(); s.bind(("127.0.0.1", port)); s.listen()
blocks = []
while True:
    blocks.append(bytearray(16 * 1024 * 1024))
"#;

    let resp = h.request(&Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Issue,
        argv: vec![
            "python3".into(),
            "-c".into(),
            script.into(),
            port.to_string(),
        ],
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.join("cgroup-balloon.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(v) if v.first().map(|(_, r)| *r) == Some(true)),
        "supervise did not become ready: {resp:?}"
    );

    let pid1 =
        pid_in_ports_json(&h.ports_json(), "api").expect("no pid in ports.json after supervise");

    // Poll up to 30 s for the pid to change: OOM-kill → crash → respawn.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut pid2: Option<u32> = None;
    loop {
        std::thread::sleep(Duration::from_millis(200));
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

    assert!(
        pid2.is_some(),
        "daemon did not respawn after OOM-kill within 30 s (pid1={pid1})"
    );
    assert_ne!(pid2.unwrap(), pid1, "pid did not change after OOM respawn");

    h.request(&Request::Down { holder, role: None });
    h.shutdown();
}

/// When `memory_max_mb > 0` but the cgroup base is non-writable / nonexistent,
/// `cgroup_caps()` returns Unavailable. The daemon still supervises the server
/// uncapped and does not fail the spawn.
#[cfg(target_os = "linux")]
#[test]
fn cap_requested_without_delegation_falls_back() {
    // Point the override at a nonexistent path so setup fails (Unavailable).
    let mut h = Harness::start_with_cgroup_cap(3600, "/sys/fs/cgroup/nonexistent-devkit-test", 64);
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
        logfile: h.home.join("fallback.log"),
        base_port: port,
    });

    // The spawn must succeed even though cgroup delegation is unavailable.
    assert!(
        matches!(&resp, Response::Supervised(v) if v.first().map(|(_, r)| *r) == Some(true)),
        "expected ready=true even without cgroup delegation, got {resp:?}"
    );

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
