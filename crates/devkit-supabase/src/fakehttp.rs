//! A loopback HTTP/1.1 server answering scripted JSON responses in order,
//! for testing a Supabase client over its real transport. Every request is
//! recorded; one that comes after the script runs out gets a 500.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
};

use serde_json::Value;

/// One request the server received.
#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    /// Path and query, as sent.
    pub path_and_query: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Recorded {
    /// The value of header `name`, matched without regard to case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The body, parsed as JSON.
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

/// A server on `127.0.0.1` that answers until the process exits.
pub struct FakeServer {
    port: u16,
    log: Arc<Mutex<Vec<Recorded>>>,
}

impl FakeServer {
    /// Serves `answers`, a status and a JSON body each, one per request in
    /// order.
    pub fn start(answers: Vec<(u16, Value)>) -> FakeServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let log = Arc::new(Mutex::new(Vec::new()));
        let seen = log.clone();
        std::thread::spawn(move || {
            let mut answers = answers.into_iter();
            for mut tcp in listener.incoming().flatten() {
                let Some(request) = read_request(&mut tcp) else {
                    continue;
                };
                seen.lock().unwrap().push(request);
                let (status, body) = answers
                    .next()
                    .unwrap_or((500, serde_json::json!({"message": "no scripted answer"})));
                let body = body.to_string();
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = tcp.write_all(head.as_bytes());
                let _ = tcp.write_all(body.as_bytes());
            }
        });
        FakeServer { port, log }
    }

    /// `http://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<Recorded> {
        self.log.lock().unwrap().clone()
    }
}

fn read_request(tcp: &mut TcpStream) -> Option<Recorded> {
    let mut reader = BufReader::new(tcp);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path_and_query = parts.next()?.to_string();
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? <= 2 {
            break;
        }
        if let Some((k, v)) = header.trim_end().split_once(':') {
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
    Some(Recorded {
        method,
        path_and_query,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}
