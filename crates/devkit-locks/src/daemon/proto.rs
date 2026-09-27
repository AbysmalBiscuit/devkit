//! Lock-registry wire protocol. Payloads carry context the daemon cannot
//! resolve itself (project root, holder, anchor pid); the daemon stamps `now`.

use serde::{Deserialize, Serialize};

use crate::model::{AcquireOutcome, Conflict, LockEntry, Refusal};

/// Wire-format version, independent of the port proto. Bump on any incompatible
/// change.
pub const PROTO: u32 = 4;

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
        prune: bool,
    },
    CheckCovering {
        root: String,
        holder: String,
        dirs: Vec<String>,
    },
    Release {
        root: String,
        holder: String,
        paths: Vec<String>,
        force: bool,
    },
    ReleaseAll {
        /// The caller's checkout. Release ignores it and frees the holder in
        /// every root; it stays so an older daemon still parses the request.
        root: String,
        holder: String,
    },
    Status {
        root: String,
        all: bool,
    },
    Prune,
    WriteDecide {
        root: String,
        holder: String,
        path: String,
        pid: Option<u32>,
        note: Option<String>,
        ttl: u64,
    },
    ReleasePrefix {
        prefix: String,
    },
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
        refused: Vec<Refusal>,
    },
    Freed(Vec<String>),
    Locks(Vec<LockEntry>),
    Pruned(usize),
    WriteDecided(crate::model::WriteDecision),
    Ok,
    Err(String),
}

#[cfg(test)]
mod tests {
    use devkit_common::daemon::framing::{recv, send};

    use super::*;

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

    /// A daemon one release behind parses this exact shape.
    #[test]
    fn release_all_keeps_its_wire_shape() {
        let msg = Request::ReleaseAll {
            root: "/repo".into(),
            holder: "alice".into(),
        };
        let wire = serde_json::to_value(&msg).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({ "ReleaseAll": { "root": "/repo", "holder": "alice" } })
        );
        let back: Request = serde_json::from_value(wire).unwrap();
        assert!(matches!(
            back,
            Request::ReleaseAll { root, holder } if root == "/repo" && holder == "alice"
        ));
    }
}
