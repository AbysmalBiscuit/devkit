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
mod lock_server;
mod server;
#[allow(dead_code)]
mod supervisor;

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
        devkit_locks::store::MemoryStore::new(self.locks.clone(), devkit_common::paths::locks_file())
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
        match devkit_common::supervise::spawn_detached(&launch.argv, &launch.cwd, &launch.env, &log)
        {
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
                // Reap exited children; restart only those whose ports.json row survives
                // (the cross-tool stop signal). Debounce the read so a concurrent legacy
                // `down` that removes the row just before the exit isn't misread as a crash.
                // Use a raw read (no liveness prune) so a row with a now-dead pid is still
                // visible here — it's the daemon's signal that the exit was a crash, not an
                // intentional stop. `snapshot()` prunes dead-pid rows before we can see them.
                let dead = d.sup.lock().unwrap().reap_once();
                if !dead.is_empty() {
                    std::thread::sleep(Duration::from_millis(200)); // debounce
                    let snap = d.ports.lock().unwrap().clone();
                    for key in dead {
                        let row = snap.entries.values().find(|e| {
                            e.holder == key.holder && e.app == key.app && e.role == key.role
                        });
                        match row {
                            Some(_) => restart(&d, &key),
                            None => {
                                d.sup.lock().unwrap().remove(&key);
                            } // intentional stop
                        }
                    }
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
            }
        });
    }

    // Lock control channel — second socket, same process and lifecycle.
    let lock_sock = paths::lock_socket_file();
    let _ = std::fs::remove_file(&lock_sock);
    let lock_name = transport::socket_name(&lock_sock).with_context(|| "building lock socket name")?;
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
        return;
    }
    if !sup.may_restart(&key.holder, &key.app, key.role) {
        sup.remove(key);
        drop(sup);
        log_line(&format!(
            "giving up on {}/{} ({:?}) — crash-loop budget exhausted",
            key.holder, key.app, key.role
        ));
        return;
    }
    drop(sup);
    log_line(&format!(
        "restart: {}/{} ({:?})",
        key.holder, key.app, key.role
    ));
    daemon.respawn(key);
}

fn log_line(msg: &str) {
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
