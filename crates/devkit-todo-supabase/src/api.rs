use std::{
    fmt,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use devkit_common::http;
use devkit_todo::{Claimed, Holder};
use reqwest::{
    StatusCode,
    blocking::{RequestBuilder, Response},
};
use serde::{Deserialize, de::DeserializeOwned};

/// The SQLSTATE a todo function raises when another holder has the todo in
/// progress, with that holder as the error's detail.
const CLAIMED: &str = "DK001";
/// The SQLSTATE a todo function raises for an id that names no todo, or
/// more than one.
const UNKNOWN_TODO: &str = "DK002";
/// The schema devkit's tables and functions live in.
const SCHEMA: &str = "devkit";

/// A Supabase project's Data API, the PostgREST server under `/rest/v1/`,
/// serving the `devkit` schema. Each request gives up after `wait`, or
/// sooner at a deadline [`Api::finish_by`] sets, and once one gets no answer
/// the requests after it fail at once, so an unreachable API costs one wait.
///
/// With a key, each request carries it as `apikey` and as a bearer token;
/// without one, it carries neither, for a proxy that attaches them.
pub struct Api {
    url: String,
    key: Option<String>,
    wait: Duration,
    deadline: Mutex<Option<Instant>>,
    /// Why the last request got no answer, once one did not.
    unreachable: Mutex<Option<String>>,
    /// Called each time the API answers 401, for a caller whose key may
    /// have gone stale.
    on_rejected: OnceLock<Box<dyn Fn() + Send + Sync>>,
    /// Why every request fails, for a URL that is missing or does not parse.
    unusable: Option<String>,
}

impl fmt::Debug for Api {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Api")
            .field("url", &self.url)
            .field("key", &self.key.as_ref().map(|_| "set"))
            .field("wait", &self.wait)
            .finish()
    }
}

/// An answer no request got, from an earlier request in this process.
#[derive(Debug)]
struct Unreachable(String);

impl fmt::Display for Unreachable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unreachable {}

/// The error body PostgREST answers a refused request with.
#[derive(Default, Deserialize)]
struct Refusal {
    code: Option<String>,
    message: Option<String>,
    details: Option<String>,
}

impl Api {
    /// The API of the project whose URL is `url`, such as
    /// `https://<project-ref>.supabase.co`.
    pub fn new(url: &str, key: Option<String>, wait: Duration) -> Result<Self> {
        let url = url.trim().trim_end_matches('/');
        let parsed = reqwest::Url::parse(url).context("the todo API URL does not parse")?;
        ensure!(
            matches!(parsed.scheme(), "https" | "http"),
            "the todo API URL is not http or https: {url}"
        );
        Ok(Self {
            url: url.to_string(),
            key,
            wait,
            deadline: Mutex::new(None),
            unreachable: Mutex::new(None),
            on_rejected: OnceLock::new(),
            unusable: None,
        })
    }

    /// An API every request to fails with `reason`.
    pub fn unusable(reason: impl fmt::Display) -> Self {
        Self {
            url: String::new(),
            key: None,
            wait: Duration::ZERO,
            deadline: Mutex::new(None),
            unreachable: Mutex::new(None),
            on_rejected: OnceLock::new(),
            unusable: Some(reason.to_string()),
        }
    }

    /// The project's URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Makes every request from now on give up by `at`.
    pub fn finish_by(&self, at: Instant) {
        *self.deadline.lock().unwrap_or_else(|e| e.into_inner()) = Some(at);
    }

    /// Runs `f` each time the API answers 401.
    pub fn on_rejected(&self, f: impl Fn() + Send + Sync + 'static) {
        let _ = self.on_rejected.set(Box::new(f));
    }

    /// Asks the API for one todo, which answers only when it serves
    /// devkit's schema to this caller.
    pub fn check(&self) -> Result<()> {
        self.select::<serde_json::Value>("todos", &[("select", "id".into()), ("limit", "1".into())])
            .map(drop)
    }

    /// The rows of `table` that `query` selects.
    pub(crate) fn select<T: DeserializeOwned>(
        &self,
        table: &str,
        query: &[(&str, String)],
    ) -> Result<Vec<T>> {
        self.select_page(table, query).map(|(rows, _)| rows)
    }

    /// The rows of `table` that `query` selects, and how many it selects
    /// in all, whatever page of them `query` asks for.
    pub(crate) fn select_page<T: DeserializeOwned>(
        &self,
        table: &str,
        query: &[(&str, String)],
    ) -> Result<(Vec<T>, Option<usize>)> {
        let req = http::client()
            .get(self.endpoint(table))
            .query(query)
            .header("Accept-Profile", SCHEMA)
            .header("Prefer", "count=exact");
        let resp = self.send(req)?;
        let total = resp
            .headers()
            .get("Content-Range")
            .and_then(|range| range.to_str().ok())
            .and_then(|range| range.rsplit_once('/'))
            .and_then(|(_, total)| total.parse().ok());
        Ok((self.body(resp)?, total))
    }

    /// Calls the function `function` with the named `args`.
    pub(crate) fn call(&self, function: &str, args: &serde_json::Value) -> Result<Response> {
        let req = http::client()
            .post(self.endpoint(&format!("rpc/{function}")))
            .header("Content-Profile", SCHEMA)
            .json(args);
        self.send(req)
    }

    /// The JSON `resp` carries.
    pub(crate) fn body<T: DeserializeOwned>(&self, resp: Response) -> Result<T> {
        resp.json()
            .map_err(http::explain)
            .with_context(|| format!("reading the answer of todo API {}", self.url))
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/rest/v1/{path}", self.url)
    }

    /// How long the next request may take: its wait, cut short by the
    /// deadline.
    fn budget(&self) -> Duration {
        match *self.deadline.lock().unwrap_or_else(|e| e.into_inner()) {
            Some(at) => self.wait.min(at.saturating_duration_since(Instant::now())),
            None => self.wait,
        }
    }

    fn send(&self, req: RequestBuilder) -> Result<Response> {
        if let Some(reason) = &self.unusable {
            bail!("{reason}");
        }
        let mut unreachable = self.unreachable.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(reason) = &*unreachable {
            return Err(Unreachable(reason.clone()).into());
        }
        let budget = self.budget();
        if budget.is_zero() {
            return Err(Unreachable(format!("todo API {}: no time left", self.url)).into());
        }
        let req = match &self.key {
            Some(key) => req.header("apikey", key).bearer_auth(key),
            None => req,
        };
        let resp = match req.timeout(budget).send() {
            Ok(resp) => resp,
            Err(e) => {
                let e = http::explain(e).context(format!("todo API {}", self.url));
                if http::is_unreachable(&e) {
                    *unreachable = Some(format!("{e:#}"));
                }
                return Err(e);
            }
        };
        drop(unreachable);
        if resp.status().is_success() {
            return Ok(resp);
        }
        Err(self.refusal(resp))
    }

    /// The error a refused request reads as: the store's own refusals as
    /// every store returns them, anything else naming the API.
    fn refusal(&self, resp: Response) -> anyhow::Error {
        let status = resp.status();
        let refusal: Refusal = resp.json().unwrap_or_default();
        match refusal.code.as_deref() {
            Some(CLAIMED) => {
                return Claimed {
                    by: Holder::new(refusal.details.unwrap_or_default()),
                }
                .into();
            }
            Some(UNKNOWN_TODO) => return anyhow!("{}", refusal.message.unwrap_or_default()),
            _ => {}
        }
        if status == StatusCode::UNAUTHORIZED
            && let Some(f) = self.on_rejected.get()
        {
            f();
        }
        let message = refusal.message.unwrap_or_else(|| status.to_string());
        match advice(refusal.code.as_deref()) {
            Some(advice) => anyhow!(
                "todo API {} answered {status}: {message}; {advice}",
                self.url
            ),
            None => anyhow!("todo API {} answered {status}: {message}", self.url),
        }
    }
}

/// What to do about a refusal whose code says the database is not set up
/// for devkit.
fn advice(code: Option<&str>) -> Option<&'static str> {
    Some(match code? {
        "PGRST106" => "expose the `devkit` schema to the Data API",
        "PGRST202" | "PGRST205" | "42P01" | "42883" | "3F000" => {
            "create or update devkit's schema with \
             `DEVKIT_TODO_BACKEND=postgres devkit todo schema update`"
        }
        "42501" => "grant the API's role devkit's tables and functions",
        _ => return None,
    })
}

/// Whether `e` is a request that got no answer: the host did not resolve,
/// refused, hung up or timed out, now or on an earlier request.
pub fn is_unreachable(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Unreachable>().is_some() || http::is_unreachable(e)
}
