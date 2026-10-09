use std::{
    error::Error,
    fmt,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use devkit_common::http;
use reqwest::{
    StatusCode,
    blocking::{RequestBuilder, Response},
};
use serde::{Deserialize, de::DeserializeOwned};

use crate::auth::{self, Sessions};

/// The credentials each request carries.
pub enum Auth {
    /// None, for a proxy that attaches them.
    None,
    /// A key, sent as `apikey` and as the bearer token.
    Key(String),
    /// A signed-in user: the project's publishable key as `apikey`, and the
    /// user's access token as the bearer token, renewed and the request
    /// retried once when the API answers 401.
    User(Arc<Sessions>),
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Auth::None => "None",
            Auth::Key(_) => "Key",
            Auth::User(_) => "User",
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
    /// Held for each request, its sign-in included, so requests made
    /// together wait on an unreachable API once between them.
    turn: Mutex<()>,
    /// Called each time the API answers 401 or Auth refuses the
    /// credentials, for a caller whose credentials may have gone stale.
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
    /// `https://<project-ref>.supabase.co`, serving `schema`. A URL with a
    /// user, password, query or fragment is refused, since the URL appears
    /// in errors, and no error repeats it.
    pub fn new(
        url: &str,
        schema: &'static str,
        auth: Auth,
        wait: Duration,
        label: &'static str,
    ) -> Result<Api> {
        Ok(Api {
            url: project_url(url, label)?,
            schema,
            auth,
            wait,
            label,
            deadline: Mutex::new(None),
            unreachable: Mutex::new(None),
            turn: Mutex::new(()),
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
            turn: Mutex::new(()),
            on_rejected: OnceLock::new(),
            unusable: Some(reason.to_string()),
        }
    }

    /// The project's URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The project's URL without a user, password, query or fragment, so
    /// it names the project and carries no credential.
    pub fn identity(&self) -> String {
        let Ok(mut url) = reqwest::Url::parse(&self.url) else {
            return String::new();
        };
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        url.as_str().trim_end_matches('/').to_string()
    }

    /// What names the API in errors.
    pub fn label(&self) -> &'static str {
        self.label
    }

    /// Makes every request from now on give up by `at`.
    pub fn finish_by(&self, at: Instant) {
        *self.deadline.lock().unwrap_or_else(|e| e.into_inner()) = Some(at);
    }

    /// Runs `f` each time the API answers 401 or Auth refuses the
    /// credentials. A busy or failing Auth server, an answer that does not
    /// read, or a session that cannot be saved does not run it.
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
        let _turn = self.turn.lock().unwrap_or_else(|e| e.into_inner());
        self.reachable()?;
        let retry = match &self.auth {
            Auth::User(_) => req.try_clone(),
            Auth::None | Auth::Key(_) => None,
        };
        let resp = self.attempt(self.authorized(req, None)?)?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        let (Some(retry), Auth::User(sessions), StatusCode::UNAUTHORIZED) =
            (retry, &self.auth, resp.status())
        else {
            return Err(self.refusal(resp));
        };
        self.rejected();
        let token = sessions
            .renew_within(self.reachable()?)
            .map_err(|e| self.noted(e))?;
        let resp = self.attempt(self.authorized(retry, Some(token))?)?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        Err(self.refusal(resp))
    }

    /// `req` with the credentials [`Auth`] says to send: for a user,
    /// `token` when given, else the session's current access token.
    fn authorized(&self, req: RequestBuilder, token: Option<String>) -> Result<RequestBuilder> {
        Ok(match &self.auth {
            Auth::None => req,
            Auth::Key(key) => req.header("apikey", key).bearer_auth(key),
            Auth::User(sessions) => {
                let token = match token {
                    Some(token) => token,
                    None => sessions
                        .access_token_within(self.reachable()?)
                        .map_err(|e| self.signed_out(e))?,
                };
                req.header("apikey", sessions.client().publishable_key())
                    .bearer_auth(token)
            }
        })
    }

    fn rejected(&self) {
        if let Some(f) = self.on_rejected.get() {
            f();
        }
    }

    /// The time the next request, to the API or to Auth, may take, or the
    /// failure an earlier request that got no answer met.
    fn reachable(&self) -> Result<Duration> {
        if let Some(reason) = &*self.unreachable.lock().unwrap_or_else(|e| e.into_inner()) {
            return Err(Unreachable(reason.clone()).into());
        }
        let budget = self.budget();
        if budget.is_zero() {
            return Err(Unreachable(format!("{} {}: no time left", self.label, self.url)).into());
        }
        Ok(budget)
    }

    /// `e`, kept as the reason the requests after it fail at once when it
    /// got no answer.
    fn noted(&self, e: anyhow::Error) -> anyhow::Error {
        if http::is_unreachable(&e) {
            *self.unreachable.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("{e:#}"));
        }
        e
    }

    /// `e`, from a sign-in or refresh that failed, noted as [`Api::noted`]
    /// does, running [`Api::on_rejected`] when Auth refused the credentials.
    fn signed_out(&self, e: anyhow::Error) -> anyhow::Error {
        if auth::is_refused_credentials(&e) {
            self.rejected();
        }
        self.noted(e)
    }

    /// Sends `req` within the budget, failing at once after an earlier
    /// request got no answer.
    fn attempt(&self, req: RequestBuilder) -> Result<Response> {
        let budget = self.reachable()?;
        req.timeout(budget).send().map_err(|e| {
            self.noted(http::explain(e).context(format!("{} {}", self.label, self.url)))
        })
    }

    /// The error a refused request reads as, running [`Api::on_rejected`]
    /// on a 401.
    fn refusal(&self, resp: Response) -> anyhow::Error {
        let status = resp.status();
        let body: Body = resp.json().unwrap_or_default();
        if status == StatusCode::UNAUTHORIZED {
            self.rejected();
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

/// `url` without surrounding space or a trailing `/`, an http or https URL
/// naming a project and nothing else. A user, password, query or fragment
/// is refused, since the URL appears in errors and output and any of them
/// can carry a credential, and no error repeats the URL.
pub(crate) fn project_url(url: &str, label: &str) -> Result<String> {
    let url = url.trim().trim_end_matches('/');
    let parsed =
        reqwest::Url::parse(url).with_context(|| format!("the {label} URL does not parse"))?;
    ensure!(
        parsed.username().is_empty() && parsed.password().is_none(),
        "the {label} URL carries a user or password; credentials go in the publishable key or a sign-in"
    );
    ensure!(
        parsed.query().is_none() && parsed.fragment().is_none(),
        "the {label} URL carries a query or fragment; it names the project alone"
    );
    ensure!(
        matches!(parsed.scheme(), "https" | "http"),
        "the {label} URL is not http or https"
    );
    Ok(url.to_string())
}

/// Whether `e` is a request that got no answer: the host did not resolve,
/// refused, hung up or timed out, now or on an earlier request.
pub fn is_unreachable(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Unreachable>().is_some() || http::is_unreachable(e)
}
