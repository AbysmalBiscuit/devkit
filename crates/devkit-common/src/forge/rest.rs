//! A JSON REST client for the forges devkit reaches over a plain HTTP API
//! (GitLab, Forgejo), plus a loopback stub server their tests drive it with.

use anyhow::{Context, Result};
use serde_json::Value;

use crate::http::{agent, explain};

const UA: &str = "devkit";

/// One forge API root and the credential sent to it.
#[derive(Debug, Clone)]
pub struct Rest {
    base: String,
    /// `(header, value)`, e.g. `("Authorization", "Bearer <token>")`.
    auth: Option<(&'static str, String)>,
}

/// One page of a list endpoint.
#[derive(Debug)]
pub struct Page {
    pub body: Value,
    /// The next page's number, from GitLab's `X-Next-Page` header or the
    /// `rel="next"` entry of a `Link` header. `None` on the last page.
    pub next: Option<u32>,
}

impl Rest {
    /// `base` is the API root with no trailing slash, such as
    /// `https://gitlab.com/api/v4`.
    pub fn new(base: impl Into<String>, auth: Option<(&'static str, String)>) -> Rest {
        Rest {
            base: base.into().trim_end_matches('/').to_string(),
            auth,
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn request(&self, method: &str, path: &str) -> ureq::Request {
        let mut req = agent()
            .request(method, &format!("{}{path}", self.base))
            .set("User-Agent", UA)
            .set("Accept", "application/json");
        if let Some((header, value)) = &self.auth {
            req = req.set(header, value);
        }
        req
    }

    /// GET `path`. `Ok(None)` on 404, a clean "absent" the caller can act on.
    pub fn get_opt(&self, path: &str) -> Result<Option<Value>> {
        let _span = crate::timing::io_span("forge REST", path).entered();
        match self.request("GET", path).call() {
            Ok(r) => Ok(Some(r.into_json().context("parsing a forge response")?)),
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(e) => Err(explain(e)).with_context(|| format!("GET {path}")),
        }
    }

    /// GET `path`, erroring on 404.
    pub fn get(&self, path: &str) -> Result<Value> {
        self.get_opt(path)?
            .with_context(|| format!("GET {path}: not found"))
    }

    /// GET one page of a list endpoint, with the number of the next page.
    pub fn get_page(&self, path: &str) -> Result<Page> {
        let _span = crate::timing::io_span("forge REST", path).entered();
        let resp = self
            .request("GET", path)
            .call()
            .map_err(explain)
            .with_context(|| format!("GET {path}"))?;
        let next = next_page(resp.header("x-next-page"), resp.header("link"));
        Ok(Page {
            body: resp.into_json().context("parsing a forge response")?,
            next,
        })
    }

    /// Send `body` with `method` (`POST`, `PUT`, `PATCH`) and return the
    /// response body, or `Value::Null` when the forge answers with none.
    pub fn send(&self, method: &str, path: &str, body: &Value) -> Result<Value> {
        let _span = crate::timing::io_span("forge REST", path).entered();
        let resp = self
            .request(method, path)
            .send_json(body)
            .map_err(explain)
            .with_context(|| format!("{method} {path}"))?;
        let text = resp.into_string().context("reading a forge response")?;
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).context("parsing a forge response")
    }
}

/// The next page number from GitLab's `X-Next-Page` (empty on the last page)
/// or, failing that, a `Link: <...?page=N...>; rel="next"` header.
fn next_page(x_next_page: Option<&str>, link: Option<&str>) -> Option<u32> {
    if let Some(n) = x_next_page.and_then(|v| v.trim().parse().ok()) {
        return Some(n);
    }
    link?.split(',').find_map(|entry| {
        let (url, params) = entry.split_once(';')?;
        if !params.contains("rel=\"next\"") {
            return None;
        }
        let url = url.trim().trim_start_matches('<').trim_end_matches('>');
        let query = url.split_once('?')?.1;
        query
            .split('&')
            .find_map(|kv| kv.strip_prefix("page=")?.parse().ok())
    })
}

/// Percent-encode one path segment, such as a GitLab project path used as its
/// id (`group/sub/app` becomes `group%2Fsub%2Fapp`) or a query value.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A loopback HTTP/1.1 server answering from canned responses, for testing a
/// backend over its real transport. Each request is answered by the first
/// route whose method matches and whose path-and-query starts with the
/// route's prefix; an unmatched request gets a 404. Every request is recorded.
#[cfg(any(test, feature = "test-support"))]
pub mod stub {
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
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{stub::Route, *};

    #[test]
    fn the_next_page_comes_from_either_header() {
        assert_eq!(next_page(Some("3"), None), Some(3));
        assert_eq!(next_page(Some(""), None), None);
        let link = r#"<https://x/api/v1/repos/o/r/pulls?page=2&limit=50>; rel="next", <https://x/api/v1/repos/o/r/pulls?page=9&limit=50>; rel="last""#;
        assert_eq!(next_page(None, Some(link)), Some(2));
        assert_eq!(
            next_page(None, Some(r#"<https://x/?page=1>; rel="prev""#)),
            None
        );
    }

    #[test]
    fn a_path_segment_is_percent_encoded() {
        assert_eq!(encode("group/sub/app"), "group%2Fsub%2Fapp");
        assert_eq!(encode("feat/x y"), "feat%2Fx%20y");
        assert_eq!(encode("plain-name_1.0"), "plain-name_1.0");
    }

    #[test]
    fn the_client_sends_auth_and_reads_absence_pages_and_writes() {
        let s = stub::serve(vec![
            Route::new("GET", "/api/thing", 200, r#"{"a":1}"#),
            Route::new("GET", "/api/list", 200, "[1,2]").header("X-Next-Page", "2"),
            Route::new("PUT", "/api/thing", 200, ""),
        ]);
        let rest = Rest::new(
            format!("{}/api", s.url()),
            Some(("PRIVATE-TOKEN", "t0k".into())),
        );
        assert_eq!(rest.get("/thing").unwrap(), json!({"a": 1}));
        assert!(rest.get_opt("/missing").unwrap().is_none());
        let page = rest.get_page("/list").unwrap();
        assert_eq!(page.body, json!([1, 2]));
        assert_eq!(page.next, Some(2));
        assert_eq!(
            rest.send("PUT", "/thing", &json!({"x": true})).unwrap(),
            Value::Null
        );

        let reqs = s.requests();
        assert_eq!(reqs[0].header("PRIVATE-TOKEN"), Some("t0k"));
        let put = reqs.iter().find(|r| r.method == "PUT").unwrap();
        assert_eq!(put.body, r#"{"x":true}"#);
    }
}
