//! Shared test harness for devkit-portd integration tests.
//!
//! Compile-time unused helpers are expected: different test binaries use different
//! subsets. The allow below is the standard idiom for shared test modules.
#![allow(dead_code)]

use devkit_ports::daemon::proto::{self, Request, Response};
use devkit_ports::daemon::transport;
use interprocess::local_socket::Stream;
use interprocess::local_socket::traits::Stream as _;
use std::io::{BufReader, BufWriter};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Return a globally unique u64 for this process run — used to generate
/// collision-free temp-dir names without wall-clock time or rand.
pub fn unique() -> u64 {
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    (std::process::id() as u64) << 32 | seq
}

/// Bind a TCP listener on an OS-assigned port, read the port, then drop
/// the listener so the port is available for the next caller.
pub fn free_port() -> u16 {
    let l = TcpListener::bind(("127.0.0.1", 0)).expect("bind for free_port");
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

/// Parse a `ports.json` body (as written by `registry::Data`) and return the
/// `pid` for the entry whose `app` field matches `app_name`.
///
/// ports.json shape:
/// ```json
/// { "version": 1, "entries": { "<port>": { "app": "...", "holder": "...",
///   "role": "...", "pid": <u32|null>, "logfile": "...", "ts": <u64> } } }
/// ```
/// `pid` is `Option<u32>` serialised as `null` or a number.
pub fn pid_in_ports_json(body: &str, app_name: &str) -> Option<u32> {
    if body.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let entries = v.get("entries")?.as_object()?;
    for (_port_key, entry) in entries {
        if entry.get("app")?.as_str()? == app_name {
            return entry.get("pid")?.as_u64().map(|p| p as u32);
        }
    }
    None
}

/// A running `devkit-portd` instance bound to a throwaway HOME directory.
pub struct Harness {
    pub home: PathBuf,
    /// `$XDG_STATE_HOME` passed to the daemon — state lives under `<xdg_state>/devkit/`.
    pub xdg_state: PathBuf,
    child: Child,
}

impl Harness {
    /// Start a daemon with idle timeout of 3600 s (effectively never idle).
    pub fn start() -> Self {
        Self::start_with_idle(3600)
    }

    /// Start a daemon that idle-exits after `idle_secs` seconds of inactivity.
    pub fn start_with_idle(idle_secs: u64) -> Self {
        let home = std::env::temp_dir().join(format!("portd-test-{}", unique()));
        // XDG_STATE_HOME is set explicitly so the daemon's state_dir() resolves
        // to a path inside the throwaway temp dir, even when the real user's
        // XDG_STATE_HOME env var is set in the surrounding shell.
        let xdg_state = home.join("state");
        std::fs::create_dir_all(xdg_state.join("devkit/logs")).expect("create test HOME dirs");

        let bin = env!("CARGO_BIN_EXE_devkit-portd");
        let child = Command::new(bin)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", &xdg_state)
            .env("DEVKIT_DAEMON_IDLE_SECS", idle_secs.to_string())
            // Suppress the daemon from touching the real HOME's registry.
            .env("DEVKIT_PORTD_SELF", "1")
            .spawn()
            .expect("spawn devkit-portd");

        let h = Harness {
            home,
            xdg_state,
            child,
        };
        h.wait_for_socket(Duration::from_secs(5));
        h
    }

    /// Path of the unix socket the daemon binds.
    pub fn socket(&self) -> PathBuf {
        self.xdg_state.join("devkit/portd.sock")
    }

    /// Poll until the daemon endpoint accepts a connection, or panic.
    fn wait_for_socket(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let sock = self.socket();
        loop {
            if self.connect().is_some() {
                return;
            }
            if Instant::now() >= deadline {
                panic!("devkit-portd socket never came up at {}", sock.display());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Best-effort connect to the daemon endpoint for this harness.
    fn connect(&self) -> Option<Stream> {
        let name = transport::socket_name(&self.socket()).ok()?;
        Stream::connect(name).ok()
    }

    /// Open a fresh connection, send one request, receive one response, close.
    pub fn request(&self, req: &Request) -> Response {
        let stream = self.connect().expect("connect to portd socket");
        let (recv, send) = stream.split();
        let mut writer = BufWriter::new(send);
        let mut reader = BufReader::new(recv);
        proto::send(&mut writer, req).expect("send request");
        proto::recv::<_, Response>(&mut reader)
            .expect("recv response")
            .expect("EOF before response")
    }

    /// True if the daemon endpoint no longer accepts connections (it has exited).
    pub fn socket_gone(&self) -> bool {
        self.connect().is_none()
    }

    /// Send `Request::Shutdown` and wait for the daemon to exit.
    pub fn shutdown(&mut self) {
        // Ignore errors — the daemon may have already exited (e.g. idle-exit test).
        if let Some(stream) = self.connect() {
            let (recv, send) = stream.split();
            let mut writer = BufWriter::new(send);
            let mut reader = BufReader::new(recv);
            let _ = proto::send(&mut writer, &Request::Shutdown);
            let _ = proto::recv::<_, Response>(&mut reader);
        }
        let _ = self.child.wait();
    }

    /// Read the ports.json content, or empty string if absent.
    pub fn ports_json(&self) -> String {
        let path = self.xdg_state.join("devkit/ports.json");
        std::fs::read_to_string(&path).unwrap_or_default()
    }

    /// Wait up to `timeout` for the daemon process to exit (by polling `socket_gone`
    /// and `child.try_wait`).  Returns true if it exited within the deadline.
    pub fn wait_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(Some(_)) = self.child.try_wait() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Best-effort cleanup — kill the child (already exited in happy paths),
        // then remove the throwaway HOME.
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Convenience: path to the daemon binary under test (resolved at compile time).
pub fn daemon_bin() -> &'static str {
    env!("CARGO_BIN_EXE_devkit-portd")
}
