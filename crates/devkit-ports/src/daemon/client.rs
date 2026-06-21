//! Port daemon client: connect to the supervisor over `ports.sock`, with the
//! port-proto handshake layered on the shared `Client`.

use crate::daemon::proto::{PROTO, Request, Response};
use anyhow::{Result, anyhow};
use devkit_common::daemon::{self, Client};
use devkit_common::paths;
use std::time::{Duration, Instant};

pub fn handshake_ok(server_proto: u32) -> bool {
    server_proto == PROTO
}

/// Validate a fresh connection with the port Ping/Pong handshake. A proto
/// mismatch (old daemon survived an upgrade) asks it to shut down and fails.
fn shake(mut c: Client) -> Option<Client> {
    match c.request::<Request, Response>(&Request::Ping { proto: PROTO }) {
        Ok(Response::Pong { proto, .. }) if handshake_ok(proto) => Some(c),
        Ok(Response::Pong { .. }) => {
            let _ = c.request::<Request, Response>(&Request::Shutdown);
            None
        }
        _ => None,
    }
}

/// Connect to an already-running daemon; `None` if none is up or the handshake
/// fails. Never autostarts.
pub fn try_existing() -> Option<Client> {
    shake(daemon::connect(&paths::port_socket_file())?)
}

/// Locate the daemon binary: `$DEVKITD_BIN`, else a sibling of the current exe,
/// else `devkitd` on `PATH`.
fn devkitd_bin() -> std::path::PathBuf {
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

/// Connect, autostarting a daemon if none is running (supervision paths only).
pub fn ensure_running() -> Result<Client> {
    if let Some(c) = try_existing() {
        return Ok(c);
    }
    daemon::spawn(&devkitd_bin())?;
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
        assert!(handshake_ok(PROTO));
        assert!(!handshake_ok(PROTO + 1));
    }
}
