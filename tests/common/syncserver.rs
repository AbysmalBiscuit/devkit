//! A taskchampion sync server in a thread: it keeps each client's sealed
//! versions as opaque bytes and answers the four endpoints taskchampion's
//! client calls. A test can make it hang one request, to hold a client
//! mid-sync.
#![allow(dead_code)]

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use devkit_todo_taskchampion::Uuid;

const NIL: &str = "00000000-0000-0000-0000-000000000000";
const SEGMENT: &str = "application/vnd.taskchampion.history-segment";

#[derive(Default)]
struct Versions {
    latest: Option<String>,
    /// Version ids in the order the server accepted them.
    order: Vec<String>,
    /// Parent id to (child id, sealed bytes).
    children: HashMap<String, (String, Vec<u8>)>,
}

#[derive(Default)]
struct Hang {
    /// `Some(None)` hangs the next child-version request, `Some(Some(p))` the
    /// next one asking for the child of `p`.
    armed: Option<Option<String>>,
    hung: bool,
    released: bool,
}

#[derive(Default)]
struct Shared {
    versions: Mutex<Versions>,
    hang: Mutex<Hang>,
    changed: Condvar,
}

pub struct SyncServer {
    pub url: String,
    shared: Arc<Shared>,
}

impl SyncServer {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let shared = Arc::new(Shared::default());
        let accepting = shared.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let shared = accepting.clone();
                std::thread::spawn(move || {
                    let _ = serve(&shared, stream);
                });
            }
        });
        Self { url, shared }
    }

    /// Version ids, oldest first.
    pub fn versions(&self) -> Vec<String> {
        self.shared.versions.lock().unwrap().order.clone()
    }

    /// Makes the next request for the child of `parent`, or for any child
    /// when `None`, hang until [`SyncServer::release`].
    pub fn hang_on_child_of(&self, parent: Option<&str>) {
        let mut hang = self.shared.hang.lock().unwrap();
        *hang = Hang {
            armed: Some(parent.map(str::to_string)),
            ..Hang::default()
        };
    }

    /// Blocks until a request is hanging, or panics after `limit`.
    pub fn wait_hung(&self, limit: Duration) {
        let hang = self.shared.hang.lock().unwrap();
        let (hang, timeout) = self
            .shared
            .changed
            .wait_timeout_while(hang, limit, |h| !h.hung)
            .unwrap();
        assert!(!timeout.timed_out() && hang.hung, "no request hung");
    }

    /// Lets a hanging request finish normally.
    pub fn release(&self) {
        self.shared.hang.lock().unwrap().released = true;
        self.shared.changed.notify_all();
    }
}

fn serve(shared: &Shared, stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, path) = (
        parts.next().unwrap_or_default().to_string(),
        parts.next().unwrap_or_default().to_string(),
    );
    let mut length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let reply = route(shared, &method, &path, body);
    respond(stream, reply)
}

struct Reply {
    status: &'static str,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}

fn reply(status: &'static str) -> Reply {
    Reply {
        status,
        headers: Vec::new(),
        body: Vec::new(),
    }
}

fn route(shared: &Shared, method: &str, path: &str, body: Vec<u8>) -> Reply {
    let path = path.trim_start_matches('/');
    if let Some(parent) = path.strip_prefix("v1/client/add-version/") {
        let mut v = shared.versions.lock().unwrap();
        let latest = v.latest.clone().unwrap_or_else(|| NIL.to_string());
        if parent != latest {
            let mut r = reply("409 Conflict");
            r.headers.push(("X-Parent-Version-Id", latest));
            return r;
        }
        let id = Uuid::new_v4().to_string();
        v.children.insert(parent.to_string(), (id.clone(), body));
        v.order.push(id.clone());
        v.latest = Some(id.clone());
        let mut r = reply("200 OK");
        r.headers.push(("X-Version-Id", id));
        return r;
    }
    if let Some(parent) = path.strip_prefix("v1/client/get-child-version/") {
        hang_if_armed(shared, parent);
        let v = shared.versions.lock().unwrap();
        return match v.children.get(parent) {
            Some((id, sealed)) => Reply {
                status: "200 OK",
                headers: vec![
                    ("X-Version-Id", id.clone()),
                    ("X-Parent-Version-Id", parent.to_string()),
                    ("Content-Type", SEGMENT.to_string()),
                ],
                body: sealed.clone(),
            },
            None => reply("404 Not Found"),
        };
    }
    match (method, path) {
        ("POST", p) if p.starts_with("v1/client/add-snapshot/") => reply("200 OK"),
        _ => reply("404 Not Found"),
    }
}

fn hang_if_armed(shared: &Shared, parent: &str) {
    let mut hang = shared.hang.lock().unwrap();
    let hits = match &hang.armed {
        Some(None) => true,
        Some(Some(p)) => p == parent,
        None => false,
    };
    if !hits {
        return;
    }
    hang.armed = None;
    hang.hung = true;
    shared.changed.notify_all();
    let _hang = shared.changed.wait_while(hang, |h| !h.released).unwrap();
}

fn respond(mut stream: TcpStream, r: Reply) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        r.status,
        r.body.len()
    );
    for (name, value) in r.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&r.body)?;
    stream.flush()
}

/// A server that accepts connections and never answers, holding each one
/// open until the returned guard drops.
pub struct Silent {
    pub url: String,
    held: Arc<Mutex<Vec<TcpStream>>>,
}

impl Silent {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let held = Arc::new(Mutex::new(Vec::new()));
        let keep = held.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                keep.lock().unwrap().push(stream);
            }
        });
        Self { url, held }
    }

    pub fn connections(&self) -> usize {
        self.held.lock().unwrap().len()
    }
}

impl Drop for Silent {
    fn drop(&mut self) {
        for stream in self.held.lock().unwrap().drain(..) {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// A server that closes every connection as soon as it accepts it, counting
/// them.
pub struct Refusing {
    pub url: String,
    accepts: Arc<Mutex<usize>>,
}

impl Refusing {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let accepts = Arc::new(Mutex::new(0));
        let count = accepts.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                *count.lock().unwrap() += 1;
                drop(stream);
            }
        });
        Self { url, accepts }
    }

    pub fn accepts(&self) -> usize {
        *self.accepts.lock().unwrap()
    }
}
