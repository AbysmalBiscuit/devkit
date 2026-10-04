//! A loopback HTTP/1.1 server answering from canned responses, for testing a
//! backend over its real transport. Each request is answered by the first
//! route whose method matches and whose path-and-query starts with the
//! route's prefix; an unmatched request gets a 404. Every request is recorded.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
};

/// One canned answer.
#[derive(Clone, Debug)]
pub struct Route {
    pub method: &'static str,
    pub prefix: String,
    pub status: u16,
    pub body: String,
    /// Extra response headers, such as `("X-Next-Page", "2")`.
    pub headers: Vec<(String, String)>,
}

impl Route {
    pub fn new(method: &'static str, prefix: &str, status: u16, body: &str) -> Route {
        Route {
            method,
            prefix: prefix.to_string(),
            status,
            body: body.to_string(),
            headers: Vec::new(),
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Route {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

/// One request the stub received.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    /// Path and query, as sent.
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

pub struct Stub {
    pub port: u16,
    log: Arc<Mutex<Vec<Request>>>,
}

impl Stub {
    /// `http://127.0.0.1:<port>`, to which a backend appends its API path.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn requests(&self) -> Vec<Request> {
        self.log.lock().unwrap().clone()
    }
}

/// Serve `routes` on an ephemeral loopback port until the process exits.
pub fn serve(routes: Vec<Route>) -> Stub {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    std::thread::spawn(move || {
        for mut tcp in listener.incoming().flatten() {
            let Some(req) = read_request(&mut tcp) else {
                continue;
            };
            let route = routes
                .iter()
                .find(|r| r.method == req.method && req.path.starts_with(&r.prefix));
            seen.lock().unwrap().push(req);
            let (status, body, headers) = match route {
                Some(r) => (r.status, r.body.clone(), r.headers.clone()),
                None => (404, "{\"message\":\"404 Not Found\"}".into(), Vec::new()),
            };
            let mut head = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for (k, v) in headers {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            head.push_str("\r\n");
            let _ = tcp.write_all(head.as_bytes());
            let _ = tcp.write_all(body.as_bytes());
        }
    });
    Stub { port, log }
}

fn read_request(tcp: &mut std::net::TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(tcp);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).ok()? <= 2 {
            break;
        }
        if let Some((k, v)) = h.trim_end().split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; len];
    reader.read_exact(&mut body).ok()?;
    Some(Request {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}
