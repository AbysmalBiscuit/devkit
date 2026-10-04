//! A JSON REST client for the forges devkit reaches over a plain HTTP API
//! (GitLab, Forgejo).

use anyhow::{Context, Result};
pub use reqwest::Method;
use reqwest::blocking::RequestBuilder;
use serde_json::Value;

use crate::http::{self, StatusCode, client};

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

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let mut req = client()
            .request(method, format!("{}{path}", self.base))
            .header("User-Agent", UA)
            .header("Accept", "application/json");
        if let Some((header, value)) = &self.auth {
            req = req.header(*header, value);
        }
        req
    }

    /// GET `path`. `Ok(None)` on 404, a clean "absent" the caller can act on.
    pub fn get_opt(&self, path: &str) -> Result<Option<Value>> {
        let _span = devkit_timing::io_span("forge REST", path).entered();
        match http::send(self.request(Method::GET, path)) {
            Ok(r) => Ok(Some(r.json().context("parsing a forge response")?)),
            Err(e) if http::status(&e) == Some(StatusCode::NOT_FOUND) => Ok(None),
            Err(e) => Err(e).with_context(|| format!("GET {path}")),
        }
    }

    /// GET `path`, erroring on 404.
    pub fn get(&self, path: &str) -> Result<Value> {
        self.get_opt(path)?
            .with_context(|| format!("GET {path}: not found"))
    }

    /// GET one page of a list endpoint, with the number of the next page.
    pub fn get_page(&self, path: &str) -> Result<Page> {
        let _span = devkit_timing::io_span("forge REST", path).entered();
        let resp =
            http::send(self.request(Method::GET, path)).with_context(|| format!("GET {path}"))?;
        let header = |name| resp.headers().get(name).and_then(|v| v.to_str().ok());
        let next = next_page(header("x-next-page"), header("link"));
        Ok(Page {
            body: resp.json().context("parsing a forge response")?,
            next,
        })
    }

    /// Send `body` with `method` (`POST`, `PUT`, `PATCH`) and return the
    /// response body, or `Value::Null` when the forge answers with none.
    pub fn send(&self, method: Method, path: &str, body: &Value) -> Result<Value> {
        let _span = devkit_timing::io_span("forge REST", path).entered();
        let resp = http::send(self.request(method.clone(), path).json(body))
            .with_context(|| format!("{method} {path}"))?;
        let text = resp.text().context("reading a forge response")?;
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::http::stub::{self, Route};

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
            rest.send(Method::PUT, "/thing", &json!({"x": true}))
                .unwrap(),
            Value::Null
        );

        let reqs = s.requests();
        assert_eq!(reqs[0].header("PRIVATE-TOKEN"), Some("t0k"));
        let put = reqs.iter().find(|r| r.method == "PUT").unwrap();
        assert_eq!(put.body, r#"{"x":true}"#);
    }
}
