//! devkit-portd — optional supervisor daemon. Single-instance (portd.lock),
//! binds a unix socket, serves one request per line. This entry point owns
//! lifecycle: lock, bind, accept, idle-exit; supervision and registry dispatch
//! live in submodules.

use anyhow::{Context, Result};
use devkit_common::paths;
use devkit_ports::daemon::proto::{self, Request};
use devkit_ports::registry;
use fd_lock::RwLock;
use std::fs::OpenOptions;
use std::io::{BufReader, BufWriter};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// A few supervisor accessors back deferred features (memory-limit action, log serving)
// and have no caller yet.
#[allow(dead_code)]
mod supervisor;
mod server;

/// Shared daemon state, accessed from the connection threads and the idle watcher.
pub(crate) struct Daemon {
    pub(crate) last_activity: Mutex<Instant>,
    pub(crate) active_conns: AtomicUsize,
    pub(crate) shutdown: AtomicBool,
    pub(crate) idle_timeout: Duration,
    pub(crate) sup: Mutex<supervisor::Supervisor>,
}

impl Daemon {
    fn touch(&self) {
        *self.last_activity.lock().unwrap() = Instant::now();
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
            log_line(&format!("cannot respawn {}/{} — no launch spec", key.holder, key.app));
            return;
        };
        match devkit_common::supervise::spawn_detached(&launch.argv, &launch.cwd, &launch.env, &log) {
            Ok(pid) => {
                let _ = registry::record_pid(port, &key.app, &key.holder, key.role, pid, log);
                self.sup.lock().unwrap().set_pid(key, pid);
            }
            Err(e) => log_line(&format!("respawn failed for {}/{}: {e:#}", key.holder, key.app)),
        }
    }
}

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("devkit-portd");
    // The daemon's own handlers must use flock, never route back into a daemon
    // (which would be this process connecting to itself).
    unsafe { std::env::set_var("DEVKIT_PORTD_SELF", "1") };
    std::fs::create_dir_all(paths::state_dir())?;
    std::fs::create_dir_all(paths::logs_dir())?;

    // Single-instance: hold portd.lock for the daemon's whole life. If another
    // daemon holds it, exit 0 — autostart races resolve to exactly one winner.
    let lock_path = paths::daemon_lock_file();
    let lock_file = OpenOptions::new().create(true).write(true).truncate(false).open(&lock_path)?;
    let mut lock = RwLock::new(lock_file);
    let guard = match lock.try_write() {
        Ok(g) => g,
        Err(_) => return Ok(()), // another daemon already running
    };

    // Holding the lock, no live daemon owns the socket — clear any stale one and bind.
    let sock = paths::socket_file();
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).with_context(|| format!("binding {}", sock.display()))?;

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
        sup: Mutex::new(supervisor::Supervisor::new(max_restarts, restart_window, mem_warn, mem_limit)),
    });

    // Adopt servers a previous daemon left running: monitor by poll, not waitpid.
    if let Ok(data) = registry::snapshot() {
        let mut sup = daemon.sup.lock().unwrap();
        for (port, e) in &data.entries {
            if let (Some(pid), Some(log)) = (e.pid, e.logfile.clone())
                && registry::pid_alive(pid)
            {
                sup.insert_adopted(
                    supervisor::Key { holder: e.holder.clone(), app: e.app.clone(), role: e.role },
                    pid, *port, log,
                );
            }
        }
    }

    // Combined supervision thread: reaps exited children, restarts crashed ones,
    // warns on memory breaches, and triggers idle-exit.
    {
        let d = Arc::clone(&daemon);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(500));
            if d.shutdown.load(Ordering::SeqCst) || d.is_idle() {
                d.shutdown.store(true, Ordering::SeqCst);
                let _ = UnixStream::connect(paths::socket_file());
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
                let snap = registry::with_lock(|d| Ok(d.clone())).unwrap_or_default();
                for key in dead {
                    let row = snap.entries.values().find(|e|
                        e.holder == key.holder && e.app == key.app && e.role == key.role);
                    match row {
                        Some(_) => restart(&d, &key),
                        None => { d.sup.lock().unwrap().remove(&key); } // intentional stop
                    }
                }
            }
            // Memory: warn once per breach (the implemented action is warn-only).
            for (key, rss) in d.sup.lock().unwrap().memory_breaches() {
                log_line(&format!(
                    "memory: {}/{} ({:?}) tree-RSS {} MB exceeds warn threshold",
                    key.holder, key.app, key.role, rss / 1024 / 1024));
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

    // Clean shutdown: drop the socket and release the lock.
    let _ = std::fs::remove_file(paths::socket_file());
    drop(guard);
    Ok(())
}

/// Serve requests on one connection until EOF or a close-signalling response.
fn handle_conn(daemon: &Arc<Daemon>, stream: UnixStream) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = BufWriter::new(stream);
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

/// Respawn a crashed child if its crash-loop budget allows; otherwise drop it and log.
fn restart(daemon: &Arc<Daemon>, key: &supervisor::Key) {
    let mut sup = daemon.sup.lock().unwrap();
    // An adopted survivor has no stored launch spec, so it can't be respawned —
    // drop it on exit rather than charging the crash-loop budget for a spawn that
    // can never happen.
    if sup.launch_of(key).is_none() {
        sup.remove(key);
        drop(sup);
        log_line(&format!("dropping {}/{} ({:?}) — no launch spec to respawn",
            key.holder, key.app, key.role));
        return;
    }
    if !sup.may_restart(&key.holder, &key.app, key.role) {
        sup.remove(key);
        drop(sup);
        log_line(&format!("giving up on {}/{} ({:?}) — crash-loop budget exhausted",
            key.holder, key.app, key.role));
        return;
    }
    drop(sup);
    log_line(&format!("restart: {}/{} ({:?})", key.holder, key.app, key.role));
    daemon.respawn(key);
}

fn log_line(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(paths::daemon_log()) {
        let _ = writeln!(f, "{msg}");
    }
}

fn env_u64(k: &str, d: u64) -> u64 {
    std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d)
}

fn env_u32(k: &str, d: u32) -> u32 {
    std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d)
}
