//! Lock-registry wire protocol. Payloads carry context the daemon cannot resolve
//! itself (project root, holder, anchor pid); the daemon stamps `now`.

use crate::model::{AcquireOutcome, Conflict, LockEntry};
use serde::{Deserialize, Serialize};

/// Wire-format version, independent of the port proto. Bump on any incompatible change.
pub const PROTO: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Ping {
        proto: u32,
    },
    Acquire {
        root: String,
        holder: String,
        paths: Vec<String>,
        pid: Option<u32>,
        note: Option<String>,
        ttl: u64,
    },
    Check {
        root: String,
        holder: String,
        paths: Vec<String>,
    },
    Release {
        root: String,
        holder: String,
        paths: Vec<String>,
        force: bool,
    },
    ReleaseAll {
        root: String,
        holder: String,
    },
    Status {
        root: String,
        all: bool,
    },
    Prune,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Pong {
        proto: u32,
        pid: u32,
    },
    Acquired(AcquireOutcome),
    Conflicts(Vec<Conflict>),
    Released {
        released: Vec<String>,
        refused: Vec<String>,
    },
    Freed(Vec<String>),
    Locks(Vec<LockEntry>),
    Pruned(usize),
    Ok,
    Err(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use devkit_common::daemon::framing::{recv, send};

    #[test]
    fn acquire_frame_roundtrips() {
        let msg = Request::Acquire {
            root: "/repo".into(),
            holder: "alice".into(),
            paths: vec!["scenes".into()],
            pid: Some(42),
            note: Some("refactor".into()),
            ttl: 1800,
        };
        let mut buf: Vec<u8> = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut rdr = std::io::BufReader::new(&buf[..]);
        let back: Request = recv(&mut rdr).unwrap().expect("one frame");
        match back {
            Request::Acquire {
                root, holder, pid, ..
            } => {
                assert_eq!(root, "/repo");
                assert_eq!(holder, "alice");
                assert_eq!(pid, Some(42));
            }
            _ => panic!("wrong variant"),
        }
    }
}
