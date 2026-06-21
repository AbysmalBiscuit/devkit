//! Generic daemon client: a reusable connection over a local socket, plus
//! connect/spawn helpers. Handshake (Ping/Pong/proto) is registry-specific and
//! lives in each consumer's thin wrapper.

use crate::daemon::framing;
use crate::daemon::transport;
use anyhow::{Context, Result, anyhow};
use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{RecvHalf, SendHalf, Stream};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::{BufReader, BufWriter};
use std::path::Path;

/// A live connection to a daemon. Reusable across requests.
pub struct Client {
    reader: BufReader<RecvHalf>,
    writer: BufWriter<SendHalf>,
}

impl Client {
    /// Send one request frame, read one response frame.
    pub fn request<Req: Serialize, Resp: DeserializeOwned>(&mut self, req: &Req) -> Result<Resp> {
        framing::send(&mut self.writer, req)?;
        framing::recv(&mut self.reader)?.ok_or_else(|| anyhow!("daemon closed connection"))
    }
}

/// Connect to the socket at `path` without any handshake. `None` if nothing is
/// listening (no daemon, or a stale socket file).
pub fn connect(path: &Path) -> Option<Client> {
    let name = transport::socket_name(path).ok()?;
    let stream = Stream::connect(name).ok()?;
    let (recv, send) = stream.split();
    Some(Client {
        reader: BufReader::new(recv),
        writer: BufWriter::new(send),
    })
}

/// Spawn the daemon binary at `bin` (it backgrounds itself by taking its lock and
/// binding sockets). Callers poll `connect` until it answers.
pub fn spawn(bin: &Path) -> Result<()> {
    std::process::Command::new(bin)
        .spawn()
        .with_context(|| format!("spawning {}", bin.display()))?;
    Ok(())
}
