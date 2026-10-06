//! Direct GitHub REST/GraphQL access over [`crate::http::client`], for
//! github.com and GitHub Enterprise Server alike.
//!
//! Auth reuses whatever `gh` already relies on for the host: `GH_TOKEN` or
//! `GITHUB_TOKEN` for github.com, `GH_ENTERPRISE_TOKEN` or
//! `GITHUB_ENTERPRISE_TOKEN` for any other host, else the token
//! `gh auth token --hostname <host>` prints (spawned once per host and cached).
//! No credential is stored by devkit. When no token resolves, [`Api::token`]
//! returns `None` and callers fall back to their `gh` path.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use anyhow::{Context, Result};
pub use reqwest::Method;
use serde_json::Value;

use crate::{
    forge::remote::same_host,
    http::{self, StatusCode, client},
};

const UA: &str = "devkit";

/// The public host, whose API lives on its own `api.` subdomain.
pub const GITHUB_COM: &str = "github.com";

/// Where the GitHub token devkit sends was found. `Env` names the variable so
/// a report can print it; `Gh` means `gh auth token` produced it, which is the
/// only case where gh's active account is also devkit's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    Env(&'static str),
    Gh,
    None,
}

/// The variables `gh` reads a token from for `host`, in its own order.
fn token_vars(host: &str) -> [&'static str; 2] {
    if same_host(host, GITHUB_COM) {
        ["GH_TOKEN", "GITHUB_TOKEN"]
    } else {
        ["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"]
    }
}

fn resolve_token(host: &str) -> (Option<String>, TokenSource) {
    for key in token_vars(host) {
        if let Ok(v) = std::env::var(key) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return (Some(v), TokenSource::Env(key));
            }
        }
    }
    // `--hostname` is explicit: with `GH_HOST` set, an unqualified call
    // returns another host's token, which would then be sent here.
    let gh = crate::cmd::capture("gh", &["auth", "token", "--hostname", host], None)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    match gh {
        Some(v) => (Some(v), TokenSource::Gh),
        None => (None, TokenSource::None),
    }
}

type Resolution = (Option<String>, TokenSource);

/// Token and source for `host`, resolved exactly once per process and host.
/// Both [`Api::token`] and [`Api::token_source`] read this same cache so the
/// two can never disagree about where a token came from.
fn resolved(host: &str) -> &'static Resolution {
    static T: OnceLock<Mutex<HashMap<String, &'static Resolution>>> = OnceLock::new();
    let key = host.to_ascii_lowercase();
    let mut cache = T.get_or_init(Default::default).lock().expect("token cache");
    if let Some(r) = cache.get(&key) {
        return r;
    }
    let r: &'static Resolution = Box::leak(Box::new(resolve_token(host)));
    cache.insert(key, r);
    r
}

/// One GitHub host's API: github.com, or a GitHub Enterprise Server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Api {
    host: String,
}

impl Api {
    pub fn new(host: &str) -> Api {
        Api {
            host: host.to_string(),
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    /// The REST root. GitHub Enterprise Server serves it under `/api/v3` on
    /// its own host rather than on an `api.` subdomain.
    fn rest_base(&self) -> String {
        if let Some(base) = test_api_base() {
            return base;
        }
        if same_host(&self.host, GITHUB_COM) {
            "https://api.github.com".to_string()
        } else {
            format!("https://{}/api/v3", self.host)
        }
    }

    fn graphql_url(&self) -> String {
        if let Some(base) = test_api_base() {
            return format!("{base}/graphql");
        }
        if same_host(&self.host, GITHUB_COM) {
            "https://api.github.com/graphql".to_string()
        } else {
            format!("https://{}/api/graphql", self.host)
        }
    }

    /// The token for this host, or `None` when neither the environment nor
    /// `gh` has one. Callers then use their `gh` fallback.
    pub fn token(&self) -> Option<&'static str> {
        resolved(&self.host).0.as_deref()
    }

    /// Where [`Api::token`] came from, from the same resolution it reads.
    pub fn token_source(&self) -> TokenSource {
        resolved(&self.host).1
    }

    /// How to give devkit a token for this host.
    pub fn token_hint(&self) -> String {
        let [a, b] = token_vars(&self.host);
        format!(
            "set {a}/{b} or run `gh auth login --hostname {}`",
            self.host
        )
    }

    fn bearer(&self) -> Result<String> {
        self.token()
            .map(|t| format!("Bearer {t}"))
            .with_context(|| format!("no GitHub token for {} ({})", self.host, self.token_hint()))
    }

    /// POST a raw GraphQL query, returning the response envelope whole
    /// (`{ "data": ..., "errors": ... }`) with no error handling of its own,
    /// so the caller decides which errors it accepts.
    pub fn graphql_value(&self, query: &str) -> Result<Value> {
        let _span = devkit_timing::io_span("github graphql", "graphql").entered();
        Ok(http::send(
            client()
                .post(self.graphql_url())
                .header("Authorization", self.bearer()?)
                .header("User-Agent", UA)
                .json(&serde_json::json!({ "query": query })),
        )?
        .json()?)
    }

    /// POST a raw GraphQL query. The response envelope is returned whole
    /// (`{ "data": ... }`); a non-empty `errors` array is an error.
    pub fn graphql(&self, query: &str) -> Result<Value> {
        let v = self.graphql_value(query)?;
        if let Some(errors) = v.get("errors").and_then(|e| e.as_array())
            && !errors.is_empty()
        {
            anyhow::bail!("GitHub GraphQL error: {}", graphql_error_message(&v));
        }
        Ok(v)
    }

    /// A GraphQL response whose every error is a `NOT_FOUND` is a successful
    /// partial answer: an aliased batch reports one missing id that way while
    /// returning real data for the rest. Any other error class still fails.
    pub fn graphql_partial(&self, query: &str) -> Result<Value> {
        let v = self.graphql_value(query)?;
        if accepts_partial(&v) {
            return Ok(v);
        }
        anyhow::bail!("GitHub GraphQL error: {}", graphql_error_message(&v));
    }

    /// Send `query` through `with_token` (one of the `graphql*` methods,
    /// whose acceptance rule applies) when this host has a token, else
    /// through [`Api::gh_graphql`], since `gh` may still reach the host.
    pub fn graphql_or_gh(
        &self,
        query: &str,
        with_token: impl FnOnce(&Api, &str) -> Result<Value>,
    ) -> Result<Value> {
        match self.token() {
            Some(_) => with_token(self, query),
            None => self.gh_graphql(query),
        }
    }

    /// Send `query` with `gh api graphql --hostname <host>`, returning what
    /// `gh` prints.
    pub fn gh_graphql(&self, query: &str) -> Result<Value> {
        crate::cmd::gh_json(
            &[
                "api",
                "graphql",
                "--hostname",
                self.host(),
                "-f",
                &format!("query={query}"),
            ],
            ".",
        )
    }

    fn rest_direct(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Option<Value>> {
        let _span = devkit_timing::io_span("github REST", path).entered();
        let mut req = client()
            .request(method, format!("{}{path}", self.rest_base()))
            .header("Authorization", self.bearer()?)
            .header("User-Agent", UA)
            .header("Accept", "application/vnd.github+json");
        if let Some(body) = body {
            req = req.json(body);
        }
        match http::send(req) {
            Ok(r) => Ok(Some(json_or_null(&r.text()?)?)),
            Err(e) if http::status(&e) == Some(StatusCode::NOT_FOUND) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// A REST call to `path` (rooted, such as `/repos/o/r/pulls`), with `body`
    /// as its JSON payload. `Ok(None)` is a 404.
    ///
    /// Direct when this host has a token, else through `gh api`, which may be
    /// authenticated where devkit is not. A GET the direct call cannot make is
    /// retried through `gh`; a write is not, since it may have landed.
    pub fn rest(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Option<Value>> {
        if self.token().is_none() {
            return self.gh_rest(&method, path, body);
        }
        match self.rest_direct(method.clone(), path, body) {
            Err(e) if method == Method::GET => self
                .gh_rest(&method, path, body)
                .map_err(|gh| anyhow::anyhow!("{e:#}; gh: {gh:#}")),
            other => other,
        }
    }

    /// [`Api::rest`] through `gh api --hostname <host>`.
    fn gh_rest(&self, method: &Method, path: &str, body: Option<&Value>) -> Result<Option<Value>> {
        let mut args: Vec<String> = [
            "api",
            "--hostname",
            self.host(),
            "--method",
            method.as_str(),
            path.trim_start_matches('/'),
        ]
        .map(String::from)
        .into();
        args.extend(gh_fields(body)?);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        match crate::cmd::capture("gh", &refs, None) {
            Ok(out) => Ok(Some(json_or_null(&out)?)),
            Err(e) if crate::cmd::failed_stderr(&e).is_some_and(|s| s.contains("(HTTP 404)")) => {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// GET `path` under the REST root, erroring on 404.
    pub fn rest_get(&self, path: &str) -> Result<Value> {
        self.rest_direct(Method::GET, path, None)?
            .context("GitHub returned 404")
    }
}

/// Debug builds only: `DEVKIT_TEST_GITHUB_API` replaces every host's API root,
/// REST and GraphQL alike, so a test can serve the direct calls from loopback.
fn test_api_base() -> Option<String> {
    #[cfg(debug_assertions)]
    if let Ok(base) = std::env::var("DEVKIT_TEST_GITHUB_API")
        && !base.trim().is_empty()
    {
        return Some(base.trim().trim_end_matches('/').to_string());
    }
    None
}

/// A response body as JSON, an empty one (a 204) as `null`.
fn json_or_null(text: &str) -> Result<Value> {
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(text).context("parsing a GitHub response")
}

/// A JSON object payload as `gh api` field flags: `-f` sends a string as
/// given, `-F` a boolean or number as typed, and `key[]` one array element.
/// A value those flags cannot carry (a null, an object, a non-string array
/// item) is an error rather than a field silently left out.
fn gh_fields(body: Option<&Value>) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for (key, value) in body.and_then(Value::as_object).into_iter().flatten() {
        match value {
            Value::String(s) => out.extend(["-f".to_string(), format!("{key}={s}")]),
            Value::Bool(_) | Value::Number(_) => {
                out.extend(["-F".to_string(), format!("{key}={value}")])
            }
            Value::Array(items) => {
                for item in items {
                    let Some(item) = item.as_str() else {
                        anyhow::bail!("`gh api` cannot send {key}[] item {item}");
                    };
                    out.extend(["-f".to_string(), format!("{key}[]={item}")]);
                }
            }
            Value::Null | Value::Object(_) => {
                anyhow::bail!("`gh api` cannot send {key} = {value}")
            }
        }
    }
    Ok(out)
}

fn graphql_error_message(v: &Value) -> &str {
    v.get("errors")
        .and_then(|e| e.as_array())
        .and_then(|e| e.first())
        .and_then(|e| e["message"].as_str())
        .unwrap_or("unknown GraphQL error")
}

/// Whether a GraphQL response envelope is a usable answer: no errors at all,
/// or every error is a `NOT_FOUND` alongside real `data`. An aliased batch
/// reports one missing id this way while returning real data for the rest; an
/// error with no `type`, a mix of `NOT_FOUND` and another error class, or
/// `NOT_FOUND` with `data` absent or null, is still a hard failure.
fn accepts_partial(v: &Value) -> bool {
    match v.get("errors").and_then(|e| e.as_array()) {
        None => true,
        Some(errors) if errors.is_empty() => true,
        Some(errors) => {
            let all_not_found = errors
                .iter()
                .all(|e| e.get("type").and_then(|t| t.as_str()) == Some("NOT_FOUND"));
            all_not_found && v.get("data").is_some_and(|d| !d.is_null())
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn accepts_partial_admits_an_errors_free_response() {
        assert!(accepts_partial(&json!({ "data": { "a": 1 } })));
    }

    #[test]
    fn accepts_partial_admits_an_all_not_found_response_with_data() {
        assert!(accepts_partial(&json!({
            "data": { "repository": { "i0": { "state": "CLOSED" }, "i1": null } },
            "errors": [{ "type": "NOT_FOUND", "path": ["repository", "i1"] }]
        })));
    }

    #[test]
    fn accepts_partial_rejects_a_mix_of_not_found_and_another_error() {
        assert!(!accepts_partial(&json!({
            "data": { "a": 1 },
            "errors": [
                { "type": "NOT_FOUND", "path": ["repository", "i1"] },
                { "type": "FORBIDDEN", "path": ["repository", "i0"] }
            ]
        })));
    }

    #[test]
    fn accepts_partial_rejects_an_error_with_no_type() {
        assert!(!accepts_partial(&json!({
            "data": { "a": 1 },
            "errors": [{ "message": "something went wrong" }]
        })));
    }

    #[test]
    fn accepts_partial_rejects_all_not_found_with_null_data() {
        assert!(!accepts_partial(&json!({
            "data": null,
            "errors": [{ "type": "NOT_FOUND", "path": ["repository"] }]
        })));
    }

    #[test]
    fn gh_fields_type_each_value_the_way_gh_api_reads_it() {
        let body = json!({
            "title": "fix: a=b", "draft": false, "reviewers": ["al", "bo"]
        });
        let got = gh_fields(Some(&body)).unwrap().join(" ");
        for want in [
            "-f title=fix: a=b",
            "-F draft=false",
            "-f reviewers[]=al -f reviewers[]=bo",
        ] {
            assert!(got.contains(want), "{want} missing: {got}");
        }
    }

    #[test]
    fn gh_fields_refuse_a_value_gh_api_cannot_carry() {
        for body in [
            json!({ "gone": null }),
            json!({ "nested": { "a": 1 } }),
            json!({ "reviewers": ["al", 7] }),
        ] {
            assert!(gh_fields(Some(&body)).is_err(), "{body}");
        }
    }

    /// GitHub Enterprise Server serves both APIs from its own host, and its
    /// token comes from the enterprise variables `gh` reads for it, so a
    /// github.com token is never sent to it.
    #[test]
    fn an_enterprise_host_has_its_own_endpoints_and_token_variables() {
        let ghe = Api::new("ghe.acme.test");
        assert_eq!(ghe.rest_base(), "https://ghe.acme.test/api/v3");
        assert_eq!(ghe.graphql_url(), "https://ghe.acme.test/api/graphql");
        assert_eq!(token_vars("ghe.acme.test"), [
            "GH_ENTERPRISE_TOKEN",
            "GITHUB_ENTERPRISE_TOKEN"
        ]);

        let dotcom = Api::new("github.com");
        assert_eq!(dotcom.rest_base(), "https://api.github.com");
        assert_eq!(dotcom.graphql_url(), "https://api.github.com/graphql");
        assert_eq!(token_vars("www.github.com"), ["GH_TOKEN", "GITHUB_TOKEN"]);
    }
}
