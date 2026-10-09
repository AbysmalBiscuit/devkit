use std::{
    error::Error,
    fmt,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use devkit_common::http;
use reqwest::{
    StatusCode,
    blocking::{RequestBuilder, Response},
};
use serde::{Deserialize, de::DeserializeOwned};

/// The credentials each request carries.
pub enum Auth {
    /// None, for a proxy that attaches them.
    None,
    /// A key, sent as `apikey` and as the bearer token.
    Key(String),
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Auth::None => "None",
            Auth::Key(_) => "Key",
        })
    }
}

/// A Supabase project's Data API, the PostgREST server under `/rest/v1/`,
/// serving one schema. Each request gives up after `wait`, or sooner at a
/// deadline [`Api::finish_by`] sets, and once one gets no answer the
/// requests after it fail at once, so an unreachable API costs one wait.
pub struct Api {
    url: String,
    schema: &'static str,
    auth: Auth,
    wait: Duration,
    /// Names the API in errors, such as "rules API".
    label: &'static str,
    deadline: Mutex<Option<Instant>>,
    /// Why the last request got no answer, once one did not.
    unreachable: Mutex<Option<String>>,
    /// Called each time the API answers 401, for a caller whose
    /// credentials may have gone stale.
    on_rejected: OnceLock<Box<dyn Fn() + Send + Sync>>,
    /// Why every request fails, for a URL that is missing or does not parse.
    unusable: Option<String>,
}

impl fmt::Debug for Api {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Api")
            .field("label", &self.label)
            .field("url", &self.url)
            .field("schema", &self.schema)
            .field("auth", &self.auth)
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

impl Error for Unreachable {}

/// A request the API answered with a status other than success, and the
/// error body PostgREST sends with it.
#[derive(Debug)]
pub struct Refused {
    pub status: StatusCode,
    /// The SQLSTATE or PostgREST code, such as `42501` or `PGRST202`.
    pub code: Option<String>,
    pub message: String,
    pub details: Option<String>,
    label: &'static str,
    url: String,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} answered {}: {}",
            self.label, self.url, self.status, self.message
        )
    }
}

impl Error for Refused {}

/// The error body PostgREST answers a refused request with.
#[derive(Default, Deserialize)]
struct Body {
    code: Option<String>,
    message: Option<String>,
    details: Option<String>,
}

impl Api {
    /// The API of the project whose URL is `url`, such as
    /// `https://<project-ref>.supabase.co`, serving `schema`.
    pub fn new(
        url: &str,
        schema: &'static str,
        auth: Auth,
        wait: Duration,
        label: &'static str,
    ) -> Result<Api> {
        let url = url.trim().trim_end_matches('/');
        let parsed =
            reqwest::Url::parse(url).with_context(|| format!("the {label} URL does not parse"))?;
        ensure!(
            matches!(parsed.scheme(), "https" | "http"),
            "the {label} URL is not http or https: {url}"
        );
        Ok(Api {
            url: url.to_string(),
            schema,
            auth,
            wait,
            label,
            deadline: Mutex::new(None),
            unreachable: Mutex::new(None),
            on_rejected: OnceLock::new(),
            unusable: None,
        })
    }

    /// An API every request to fails with `reason`.
    pub fn unusable(reason: impl fmt::Display, label: &'static str) -> Api {
        Api {
            url: String::new(),
            schema: "",
            auth: Auth::None,
            wait: Duration::ZERO,
            label,
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

    /// What names the API in errors.
    pub fn label(&self) -> &'static str {
        self.label
    }

    /// Makes every request from now on give up by `at`.
    pub fn finish_by(&self, at: Instant) {
        *self.deadline.lock().unwrap_or_else(|e| e.into_inner()) = Some(at);
    }

    /// Runs `f` each time the API answers 401.
    pub fn on_rejected(&self, f: impl Fn() + Send + Sync + 'static) {
        let _ = self.on_rejected.set(Box::new(f));
    }

    /// The rows of `table` that `query` selects.
    pub fn select<T: DeserializeOwned>(
        &self,
        table: &str,
        query: &[(&str, String)],
    ) -> Result<Vec<T>> {
        self.select_page(table, query).map(|(rows, _)| rows)
    }

    /// The rows of `table` that `query` selects, and how many it selects
    /// in all, whatever page of them `query` asks for.
    pub fn select_page<T: DeserializeOwned>(
        &self,
        table: &str,
        query: &[(&str, String)],
    ) -> Result<(Vec<T>, Option<usize>)> {
        let req = http::client()
            .get(self.endpoint(table))
            .query(query)
            .header("Accept-Profile", self.schema)
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
    pub fn call(&self, function: &str, args: &serde_json::Value) -> Result<Response> {
        let req = http::client()
            .post(self.endpoint(&format!("rpc/{function}")))
            .header("Content-Profile", self.schema)
            .json(args);
        self.send(req)
    }

    /// The JSON `resp` carries.
    pub fn body<T: DeserializeOwned>(&self, resp: Response) -> Result<T> {
        resp.json()
            .map_err(http::explain)
            .with_context(|| format!("reading the answer of {} {}", self.label, self.url))
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
        let resp = self.attempt(self.authorized(req)?)?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        Err(self.refusal(resp))
    }

    /// `req` with the credentials [`Auth`] says to send.
    fn authorized(&self, req: RequestBuilder) -> Result<RequestBuilder> {
        Ok(match &self.auth {
            Auth::None => req,
            Auth::Key(key) => req.header("apikey", key).bearer_auth(key),
        })
    }

    /// Sends `req` within the budget, failing at once after an earlier
    /// request got no answer.
    fn attempt(&self, req: RequestBuilder) -> Result<Response> {
        let mut unreachable = self.unreachable.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(reason) = &*unreachable {
            return Err(Unreachable(reason.clone()).into());
        }
        let budget = self.budget();
        if budget.is_zero() {
            return Err(Unreachable(format!("{} {}: no time left", self.label, self.url)).into());
        }
        match req.timeout(budget).send() {
            Ok(resp) => Ok(resp),
            Err(e) => {
                let e = http::explain(e).context(format!("{} {}", self.label, self.url));
                if http::is_unreachable(&e) {
                    *unreachable = Some(format!("{e:#}"));
                }
                Err(e)
            }
        }
    }

    /// The error a refused request reads as, running [`Api::on_rejected`]
    /// on a 401.
    fn refusal(&self, resp: Response) -> anyhow::Error {
        let status = resp.status();
        let body: Body = resp.json().unwrap_or_default();
        if status == StatusCode::UNAUTHORIZED
            && let Some(f) = self.on_rejected.get()
        {
            f();
        }
        Refused {
            status,
            code: body.code,
            message: body.message.unwrap_or_else(|| status.to_string()),
            details: body.details,
            label: self.label,
            url: self.url.clone(),
        }
        .into()
    }
}

/// Whether `e` is a request that got no answer: the host did not resolve,
/// refused, hung up or timed out, now or on an earlier request.
pub fn is_unreachable(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Unreachable>().is_some() || http::is_unreachable(e)
}
