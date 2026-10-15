//! A Linear GraphQL endpoint in a thread, for `DEVKIT_TEST_LINEAR_URL`: it
//! answers each POST by the first registered substring its query or body
//! contains, with a status and body or by hanging up, and records every body
//! it was sent. Like Linear it refuses a query whose [`complexity`] passes
//! [`MAX_COMPLEXITY`], so a list nested in a list of full pages fails here
//! as it would there.
#![allow(dead_code)]

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
};

/// An HTTP status and body, or `None` to hang up without answering.
type Reply = Option<(u16, String)>;

#[derive(Default)]
struct Shared {
    answers: Mutex<Vec<(String, Reply)>>,
    bodies: Mutex<Vec<serde_json::Value>>,
}

pub struct LinearFake {
    pub url: String,
    shared: Arc<Shared>,
}

impl LinearFake {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/graphql", listener.local_addr().unwrap());
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

    /// Answer a request whose body contains `needle` with `body`, replacing
    /// an earlier answer for the same `needle`.
    pub fn answer(&self, needle: &str, body: &str) {
        self.register(needle, Some((200, body.to_string())));
    }

    /// Answer a request whose body contains `needle` with HTTP `status` and
    /// `body`, replacing an earlier answer for the same `needle`.
    pub fn answer_status(&self, needle: &str, status: u16, body: &str) {
        self.register(needle, Some((status, body.to_string())));
    }

    /// Read a request whose body contains `needle`, then close the
    /// connection without answering, replacing an earlier answer for it.
    pub fn hang_up(&self, needle: &str) {
        self.register(needle, None);
    }

    fn register(&self, needle: &str, reply: Reply) {
        let mut answers = self.shared.answers.lock().unwrap();
        match answers.iter_mut().find(|(n, _)| n == needle) {
            Some(entry) => entry.1 = reply,
            None => answers.push((needle.to_string(), reply)),
        }
    }

    /// Every request body, oldest first.
    pub fn bodies(&self) -> Vec<serde_json::Value> {
        self.shared.bodies.lock().unwrap().clone()
    }

    /// The bodies whose query contains `needle`.
    pub fn sent(&self, needle: &str) -> Vec<serde_json::Value> {
        self.bodies()
            .into_iter()
            .filter(|b| b["query"].as_str().is_some_and(|q| q.contains(needle)))
            .collect()
    }

    /// The environment that points devkit at this fake with a key.
    pub fn env(&self) -> [(&str, &str); 2] {
        [
            ("DEVKIT_TEST_LINEAR_URL", &self.url),
            ("LINEAR_API_KEY", "lin_test"),
        ]
    }
}

/// The most a query may cost before Linear refuses it.
pub const MAX_COMPLEXITY: u64 = 10_000;

/// A query's cost as Linear counts its lists: each list costs its page size
/// (`first:`) times the page sizes of the lists it sits in. Linear also
/// charges for each field and object, which a list's count dwarfs.
pub fn complexity(query: &str) -> u64 {
    let mut scale = vec![1u64];
    let mut page = 1u64;
    let mut total = 0;
    let mut chars = query.chars();
    while let Some(c) = chars.next() {
        match c {
            '(' => {
                let args: String = chars.by_ref().take_while(|&c| c != ')').collect();
                if let Some(n) = args.split_once("first:").and_then(|(_, rest)| {
                    let digits: String = rest
                        .trim_start()
                        .chars()
                        .take_while(char::is_ascii_digit)
                        .collect();
                    digits.parse().ok()
                }) {
                    page = n;
                }
            }
            '{' => {
                let outer = scale.last().copied().unwrap_or(1);
                if page > 1 {
                    total += outer * page;
                }
                scale.push(outer * page);
                page = 1;
            }
            '}' => {
                scale.pop();
            }
            _ => {}
        }
    }
    total
}

/// What Linear answers a query nothing was registered for: for a read of an
/// issue by its identifier, the error it gives an identifier no issue has.
fn unanswered(query: &str) -> String {
    let alias = query
        .split_once(": issue(id: ")
        .and_then(|(head, _)| head.rsplit([' ', '{']).next());
    match alias {
        Some(alias) => serde_json::json!({
            "data": null,
            "errors": [{
                "message": "Entity not found: Issue",
                "path": [alias],
                "extensions": { "code": "INVALID_INPUT", "userPresentableMessage": "Could not find referenced Issue." }
            }]
        })
        .to_string(),
        None => r#"{"errors":[{"message":"linearfake: no answer"}]}"#.into(),
    }
}

fn serve(shared: &Shared, stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut stream = stream;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let mut length = 0;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header)?;
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((k, v)) = header.split_once(':')
                && k.eq_ignore_ascii_case("content-length")
            {
                length = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
        let text = String::from_utf8_lossy(&body).into_owned();
        let parsed: serde_json::Value =
            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        let query = parsed["query"].as_str().unwrap_or_default().to_string();
        shared.bodies.lock().unwrap().push(parsed);
        let cost = complexity(&query);
        let reply = if cost > MAX_COMPLEXITY {
            Some((
                200,
                serde_json::json!({ "errors": [{
                    "message": format!("linearfake: query complexity {cost} exceeds {MAX_COMPLEXITY}")
                }] })
                .to_string(),
            ))
        } else {
            shared
                .answers
                .lock()
                .unwrap()
                .iter()
                .find(|(needle, _)| {
                    query.contains(needle.as_str()) || text.contains(needle.as_str())
                })
                .map(|(_, a)| a.clone())
                .unwrap_or_else(|| Some((200, unanswered(&query))))
        };
        let Some((status, reply)) = reply else {
            return Ok(());
        };
        write!(
            stream,
            "HTTP/1.1 {status} Reply\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{reply}",
            reply.len()
        )?;
        stream.flush()?;
    }
}
