//! devkitd — optional supervisor daemon. Single-instance (devkitd.lock),
//! binds a unix socket, serves one request per line. This entry point owns
//! lifecycle: lock, bind, accept, idle-exit; supervision and registry dispatch
//! live in submodules.

use anyhow::{Context, Result};
use devkit_common::paths;
use devkit_ports::daemon::proto::{self, Request};
use devkit_ports::daemon::transport;
use devkit_ports::registry;
use fd_lock::RwLock;
use interprocess::local_socket::traits::{ListenerExt as _, Stream as _};
use interprocess::local_socket::{ListenerOptions, Stream};
use std::fs::OpenOptions;
use std::io::{BufReader, BufWriter};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// A few supervisor accessors back deferred features (memory-limit action, log serving)
// and have no caller yet.
mod cgroup;
mod lock_server;
mod server;
mod service;
#[allow(dead_code)]
mod supervisor;

/// Active hard-cap parameters for the spawn paths.
pub(crate) struct CgroupCap {
    pub(crate) base: std::path::PathBuf,
    pub(crate) max_bytes: u64,
}

/// Shared daemon state, accessed from the connection threads and the idle watcher.
pub(crate) struct Daemon {
    pub(crate) last_activity: Mutex<Instant>,
    pub(crate) active_conns: AtomicUsize,
    pub(crate) shutdown: AtomicBool,
    pub(crate) idle_timeout: Duration,
    pub(crate) sup: Mutex<supervisor::Supervisor>,
    /// Authoritative port registry, served from memory; the file is write-through.
    pub(crate) ports: std::sync::Arc<std::sync::Mutex<registry::Data>>,
    /// Authoritative lock registry, served from memory; the file is write-through.
    pub(crate) locks: std::sync::Arc<std::sync::Mutex<devkit_locks::model::Data>>,
    /// Resolved hard-cap state: `Some` only when `memory_max_mb > 0` and cgroup-v2
    /// enforcement is available. Consulted by both spawn paths.
    pub(crate) cgroup_cap: Option<CgroupCap>,
}

impl Daemon {
    fn touch(&self) {
        *self.last_activity.lock().unwrap() = Instant::now();
    }

    /// A `Store` view over the daemon's authoritative registry.
    pub(crate) fn port_store(&self) -> registry::MemoryStore {
        registry::MemoryStore::new(self.ports.clone(), devkit_common::paths::registry_file())
    }

    /// A `Store` view over the daemon's authoritative lock registry.
    pub(crate) fn lock_store(&self) -> devkit_locks::store::MemoryStore {
        devkit_locks::store::MemoryStore::new(
            self.locks.clone(),
            devkit_common::paths::locks_file(),
        )
    }

    /// Idle = no live connections and no supervised children, for longer than the
    /// timeout. Supervision suppresses this by keeping `supervising()` true.
    fn is_idle(&self) -> bool {
        self.active_conns.load(Ordering::SeqCst) == 0
            && !self.supervising()
            && self.last_activity.lock().unwrap().elapsed() >= self.idle_timeout
    }
    /// Whether the daemon currently owns live supervised child processes.
    fn supervising(&self) -> bool {
        self.sup.lock().unwrap().any_live()
    }

    fn respawn(self: &Arc<Self>, key: &supervisor::Key) {
        let Some((launch, log, port)) = self.sup.lock().unwrap().launch_of(key) else {
            log_line(&format!(
                "cannot respawn {}/{} — no launch spec",
                key.holder, key.app
            ));
            return;
        };
        let leaf = crate::cgroup::leaf_for(self, key);
        match devkit_common::supervise::spawn_detached(
            &launch.argv,
            &launch.cwd,
            &launch.env,
            &log,
            leaf.as_deref(),
        ) {
            Ok(pid) => {
                let _ = registry::record_pid_with(
                    &self.port_store(),
                    port,
                    &key.app,
                    &key.holder,
                    key.role,
                    pid,
                    log,
                );
                self.sup.lock().unwrap().set_pid(key, pid);
            }
            Err(e) => log_line(&format!(
                "respawn failed for {}/{}: {e:#}",
                key.holder, key.app
            )),
        }
    }
}

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("devkitd");
    match std::env::args().nth(1).as_deref() {
        Some("install-service") => return service::install(),
        Some("uninstall-service") => return service::uninstall(),
        _ => {}
    }
    // Mark this process as the daemon so registry facade calls resolve locally
    // instead of connecting back to this same daemon over the socket.
    unsafe { std::env::set_var("DEVKITD_SELF", "1") };
    std::fs::create_dir_all(paths::state_dir())?;
    std::fs::create_dir_all(paths::logs_dir())?;

    // Single-instance: hold devkitd.lock for the daemon's whole life. A peer daemon
    // holds it exclusive for its entire lifetime, so all retry attempts fail →
    // exit 0 (exactly one autostart winner). A transient shared hold by a direct
    // writer (portm/devrun taking the gate during a registry RMW) clears within
    // ~1ms, so a brief retry distinguishes that from a live peer without blocking.
    let lock_path = paths::devkitd_lock();
    let lock_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    let mut lock = RwLock::new(lock_file);
    // Retry up to 5 times with 20ms gaps. A peer daemon holds devkitd.lock exclusive
    // for its whole life, so all attempts fail → exit 0 (one autostart winner). A
    // transient shared hold by a direct registry writer clears within ~1ms, so a
    // retry succeeds without blocking indefinitely.
    let guard = 'acquire: {
        let mut attempts = 0u8;
        loop {
            match lock.try_write() {
                Ok(g) => break 'acquire g,
                Err(_) if attempts < 4 => {
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(_) => return Ok(()), // a live peer daemon holds the lock
            }
        }
    };

    // Load the registries into memory while holding devkitd.lock and before binding any
    // socket, so no request is ever served against an unpopulated registry.
    let ports = std::sync::Arc::new(std::sync::Mutex::new(registry::load()));
    let locks = std::sync::Arc::new(std::sync::Mutex::new(devkit_locks::store::load()));

    // Holding the lock, no live daemon owns the socket — clear any stale one and bind.
    let sock = paths::port_socket_file();
    let _ = std::fs::remove_file(&sock); // clear a stale unix socket file before binding
    let name = transport::socket_name(&sock).with_context(|| "building socket name")?;
    let listener = ListenerOptions::new()
        .name(name)
        .create_sync()
        .with_context(|| format!("binding {}", sock.display()))?;

    let idle_timeout = std::env::var("DEVKIT_DAEMON_IDLE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(1800));

    let max_restarts = env_u32("DEVKIT_DAEMON_MAX_RESTARTS", 5);
    let restart_window = Duration::from_secs(env_u64("DEVKIT_DAEMON_RESTART_WINDOW", 60));
    let mem_warn = env_u64("DEVKIT_DAEMON_MEM_WARN_MB", 0) * 1024 * 1024;
    let mem_limit = env_u64("DEVKIT_DAEMON_MEM_LIMIT_MB", 0) * 1024 * 1024;
    let health_probe = Duration::from_secs(env_u64("DEVKIT_DAEMON_HEALTH_PROBE_SECS", 0));
    let health_fail_threshold = env_u32("DEVKIT_DAEMON_HEALTH_FAIL_THRESHOLD", 3);
    let memory_action =
        std::env::var("DEVKIT_DAEMON_MEMORY_ACTION").unwrap_or_else(|_| "warn".to_string());
    let mem_restart = memory_action == "restart";
    let mem_limit_ticks = env_u32("DEVKIT_DAEMON_MEM_LIMIT_TICKS", 3);
    if mem_restart && mem_limit > 0 && mem_warn > 0 && mem_limit <= mem_warn {
        log_line(&format!(
            "memory: limit ({} MB) is at or below warn ({} MB) — warn threshold is redundant",
            mem_limit / 1024 / 1024,
            mem_warn / 1024 / 1024
        ));
    }

    let mem_max_mb = env_u64("DEVKIT_DAEMON_MEM_MAX_MB", 0);
    let mem_limit_mb = mem_limit / 1024 / 1024;
    if cap_below_soft_limit(mem_max_mb, mem_limit_mb) {
        log_line(&format!(
            "memory: hard cap ({mem_max_mb} MB) at or below soft limit ({mem_limit_mb} MB) — soft restart will never get to act first"
        ));
    }
    let cgroup_cap = if mem_max_mb > 0 {
        match devkit_common::sys::cgroup_caps() {
            devkit_common::sys::CgroupCaps::Enforce { base } => Some(CgroupCap {
                base,
                max_bytes: mem_max_mb * 1024 * 1024,
            }),
            devkit_common::sys::CgroupCaps::Unavailable { reason } => {
                log_line(&format!(
                    "memory: hard cap requested ({mem_max_mb} MB) but cgroup-v2 enforcement unavailable: {reason} — using soft memory_action only"
                ));
                None
            }
            // Off-Linux memory_max_mb is meaningless; stay silent.
            devkit_common::sys::CgroupCaps::Unsupported => None,
        }
    } else {
        None
    };

    let daemon = Arc::new(Daemon {
        last_activity: Mutex::new(Instant::now()),
        active_conns: AtomicUsize::new(0),
        shutdown: AtomicBool::new(false),
        idle_timeout,
        sup: Mutex::new(supervisor::Supervisor::new(
            max_restarts,
            restart_window,
            mem_warn,
            mem_limit,
        )),
        ports,
        locks,
        cgroup_cap,
    });

    // Adopt servers a previous daemon left running: monitor by poll, not waitpid.
    {
        let data = daemon.ports.lock().unwrap().clone();
        let mut sup = daemon.sup.lock().unwrap();
        for (port, e) in &data.entries {
            if let (Some(pid), Some(log)) = (e.pid, e.logfile.clone())
                && registry::pid_alive(pid)
            {
                sup.insert_adopted(
                    supervisor::Key {
                        holder: e.holder.clone(),
                        app: e.app.clone(),
                        role: e.role,
                    },
                    pid,
                    *port,
                    log,
                );
            }
        }
    }

    // Reconcile orphaned cgroup leaves from a previous daemon's unclean exit.
    {
        let live: Vec<supervisor::Key> = daemon.sup.lock().unwrap().adopted_keys();
        cgroup::reconcile(&daemon, &live);
    }

    // Combined supervision thread: reaps exited children, restarts crashed ones,
    // warns on memory breaches, and triggers idle-exit.
    {
        let d = Arc::clone(&daemon);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(500));
                if d.shutdown.load(Ordering::SeqCst) || d.is_idle() {
                    d.shutdown.store(true, Ordering::SeqCst);
                    for sock in [paths::port_socket_file(), paths::lock_socket_file()] {
                        if let Ok(name) = transport::socket_name(&sock) {
                            let _ = Stream::connect(name);
                        }
                    }
                    break;
                }
                // The supervisor table is the authority on crash vs. stop. An intentional
                // `Down` removes the key from the table before stopping the child, so a
                // stopped server never surfaces from `reap_once`; anything reaped exited on
                // its own and is a crash. `restart` enforces the crash-loop budget and drops
                // adopted survivors that have no launch spec. The bound `let` releases the
                // `sup` lock before the loop, so `restart` (which re-locks `sup`) cannot
                // deadlock.
                let dead = d.sup.lock().unwrap().reap_once();
                for key in dead {
                    restart(&d, &key);
                }
                // Memory: warn once per breach (the implemented action is warn-only).
                for (key, rss) in d.sup.lock().unwrap().memory_breaches() {
                    log_line(&format!(
                        "memory: {}/{} ({:?}) tree-RSS {} MB exceeds warn threshold",
                        key.holder,
                        key.app,
                        key.role,
                        rss / 1024 / 1024
                    ));
                }
                // Memory limit: when the action is "restart", SIGTERM a server
                // that has been over the limit for `mem_limit_ticks` consecutive
                // ticks (the reap tick respawns it within the crash-loop budget);
                // once the budget is exhausted, warn and leave it running.
                if mem_restart {
                    for action in d.sup.lock().unwrap().mem_limit_actions(mem_limit_ticks) {
                        match action {
                            supervisor::MemAction::Restart { key, pid, rss } => {
                                log_line(&format!(
                                    "memory: {}/{} ({:?}) tree-RSS {} MB over limit — restarting",
                                    key.holder,
                                    key.app,
                                    key.role,
                                    rss / 1024 / 1024
                                ));
                                devkit_common::supervise::stop(pid);
                            }
                            supervisor::MemAction::GiveUp { key, rss } => {
                                log_line(&format!(
                                    "memory: {}/{} ({:?}) tree-RSS {} MB over limit but crash-loop budget exhausted — leaving alive",
                                    key.holder,
                                    key.app,
                                    key.role,
                                    rss / 1024 / 1024
                                ));
                            }
                        }
                    }
                }
            }
        });
    }

    // Health-probe thread (enabled by DEVKIT_DAEMON_HEALTH_PROBE_SECS > 0): TCP-probe
    // each owned server's port and restart one that was once ready but has stopped
    // accepting. It runs separately from the reap loop so its blocking 300 ms connects
    // never delay reaping or idle-exit, and its only mutations are each child's probe
    // counters and a SIGTERM — the reap tick does the respawn through the crash path,
    // so the two threads never race on restart.
    if !health_probe.is_zero() {
        let d = Arc::clone(&daemon);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(health_probe);
                if d.shutdown.load(Ordering::SeqCst) {
                    break;
                }
                // Snapshot eligible (key, port) under a brief lock, then release it
                // before any connect — a 300 ms probe must never run under `sup`.
                let targets = d.sup.lock().unwrap().probe_targets();
                for (key, port) in targets {
                    let ok = devkit_common::supervise::probe_port(port);
                    // Bind the result so the `sup` guard drops before `stop`. A
                    // returned pid means K consecutive post-arming failures: the
                    // server is hung. SIGTERM it; the reap tick respawns it.
                    let hung = d
                        .sup
                        .lock()
                        .unwrap()
                        .record_probe(&key, ok, health_fail_threshold);
                    if let Some(pid) = hung {
                        log_line(&format!(
                            "health: {}/{} ({:?}) unresponsive — restarting",
                            key.holder, key.app, key.role
                        ));
                        devkit_common::supervise::stop(pid);
                    }
                }
            }
        });
    }

    // Lock control channel — second socket, same process and lifecycle.
    let lock_sock = paths::lock_socket_file();
    let _ = std::fs::remove_file(&lock_sock);
    let lock_name =
        transport::socket_name(&lock_sock).with_context(|| "building lock socket name")?;
    let lock_listener = ListenerOptions::new()
        .name(lock_name)
        .create_sync()
        .with_context(|| format!("binding {}", lock_sock.display()))?;
    {
        let d = Arc::clone(&daemon);
        std::thread::spawn(move || {
            for stream in lock_listener.incoming() {
                if d.shutdown.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let d2 = Arc::clone(&d);
                std::thread::spawn(move || {
                    d2.active_conns.fetch_add(1, Ordering::SeqCst);
                    d2.touch();
                    if let Err(e) = handle_lock_conn(&d2, stream) {
                        log_line(&format!("lock connection error: {e:#}"));
                    }
                    d2.active_conns.fetch_sub(1, Ordering::SeqCst);
                    d2.touch();
                });
            }
        });
    }

    for stream in listener.incoming() {
        if daemon.shutdown.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else { continue };
        let d = Arc::clone(&daemon);
        // A panicking handler would abort the whole daemon (panic=abort), so handlers
        // return Result and we only log failures here.
        std::thread::spawn(move || {
            d.active_conns.fetch_add(1, Ordering::SeqCst);
            d.touch();
            if let Err(e) = handle_conn(&d, stream) {
                log_line(&format!("connection error: {e:#}"));
            }
            d.active_conns.fetch_sub(1, Ordering::SeqCst);
            d.touch();
        });
    }

    // Clean shutdown: drop both sockets and release the lock.
    let _ = std::fs::remove_file(paths::port_socket_file());
    let _ = std::fs::remove_file(paths::lock_socket_file());
    drop(guard);
    Ok(())
}

/// Serve requests on one port-registry connection until EOF or a close-signalling response.
fn handle_conn(daemon: &Arc<Daemon>, stream: Stream) -> Result<()> {
    let (recv, send) = stream.split();
    let mut reader = BufReader::new(recv);
    let mut writer = BufWriter::new(send);
    while let Some(req) = proto::recv::<_, Request>(&mut reader)? {
        daemon.touch();
        let (resp, close) = server::dispatch(daemon, req);
        proto::send(&mut writer, &resp)?;
        if close {
            break;
        }
    }
    Ok(())
}

/// Serve requests on one lock-registry connection until EOF.
///
/// The lock channel has no per-request close or `Shutdown` semantics — it loops
/// until the client closes the connection (EOF). Each frame is dispatched via
/// `framing` directly rather than the port-proto `send`/`recv`.
fn handle_lock_conn(daemon: &Arc<Daemon>, stream: Stream) -> Result<()> {
    use devkit_common::daemon::framing;
    use devkit_locks::daemon::proto::Request as LockRequest;
    let (recv, send) = stream.split();
    let mut reader = BufReader::new(recv);
    let mut writer = BufWriter::new(send);
    while let Some(req) = framing::recv::<_, LockRequest>(&mut reader)? {
        daemon.touch();
        let resp = lock_server::dispatch(daemon, req);
        framing::send(&mut writer, &resp)?;
    }
    Ok(())
}

/// Respawn a crashed child if its crash-loop budget allows; otherwise drop it and log.
fn restart(daemon: &Arc<Daemon>, key: &supervisor::Key) {
    let mut sup = daemon.sup.lock().unwrap();
    // A `Down` (or a give-up) can remove the key between the reap and here. The
    // child is already gone and untracked, so there is nothing to restart — return
    // without logging a spurious drop.
    if !sup.contains(key) {
        return;
    }
    // An adopted survivor has no stored launch spec, so it can't be respawned —
    // drop it on exit rather than charging the crash-loop budget for a spawn that
    // can never happen.
    if sup.launch_of(key).is_none() {
        sup.remove(key);
        drop(sup);
        log_line(&format!(
            "dropping {}/{} ({:?}) — no launch spec to respawn",
            key.holder, key.app, key.role
        ));
        crate::cgroup::remove_leaf(daemon, key);
        return;
    }
    if !sup.may_restart(&key.holder, &key.app, key.role) {
        sup.remove(key);
        drop(sup);
        log_line(&format!(
            "giving up on {}/{} ({:?}) — crash-loop budget exhausted",
            key.holder, key.app, key.role
        ));
        crate::cgroup::remove_leaf(daemon, key);
        return;
    }
    drop(sup);
    log_line(&format!(
        "restart: {}/{} ({:?})",
        key.holder, key.app, key.role
    ));
    daemon.respawn(key);
}

pub(crate) fn log_line(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths::daemon_log())
    {
        let _ = writeln!(f, "{msg}");
    }
}

fn env_u64(k: &str, d: u64) -> u64 {
    std::env::var(k)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(d)
}

fn env_u32(k: &str, d: u32) -> u32 {
    std::env::var(k)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(d)
}

/// Whether a hard cap and a soft limit are both set with the cap at or below the
/// soft limit — a misconfiguration where the soft restart never gets to act first.
fn cap_below_soft_limit(max_mb: u64, limit_mb: u64) -> bool {
    max_mb > 0 && limit_mb > 0 && max_mb <= limit_mb
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn tests_unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static C: AtomicU64 = AtomicU64::new(0);
    (std::process::id() as u64) << 32 | C.fetch_add(1, Ordering::Relaxed)
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn test_daemon_with_base(base: std::path::PathBuf, max_bytes: u64) -> Daemon {
    Daemon {
        last_activity: Mutex::new(Instant::now()),
        active_conns: AtomicUsize::new(0),
        shutdown: AtomicBool::new(false),
        idle_timeout: Duration::from_secs(3600),
        sup: Mutex::new(supervisor::Supervisor::new(
            5,
            Duration::from_secs(60),
            0,
            0,
        )),
        ports: std::sync::Arc::new(std::sync::Mutex::new(registry::Data::default())),
        locks: std::sync::Arc::new(std::sync::Mutex::new(devkit_locks::model::Data::default())),
        cgroup_cap: Some(CgroupCap { base, max_bytes }),
    }
}

#[cfg(test)]
mod tests {
    use super::cap_below_soft_limit;

    #[test]
    fn cap_below_soft_limit_predicate() {
        // Both set and cap <= limit → true (misconfigured).
        assert!(cap_below_soft_limit(4096, 4096));
        assert!(cap_below_soft_limit(2048, 4096));
        // Cap above limit → false (correct ordering).
        assert!(!cap_below_soft_limit(8192, 4096));
        // Either unset (0) → false (not a misconfiguration to warn about).
        assert!(!cap_below_soft_limit(0, 4096));
        assert!(!cap_below_soft_limit(4096, 0));
    }
}
