// These tests drive the supervisor with POSIX signals (nix) and a `python3` TCP
// server; both are Unix-only here. Windows supervision coverage is separate.
#![cfg(unix)]

mod common;

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use common::{Harness, pid_in_ports_json};
use devkit_ports::{
    daemon::proto::{Request, Response},
    ledger::{Ledger, Origin},
    registry::Role,
    run::{self, LaunchPlan},
};
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};

/// The fixture server listens on the allocated port; the daemon's `Supervise`
/// handler calls `wait_ready` and reports `ready=true`.
#[test]
fn supervised_python_server_becomes_ready() {
    let mut h = Harness::start();
    let port = common::free_port();
    // The holder must be an existing directory so liveness probes don't prune
    // the registry entry before we can observe it.
    let holder = h.home.path().to_str().unwrap().to_string();

    let resp = h.request(&Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Workspace,
        argv: common::tcp_server_argv(port),
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.path().join("sup.log"),
        base_port: port,
    });

    assert!(
        matches!(&resp, Response::Supervised(s) if s.ready),
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
/// supervisor table (it was not stopped via `Down`). The pid in ports.json
/// changes.
#[test]
fn restart_after_kill() {
    let mut h = Harness::start();
    let port = common::free_port();
    let holder = h.home.path().to_str().unwrap().to_string();

    let resp = h.request(&Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Workspace,
        argv: common::tcp_server_argv(port),
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.path().join("kill.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(s) if s.ready),
        "supervise did not become ready: {resp:?}"
    );

    // Capture the pid the daemon recorded for `api`.
    let pid1 =
        pid_in_ports_json(&h.ports_json(), "api").expect("no pid in ports.json after supervise");

    // SIGKILL the child to simulate a crash.
    kill(Pid::from_raw(pid1 as i32), Signal::SIGKILL).expect("SIGKILL failed");

    // Poll ports.json for up to 8 s until the pid changes (daemon restarted
    // it). Supervision tick is 500 ms + python startup ≈ 1–2 s total.
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
/// the registry. The supervisor table — not the registry row — decides crash
/// vs. stop, so a concurrent `Snapshot` (which drops the dead-pid row inside
/// the daemon) cannot make a crash look like an intentional stop.
#[test]
fn restart_survives_concurrent_snapshot() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    let mut h = Harness::start();
    let port = common::free_port();
    let holder = h.home.path().to_str().unwrap().to_string();

    let resp = h.request(&Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Workspace,
        argv: common::tcp_server_argv(port),
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.path().join("snap.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(s) if s.ready),
        "supervise did not become ready: {resp:?}"
    );

    let pid1 =
        pid_in_ports_json(&h.ports_json(), "api").expect("no pid in ports.json after supervise");

    // Hammer Snapshot from independent connections: each snapshot prunes the
    // dead-pid row inside the daemon, reproducing the prune that races the
    // restart.
    let sock = h.socket();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let hammer = std::thread::spawn(move || {
        use std::io::{BufReader, BufWriter};

        use devkit_ports::daemon::{proto, transport};
        use interprocess::local_socket::{Stream, traits::Stream as _};
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

    // The daemon must restart it despite the concurrent pruning. Poll for a new
    // pid.
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
    let holder = h.home.path().to_str().unwrap().to_string();
    let sentinel = h.home.path().join("hung-once");

    // Listen on `port`, accepting connections. On first run (sentinel absent),
    // serve ~3 s — long enough for the 1 s probe to arm — then stop accepting
    // but stay alive (hung). On later runs (sentinel present) serve forever
    // (healthy).
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
        role: Role::Workspace,
        argv: vec![
            "python3".into(),
            "-c".into(),
            script.into(),
            port.to_string(),
            sentinel.to_str().unwrap().to_string(),
        ],
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.path().join("hung.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(s) if s.ready),
        "supervise did not become ready: {resp:?}"
    );

    let pid1 =
        pid_in_ports_json(&h.ports_json(), "api").expect("no pid in ports.json after supervise");

    // The fixture serves ~3 s (probe arms), then hangs; 2 failed probes ->
    // SIGTERM -> respawn. Poll ports.json for the new pid.
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
/// (sentinel-guarded), so the respawn is small and the pid in ports.json
/// changes exactly once.
#[test]
fn memory_restart_over_limit_server() {
    // Act past 60 MB after 2 consecutive over-limit ticks; generous restart
    // budget.
    let mut h = Harness::start_with_memory(3600, 60, 2, 5);
    let port = common::free_port();
    let holder = h.home.path().to_str().unwrap().to_string();
    let sentinel = h.home.path().join("ballooned-once");

    // Bind + accept so wait_ready succeeds. First run (sentinel absent): touch
    // ~120 MB resident, then keep serving (alive, over limit). Later runs:
    // small.
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
        role: Role::Workspace,
        argv: vec![
            "python3".into(),
            "-c".into(),
            script.into(),
            port.to_string(),
            sentinel.to_str().unwrap().to_string(),
        ],
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.path().join("balloon.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(s) if s.ready),
        "supervise did not become ready: {resp:?}"
    );

    let pid1 =
        pid_in_ports_json(&h.ports_json(), "api").expect("no pid in ports.json after supervise");

    // Poll up to 15 s for the pid to change (daemon restarted the over-limit
    // server).
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
    let holder = h.home.path().to_str().unwrap().to_string();

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
        role: Role::Workspace,
        argv: vec![
            "python3".into(),
            "-c".into(),
            script.into(),
            port.to_string(),
        ],
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.path().join("balloon-loop.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(s) if s.ready),
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
    let holder = h.home.path().to_str().unwrap().to_string();

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
        role: Role::Workspace,
        argv: vec![
            "python3".into(),
            "-c".into(),
            script.into(),
            port.to_string(),
        ],
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.path().join("cgroup-balloon.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(s) if s.ready),
        "supervise did not become ready: {resp:?}"
    );

    let pid1 =
        pid_in_ports_json(&h.ports_json(), "api").expect("no pid in ports.json after supervise");

    // Poll up to 30 s for the pid to change: OOM-kill -> crash -> respawn.
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
    let holder = h.home.path().to_str().unwrap().to_string();

    let resp = h.request(&Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Workspace,
        argv: common::tcp_server_argv(port),
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.path().join("fallback.log"),
        base_port: port,
    });

    // The spawn must succeed even though cgroup delegation is unavailable.
    assert!(
        matches!(&resp, Response::Supervised(s) if s.ready),
        "expected ready=true even without cgroup delegation, got {resp:?}"
    );

    h.request(&Request::Down { holder, role: None });
    h.shutdown();
}

/// A directory that `DEVKIT_DAEMON_CGROUP_ROOT` accepts as a cgroup-v2 base
/// but that is not a cgroup: its leaves get no `cgroup.procs`, so no process
/// can join them.
#[cfg(target_os = "linux")]
fn unjoinable_cgroup_base() -> tempfile::TempDir {
    let base = tempfile::tempdir().unwrap();
    std::fs::write(base.path().join("cgroup.controllers"), "memory\n").unwrap();
    base
}

/// The leaf directories the daemon created under `base`.
#[cfg(target_os = "linux")]
fn leaves(base: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(base.join("servers"))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(target_os = "linux")]
fn pids_in(leaf: &std::path::Path) -> Vec<u32> {
    std::fs::read_to_string(leaf.join("cgroup.procs"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

/// The app is in its leaf before it runs, so a process the app starts is in
/// the leaf too.
#[cfg(target_os = "linux")]
#[test]
fn a_supervised_app_and_its_children_join_the_leaf() {
    let Some(base) = test_cgroup_base() else {
        eprintln!(
            "skipping a_supervised_app_and_its_children_join_the_leaf: no writable delegated cgroup-v2 base (set DEVKIT_TEST_CGROUP_ROOT)"
        );
        return;
    };
    let mut h = Harness::start_with_cgroup_cap(3600, base.to_str().unwrap(), 256);
    let port = common::free_port();
    let holder = h.home.path().to_str().unwrap().to_string();
    let child_pid_file = h.home.path().join("child.pid");
    let mut argv = vec![
        "sh".to_string(),
        "-c".to_string(),
        r#"sleep 600 & echo $! > "$0"; exec "$@""#.to_string(),
        child_pid_file.to_str().unwrap().to_string(),
    ];
    argv.extend(common::tcp_server_argv(port));

    let resp = h.request(&Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Workspace,
        argv,
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.path().join("join.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(s) if s.ready),
        "supervise did not become ready: {resp:?}"
    );
    let app = pid_in_ports_json(&h.ports_json(), "api").expect("no pid in ports.json");
    let child: u32 = std::fs::read_to_string(&child_pid_file)
        .expect("the app wrote its child's pid")
        .trim()
        .parse()
        .unwrap();

    let leaf = leaves(&base)
        .into_iter()
        .find(|leaf| pids_in(leaf).contains(&app))
        .unwrap_or_else(|| panic!("app pid {app} is in no leaf under {}", base.display()));
    let members = pids_in(&leaf);
    let _ = kill(Pid::from_raw(child as i32), Signal::SIGKILL);
    assert!(
        members.contains(&child),
        "child pid {child} is not in {}: {members:?}",
        leaf.display()
    );

    h.request(&Request::Down { holder, role: None });
    h.shutdown();
}

/// A leaf the app cannot join leaves it uncapped, and the launch still
/// succeeds.
#[cfg(target_os = "linux")]
#[test]
fn an_unwritable_leaf_leaves_the_app_uncapped() {
    let base = unjoinable_cgroup_base();
    let mut h = Harness::start_with_cgroup_cap(3600, base.path().to_str().unwrap(), 64);
    let port = common::free_port();
    let holder = h.home.path().to_str().unwrap().to_string();

    let resp = h.request(&Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Workspace,
        argv: common::tcp_server_argv(port),
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.path().join("unjoinable.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(s) if s.ready),
        "an unjoinable leaf must not stop the app: {resp:?}"
    );
    let leaves = leaves(base.path());
    assert_eq!(leaves.len(), 1, "the daemon created one leaf: {leaves:?}");
    assert!(
        pids_in(&leaves[0]).is_empty(),
        "nothing joined the unjoinable leaf"
    );

    h.request(&Request::Down { holder, role: None });
    h.shutdown();
}

/// A capped launch still starts the app after the daemon's binary is deleted,
/// as an upgrade that replaces it does.
#[cfg(target_os = "linux")]
#[test]
fn a_capped_launch_survives_a_deleted_daemon_binary() {
    let base = unjoinable_cgroup_base();
    let bin_dir = tempfile::tempdir().unwrap();
    let bin = bin_dir.path().join("devkitd");
    std::fs::copy(env!("CARGO_BIN_EXE_devkitd"), &bin).unwrap();
    let mut h = Harness::start_binary(&bin, &[
        ("DEVKIT_DAEMON_IDLE_SECS", "3600".to_string()),
        ("DEVKIT_DAEMON_MEM_MAX_MB", "64".to_string()),
        (
            "DEVKIT_DAEMON_CGROUP_ROOT",
            base.path().to_str().unwrap().to_string(),
        ),
    ]);
    std::fs::remove_file(&bin).unwrap();
    let port = common::free_port();
    let holder = h.home.path().to_str().unwrap().to_string();

    let resp = h.request(&Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Workspace,
        argv: common::tcp_server_argv(port),
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.path().join("deleted.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(s) if s.ready),
        "a deleted daemon binary must not stop a capped launch: {resp:?}"
    );

    h.request(&Request::Down { holder, role: None });
    h.shutdown();
}

/// With no `PATH` in the daemon or the launch, a capped launch finds a bare
/// command in the default search path, as an uncapped launch does.
#[cfg(target_os = "linux")]
#[test]
fn a_capped_launch_without_path_uses_the_default_search_path() {
    let path = std::env::var_os("PATH").expect("the test runs with a PATH");
    let python = std::env::split_paths(&path)
        .map(|dir| dir.join("python3"))
        .find(|candidate| candidate.is_file())
        .expect("python3 on PATH");
    let base = unjoinable_cgroup_base();
    let uncapped = vec![("DEVKIT_DAEMON_IDLE_SECS", "3600".to_string())];
    let mut capped = uncapped.clone();
    capped.extend([
        ("DEVKIT_DAEMON_MEM_MAX_MB", "64".to_string()),
        (
            "DEVKIT_DAEMON_CGROUP_ROOT",
            base.path().to_str().unwrap().to_string(),
        ),
    ]);
    for (label, env) in [("uncapped", uncapped), ("capped", capped)] {
        let mut h = Harness::start_without(&["PATH"], &env);
        let port = common::free_port();
        let holder = h.home.path().to_str().unwrap().to_string();
        let mut server = common::tcp_server_argv(port);
        server[0] = python.to_str().unwrap().to_string();
        let mut argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            r#"exec "$@""#.to_string(),
            "sh".to_string(),
        ];
        argv.extend(server);

        let resp = h.request(&Request::Supervise {
            holder: holder.clone(),
            app: "api".into(),
            role: Role::Workspace,
            argv,
            cwd: ".".into(),
            env: BTreeMap::new(),
            logfile: h.home.path().join("no-path.log"),
            base_port: port,
        });
        assert!(
            matches!(&resp, Response::Supervised(s) if s.ready),
            "an {label} bare command must resolve without PATH: {resp:?}"
        );

        h.request(&Request::Down { holder, role: None });
        h.shutdown();
    }
}

/// A capped launch of a program that exists but cannot exec fails the request,
/// as an uncapped launch does.
#[cfg(target_os = "linux")]
#[test]
fn a_program_that_cannot_exec_fails_the_launch() {
    use std::os::unix::fs::PermissionsExt as _;
    let base = unjoinable_cgroup_base();
    let mut h = Harness::start_with_cgroup_cap(3600, base.path().to_str().unwrap(), 64);
    let port = common::free_port();
    let holder = h.home.path().to_str().unwrap().to_string();
    let script = h.home.path().join("no-interpreter");
    std::fs::write(&script, "#!/nonexistent/interpreter\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let program = script.to_str().unwrap().to_string();

    let resp = h.request(&Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Workspace,
        argv: vec![program.clone()],
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.path().join("no-interpreter.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Err(msg) if msg.contains(&program)),
        "expected an error naming {program}, got {resp:?}"
    );

    h.request(&Request::Down { holder, role: None });
    h.shutdown();
}

/// A capped launch of a program that does not exist fails the request with an
/// error that names the program.
#[cfg(target_os = "linux")]
#[test]
fn a_missing_program_fails_the_launch() {
    let base = unjoinable_cgroup_base();
    let mut h = Harness::start_with_cgroup_cap(3600, base.path().to_str().unwrap(), 64);
    let port = common::free_port();
    let holder = h.home.path().to_str().unwrap().to_string();
    let program = "devkit-test-no-such-program";

    let resp = h.request(&Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Workspace,
        argv: vec![program.into()],
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.path().join("missing.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Err(msg) if msg.contains(program)),
        "expected an error naming {program}, got {resp:?}"
    );

    h.request(&Request::Down { holder, role: None });
    h.shutdown();
}

/// A second Supervise for a key whose server is alive is a no-op: same port,
/// ready, and the pid in ports.json does not change (no duplicate spawn).
/// Only the first answer says it spawned the server, so only its caller
/// records the server's origin.
#[test]
fn second_supervise_of_live_server_is_noop() {
    let mut h = Harness::start();
    let port = common::free_port();
    let holder = h.home.path().to_str().unwrap().to_string();
    let req = Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Workspace,
        argv: common::tcp_server_argv(port),
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.path().join("noop.log"),
        base_port: port,
    };

    let first = h.request(&req);
    let pid1 = pid_in_ports_json(&h.ports_json(), "api").expect("no pid after first supervise");
    assert!(
        matches!(&first, Response::Supervised(s) if s.ready && s.spawned && s.pid == pid1),
        "first supervise must report the server it spawned: {first:?}"
    );

    let second = h.request(&req);
    assert!(
        matches!(&second, Response::Supervised(s)
            if s.port == port && s.ready && !s.spawned && s.pid == pid1),
        "second supervise must report the live server: {second:?}"
    );
    let pid2 = pid_in_ports_json(&h.ports_json(), "api").expect("no pid after second supervise");
    assert_eq!(pid2, pid1, "second supervise must not respawn");

    h.request(&Request::Down { holder, role: None });
    h.shutdown();
}

/// Two Supervise requests for one key that arrive together spawn one server,
/// and only one answer says it spawned it, so only one caller records an
/// origin for the server.
#[test]
fn concurrent_supervise_spawns_once() {
    let mut h = Harness::start();
    let port = common::free_port();
    let holder = h.home.path().to_str().unwrap().to_string();
    let req = Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Workspace,
        argv: common::tcp_server_argv(port),
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.path().join("race.log"),
        base_port: port,
    };

    let start = std::sync::Barrier::new(2);
    let answers: Vec<Response> = std::thread::scope(|s| {
        let calls: Vec<_> = (0..2)
            .map(|_| {
                s.spawn(|| {
                    start.wait();
                    h.request(&req)
                })
            })
            .collect();
        calls.into_iter().map(|c| c.join().unwrap()).collect()
    });

    let pid = pid_in_ports_json(&h.ports_json(), "api").expect("no pid after supervise");
    let spawned = answers
        .iter()
        .filter(|a| match a {
            Response::Supervised(s) => {
                assert_eq!(s.pid, pid, "each answer names the registered server: {a:?}");
                s.spawned
            }
            other => panic!("supervise failed: {other:?}"),
        })
        .count();
    assert_eq!(
        spawned, 1,
        "exactly one answer spawned the server: {answers:?}"
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
    let holder = h.home.path().to_str().unwrap().to_string();

    let resp = h.request(&Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Workspace,
        argv: common::tcp_server_argv(port),
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.path().join("down.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(s) if s.ready),
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

/// Two Supervise requests for one app race with different launch plans: the
/// second arrives while the first one's server is live but not yet ready.
/// Only the request whose answer says it spawned the server records an
/// origin, so the ledger holds the spawner's commit and plan digest. The
/// other request reports the server as reused, with that recorded origin,
/// so a verify run cannot pass it off as launched from its own plan.
#[test]
fn only_the_spawning_supervise_records_an_origin() {
    let mut h = Harness::start();
    let port = common::free_port();
    let holder = h.home.path().to_str().unwrap().to_string();
    let ledger_dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::at(ledger_dir.path());

    let plan = |argv: Vec<String>, mode: &str| LaunchPlan {
        app: "api".into(),
        port,
        argv,
        cwd: ".".into(),
        env: BTreeMap::from([("MODE".to_string(), mode.to_string())]),
        log: h.home.path().join(format!("race-{mode}.log")),
        url: String::new(),
    };
    let mut slow_server = vec![
        "sh".into(),
        "-c".into(),
        r#"sleep 2; exec "$@""#.into(),
        "sh".into(),
    ];
    slow_server.extend(common::tcp_server_argv(port));
    let first = plan(slow_server, "first");
    let second = plan(common::tcp_server_argv(port), "second");
    let request = |p: &LaunchPlan| Request::Supervise {
        holder: holder.clone(),
        app: p.app.clone(),
        role: Role::Workspace,
        argv: p.argv.clone(),
        cwd: p.cwd.to_str().unwrap().to_string(),
        env: p.env.clone(),
        logfile: p.log.clone(),
        base_port: p.port,
    };
    let origin_of = |p: &LaunchPlan, commit: &str| Origin {
        commit: commit.into(),
        plan: p.digest(),
    };
    let first_origin = origin_of(&first, &"a".repeat(40));
    let second_origin = origin_of(&second, &"b".repeat(40));
    assert_ne!(first_origin.plan, second_origin.plan);

    let (first_resp, second_resp) = std::thread::scope(|s| {
        let first_req = s.spawn(|| h.request(&request(&first)));
        let deadline = Instant::now() + Duration::from_secs(10);
        while pid_in_ports_json(&h.ports_json(), "api").is_none() {
            assert!(
                Instant::now() < deadline,
                "the first server never got a pid"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !first_req.is_finished(),
            "the first request answered before the race"
        );
        let second_resp = h.request(&request(&second));
        (first_req.join().unwrap(), second_resp)
    });

    let (Response::Supervised(first_srv), Response::Supervised(second_srv)) =
        (&first_resp, &second_resp)
    else {
        panic!("both requests must be supervised: {first_resp:?}, {second_resp:?}");
    };
    assert!(first_srv.spawned && first_srv.ready, "{first_srv:?}");
    assert!(!second_srv.spawned, "{second_srv:?}");
    assert_eq!(second_srv.pid, first_srv.pid);

    let status = |p: &LaunchPlan, srv, origin: &Origin| {
        run::supervised_status(Some(&ledger), p, Role::Workspace, srv, Some(origin.clone()))
    };

    // The losing request answers first, so it reads the ledger before the
    // spawner records; the spawner's record must survive it either way.
    let loser = status(&second, second_srv, &second_origin);
    assert!(
        loser.reused,
        "the losing request reports a launch: {loser:?}"
    );
    assert_eq!(loser.pid, Some(first_srv.pid));
    assert_eq!(loser.origin, None, "{loser:?}");
    assert_eq!(
        ledger.origin(port, first_srv.pid),
        None,
        "the losing request wrote an origin"
    );

    let spawner = status(&first, first_srv, &first_origin);
    assert!(!spawner.reused, "{spawner:?}");
    assert_eq!(spawner.pid, Some(first_srv.pid));
    assert_eq!(
        ledger.origin(port, first_srv.pid),
        Some(first_origin.clone())
    );

    let loser = status(&second, second_srv, &second_origin);
    assert!(loser.reused, "{loser:?}");
    assert_eq!(
        loser.origin.as_ref(),
        Some(&first_origin),
        "the loser must carry the spawner's provenance, not its own plan"
    );
    assert_eq!(ledger.origin(port, first_srv.pid), Some(first_origin));

    h.request(&Request::Down { holder, role: None });
    h.shutdown();
}
