//! Daemon client: connects to the supervisor over its Unix socket.

use crate::daemon::proto::{self, Request, Response, PROTO};
use anyhow::{anyhow, Context, Result};
use devkit_common::paths;
use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

/// A live connection to the daemon. Reusable across requests.
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: BufWriter<UnixStream>,
}

pub fn handshake_ok(server_proto: u32) -> bool {
    server_proto == PROTO
}

impl Client {
    fn from_stream(stream: UnixStream) -> Result<Self> {
        let reader = BufReader::new(stream.try_clone()?);
        let writer = BufWriter::new(stream);
        let mut c = Client { reader, writer };
        // Handshake: a proto mismatch means an old daemon survived a binary upgrade —
        // ask it to shut down so the caller can start a fresh one.
        match c.request(&Request::Ping { proto: PROTO })? {
            Response::Pong { proto, .. } if handshake_ok(proto) => Ok(c),
            Response::Pong { .. } => {
                let _ = c.request(&Request::Shutdown);
                Err(anyhow!("daemon proto mismatch"))
            }
            other => Err(anyhow!("unexpected handshake response: {other:?}")),
        }
    }

    /// Send one request, read one response.
    pub fn request(&mut self, req: &Request) -> Result<Response> {
        proto::send(&mut self.writer, req)?;
        proto::recv(&mut self.reader)?.ok_or_else(|| anyhow!("daemon closed connection"))
    }
}

/// Connect to an already-running daemon. Returns `None` if none is up or the
/// handshake fails — never autostarts. Used for opportunistic registry routing
/// (and by `status`, which must never spin a daemon up).
pub fn try_existing() -> Option<Client> {
    let stream = UnixStream::connect(paths::socket_file()).ok()?;
    Client::from_stream(stream).ok()
}

/// Locate the daemon binary: `$DEVKIT_PORTD_BIN`, else a sibling of the current
/// executable, else `devkit-portd` on `PATH`.
fn portd_bin() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("DEVKIT_PORTD_BIN") {
        return p.into();
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("devkit-portd");
        if sibling.is_file() {
            return sibling;
        }
    }
    std::path::PathBuf::from("devkit-portd")
}

/// Connect, autostarting a daemon if none is running. Used by supervision paths
/// (`devrun up --supervise`) — i.e. only when the run gate is on.
pub fn ensure_running() -> Result<Client> {
    if let Some(c) = try_existing() {
        return Ok(c);
    }
    std::process::Command::new(portd_bin())
        .spawn()
        .with_context(|| "spawning devkit-portd")?;
    // Poll the socket until the daemon accepts (it binds after taking its lock).
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(c) = try_existing() {
            return Ok(c);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(anyhow!("daemon did not come up within 5s"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proto_match_decision() {
        assert!(handshake_ok(crate::daemon::proto::PROTO));
        assert!(!handshake_ok(crate::daemon::proto::PROTO + 1));
    }
}
