//! Daemon client: connects to the supervisor over its local socket.

use crate::daemon::proto::{self, PROTO, Request, Response};
use crate::daemon::transport;
use anyhow::{Context, Result, anyhow};
use devkit_common::paths;
use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{RecvHalf, SendHalf, Stream};
use std::io::{BufReader, BufWriter};
use std::time::{Duration, Instant};

/// A live connection to the daemon. Reusable across requests.
pub struct Client {
    reader: BufReader<RecvHalf>,
    writer: BufWriter<SendHalf>,
}

pub fn handshake_ok(server_proto: u32) -> bool {
    server_proto == PROTO
}

impl Client {
    fn from_stream(stream: Stream) -> Result<Self> {
        let (recv, send) = stream.split();
        let reader = BufReader::new(recv);
        let writer = BufWriter::new(send);
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
    let name = transport::socket_name(&paths::port_socket_file()).ok()?;
    let stream = Stream::connect(name).ok()?;
    Client::from_stream(stream).ok()
}

/// Locate the daemon binary: `$DEVKITD_BIN`, else a sibling of the current
/// executable, else `devkitd` on `PATH`.
fn portd_bin() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("DEVKITD_BIN") {
        return p.into();
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("devkitd");
        if sibling.is_file() {
            return sibling;
        }
    }
    std::path::PathBuf::from("devkitd")
}

/// Connect, autostarting a daemon if none is running. Used by supervision paths
/// (`devrun up --supervise`) — i.e. only when the run gate is on.
pub fn ensure_running() -> Result<Client> {
    if let Some(c) = try_existing() {
        return Ok(c);
    }
    std::process::Command::new(portd_bin())
        .spawn()
        .with_context(|| "spawning devkitd")?;
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
