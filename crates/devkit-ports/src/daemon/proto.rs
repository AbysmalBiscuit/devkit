use crate::registry::{Data, Role};
use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;

/// Wire-format version. Bump on any incompatible change to these types.
pub const PROTO: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    /// Handshake — always the first frame on a connection.
    Ping { proto: u32 },
    // Registry ops (1:1 with the flock facade):
    Alloc { holder: String, reqs: Vec<(String, u16)>, role: Role },
    RecordPid { port: u16, app: String, holder: String, role: Role, pid: u32, logfile: PathBuf },
    Release { holder: String, role: Option<Role> },
    Snapshot,
    Prune,
    // Supervision (daemon-only):
    Supervise {
        holder: String, app: String, role: Role,
        argv: Vec<String>, cwd: String, env: BTreeMap<String, String>,
        logfile: PathBuf, base_port: u16,
    },
    Down { holder: String, role: Option<Role> },
    Tail { holder: String, app: String, role: Option<Role>, lines: usize },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Pong { proto: u32, pid: u32 },
    Ports(Vec<(String, u16)>),
    Snapshot(Data),
    Freed(Vec<u16>),
    /// (port, ready) for each supervised app started by a `Supervise` request.
    Supervised(Vec<(u16, bool)>),
    Lines(String),
    Ok,
    Err(String),
}

/// Write one newline-delimited JSON frame and flush.
pub fn send<W: Write>(w: &mut W, msg: &impl Serialize) -> Result<()> {
    let mut line = serde_json::to_vec(msg)?;
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()?;
    Ok(())
}

/// Read one newline-delimited JSON frame. `Ok(None)` on clean EOF. A blank line
/// (a protocol violation) propagates as a deserialization error.
pub fn recv<R: BufRead, T: DeserializeOwned>(r: &mut R) -> Result<Option<T>> {
    let mut s = String::new();
    if r.read_line(&mut s)? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(s.trim_end())?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Role;

    #[test]
    fn frames_roundtrip_over_a_pipe() {
        let reqs = vec![("api".to_string(), 9100u16)];
        let msg = Request::Alloc { holder: "/w".into(), reqs, role: Role::Issue };
        let mut buf: Vec<u8> = Vec::new();
        send(&mut buf, &msg).unwrap();
        assert_eq!(*buf.last().unwrap(), b'\n', "frame must be newline-terminated");
        let mut rdr = std::io::BufReader::new(&buf[..]);
        let back: Request = recv(&mut rdr).unwrap().expect("one frame");
        match back {
            Request::Alloc { holder, role, .. } => {
                assert_eq!(holder, "/w");
                assert_eq!(role, Role::Issue);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn recv_returns_none_on_eof() {
        let mut rdr = std::io::BufReader::new(&b""[..]);
        let got: Option<Request> = recv(&mut rdr).unwrap();
        assert!(got.is_none());
    }
}
