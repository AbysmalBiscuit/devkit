use std::{
    fmt,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use devkit_supabase::{Auth, Refused, Response};
use devkit_todo::{Claimed, Holder};
use serde::de::DeserializeOwned;

/// The SQLSTATE a todo function raises when another holder has the todo in
/// progress, with that holder as the error's detail.
const CLAIMED: &str = "DK001";
/// The SQLSTATE a todo function raises for an id that names no todo, or
/// more than one.
const UNKNOWN_TODO: &str = "DK002";
/// The schema devkit's tables and functions live in.
const SCHEMA: &str = "devkit";
/// What names the API in errors.
const LABEL: &str = "todo API";

/// A Supabase project's Data API serving the `devkit` schema. Each request
/// gives up after `wait`, or sooner at a deadline [`Api::finish_by`] sets,
/// and once one gets no answer the requests after it fail at once, so an
/// unreachable API costs one wait.
///
/// With a key, each request carries it as `apikey` and as a bearer token;
/// without one, it carries neither, for a proxy that attaches them.
#[derive(Debug)]
pub struct Api {
    inner: devkit_supabase::Api,
}

impl Api {
    /// The API of the project whose URL is `url`, such as
    /// `https://<project-ref>.supabase.co`.
    pub fn new(url: &str, key: Option<String>, wait: Duration) -> Result<Self> {
        let auth = match key {
            Some(key) => Auth::Key(key),
            None => Auth::None,
        };
        let inner = devkit_supabase::Api::new(url, SCHEMA, auth, wait, LABEL)?;
        Ok(Self { inner })
    }

    /// An API every request to fails with `reason`.
    pub fn unusable(reason: impl fmt::Display) -> Self {
        Self {
            inner: devkit_supabase::Api::unusable(reason, LABEL),
        }
    }

    /// The project's URL.
    pub fn url(&self) -> &str {
        self.inner.url()
    }

    /// Makes every request from now on give up by `at`.
    pub fn finish_by(&self, at: Instant) {
        self.inner.finish_by(at);
    }

    /// Runs `f` each time the API answers 401.
    pub fn on_rejected(&self, f: impl Fn() + Send + Sync + 'static) {
        self.inner.on_rejected(f);
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
        self.inner.select(table, query).map_err(todo_error)
    }

    /// The rows of `table` that `query` selects, and how many it selects
    /// in all, whatever page of them `query` asks for.
    pub(crate) fn select_page<T: DeserializeOwned>(
        &self,
        table: &str,
        query: &[(&str, String)],
    ) -> Result<(Vec<T>, Option<usize>)> {
        self.inner.select_page(table, query).map_err(todo_error)
    }

    /// Calls the function `function` with the named `args`.
    pub(crate) fn call(&self, function: &str, args: &serde_json::Value) -> Result<Response> {
        self.inner.call(function, args).map_err(todo_error)
    }

    /// The JSON `resp` carries.
    pub(crate) fn body<T: DeserializeOwned>(&self, resp: Response) -> Result<T> {
        self.inner.body(resp)
    }
}

/// `e` as every todo store returns it: the store's own refusals as such,
/// and a refusal whose code says the database is not set up for devkit with
/// what to do about it.
fn todo_error(e: anyhow::Error) -> anyhow::Error {
    let Some(refused) = e.downcast_ref::<Refused>() else {
        return e;
    };
    match refused.code.as_deref() {
        Some(CLAIMED) => Claimed {
            by: Holder::new(refused.details.clone().unwrap_or_default()),
        }
        .into(),
        Some(UNKNOWN_TODO) => anyhow!("{}", refused.message),
        code => match advice(code) {
            Some(advice) => anyhow!("{refused}; {advice}"),
            None => e,
        },
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
    devkit_supabase::is_unreachable(e)
}
