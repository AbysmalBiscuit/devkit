//! devkit-portd — optional supervisor daemon. Single-instance (portd.lock),
//! binds a unix socket, serves one request per line. This entry point owns
//! lifecycle: lock, bind, accept, idle-exit; supervision and registry dispatch
//! live in submodules.

use anyhow::{Context, Result};
use devkit_common::paths;
use devkit_ports::daemon::proto::{self, Request, Response, PROTO};
use fd_lock::RwLock;
use std::fs::OpenOptions;
use std::io::{BufReader, BufWriter};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Shared daemon state, accessed from the connection threads and the idle watcher.
pub(crate) struct Daemon {
    pub(crate) last_activity: Mutex<Instant>,
    pub(crate) active_conns: AtomicUsize,
    pub(crate) shutdown: AtomicBool,
    pub(crate) idle_timeout: Duration,
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
        false
    }
}

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("devkit-portd");
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

    let daemon = Arc::new(Daemon {
        last_activity: Mutex::new(Instant::now()),
        active_conns: AtomicUsize::new(0),
        shutdown: AtomicBool::new(false),
        idle_timeout,
    });

    // Idle-exit watcher: unblock the accept loop by connecting to ourselves.
    {
        let d = Arc::clone(&daemon);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            if d.shutdown.load(Ordering::SeqCst) || d.is_idle() {
                d.shutdown.store(true, Ordering::SeqCst);
                let _ = UnixStream::connect(paths::socket_file()); // wake accept()
                break;
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
/// Only Ping and Shutdown are answered; other requests return a not-implemented error.
fn handle_conn(daemon: &Arc<Daemon>, stream: UnixStream) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = BufWriter::new(stream);
    while let Some(req) = proto::recv::<_, Request>(&mut reader)? {
        daemon.touch();
        let (resp, close) = dispatch(daemon, req);
        proto::send(&mut writer, &resp)?;
        if close {
            break;
        }
    }
    Ok(())
}

/// Map a request to `(response, should_close)`. Only Ping and Shutdown are
/// handled; all other variants return `Response::Err`.
fn dispatch(daemon: &Arc<Daemon>, req: Request) -> (Response, bool) {
    match req {
        Request::Ping { .. } => (Response::Pong { proto: PROTO, pid: std::process::id() }, false),
        Request::Shutdown => {
            daemon.shutdown.store(true, Ordering::SeqCst);
            let _ = UnixStream::connect(paths::socket_file());
            (Response::Ok, true)
        }
        _ => (Response::Err("not implemented".into()), false),
    }
}

fn log_line(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(paths::daemon_log()) {
        let _ = writeln!(f, "{msg}");
    }
}
