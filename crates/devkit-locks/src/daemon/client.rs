//! Lock daemon client: connect over `locks.sock` with the lock-proto handshake.
//! `try_existing` only — `lockm` never autostarts the daemon.

use crate::daemon::proto::{PROTO, Request, Response};
use devkit_common::daemon::{self, Client};
use devkit_common::paths;

pub fn handshake_ok(server_proto: u32) -> bool {
    server_proto == PROTO
}

fn shake(mut c: Client) -> Option<Client> {
    match c.request::<Request, Response>(&Request::Ping { proto: PROTO }) {
        Ok(Response::Pong { proto, .. }) if handshake_ok(proto) => Some(c),
        _ => None,
    }
}

/// Connect to an already-running daemon's lock socket; `None` if none is up or
/// the handshake fails. Never autostarts.
pub fn try_existing() -> Option<Client> {
    shake(daemon::connect(&paths::lock_socket_file())?)
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
