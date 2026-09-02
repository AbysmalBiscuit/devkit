//! Direct GitHub REST/GraphQL access over a shared `ureq::Agent`, replacing
//! per-call `gh` subprocess spawns on the read paths.
//!
//! Auth reuses whatever `gh` already relies on: `GH_TOKEN`/`GITHUB_TOKEN` from
//! the environment, else the token `gh auth token` prints (spawned once and
//! cached). No credential is stored by devkit. When no token can be resolved,
//! [`token`] returns `None` and callers fall back to their existing `gh` path,
//! so behavior is unchanged where `gh`'s ambient auth isn't reachable.
//!
//! Every function here is read-only. Mutating and git-level operations
//! (`gh pr create`, `gh pr edit`, `gh pr checkout`) stay on `gh`.

use anyhow::{Context, Result};
use serde_json::Value;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

const API: &str = "https://api.github.com";
const UA: &str = "devkit";

/// One pooled agent for the whole process so repeated calls reuse the TCP/TLS
/// connection instead of dialing GitHub afresh each time.
fn agent() -> &'static ureq::Agent {
    static A: OnceLock<ureq::Agent> = OnceLock::new();
    A.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(30))
            .build()
    })
}

/// Where the GitHub token devkit sends was found. `Env` names the variable so
/// a report can print it; `Gh` means `gh auth token` produced it, which is the
/// only case where gh's active account is also devkit's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    Env(&'static str),
    Gh,
    None,
}

fn resolve_token() -> (Option<String>, TokenSource) {
    for key in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(v) = std::env::var(key) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return (Some(v), TokenSource::Env(key));
            }
        }
    }
    // One `gh` spawn, cached for the process — amortized across every HTTP call.
    // `--hostname` is explicit: with `GH_HOST` set, an unqualified call returns
    // an enterprise token, which the callers below would then send to
    // api.github.com.
    let gh = crate::cmd::capture("gh", &["auth", "token", "--hostname", "github.com"], None)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    match gh {
        Some(v) => (Some(v), TokenSource::Gh),
        None => (None, TokenSource::None),
    }
}

/// Token and source, resolved together exactly once per process: env first,
/// then `gh auth token`. Both [`token`] and [`token_source`] read this same
/// cache so the two can never disagree about where a token came from.
fn resolved() -> &'static (Option<String>, TokenSource) {
    static T: OnceLock<(Option<String>, TokenSource)> = OnceLock::new();
    T.get_or_init(resolve_token)
}

/// The GitHub token, resolved once per process: env first, then `gh auth token`.
/// `None` when neither is available — callers then use their `gh` fallback.
pub fn token() -> Option<&'static str> {
    resolved().0.as_deref()
}

/// Where [`token`] came from, from the same resolution `token` reads.
pub fn token_source() -> TokenSource {
    resolved().1
}

fn bearer() -> Result<String> {
    token()
        .map(|t| format!("Bearer {t}"))
        .context("no GitHub token (set GH_TOKEN/GITHUB_TOKEN or run `gh auth login`)")
}

/// POST a raw GraphQL query to `api.github.com/graphql`, returning the
/// response envelope whole (`{ "data": …, "errors": … }`) with no error
/// handling of its own — both [`graphql`] and [`graphql_partial`] apply their
/// own acceptance rule to the same request.
fn graphql_request(query: &str) -> Result<Value> {
    let _span = crate::timing::io_span("github graphql", "graphql").entered();
    Ok(agent()
        .post(&format!("{API}/graphql"))
        .set("Authorization", &bearer()?)
        .set("User-Agent", UA)
        .send_json(ureq::json!({ "query": query }))?
        .into_json()?)
}

fn graphql_error_message(v: &Value) -> &str {
    v.get("errors")
        .and_then(|e| e.as_array())
        .and_then(|e| e.first())
        .and_then(|e| e["message"].as_str())
        .unwrap_or("unknown GraphQL error")
}

/// POST a raw GraphQL query to `api.github.com/graphql`. The response envelope
/// is returned whole (`{ "data": … }`); a non-empty `errors` array is an error.
pub fn graphql(query: &str) -> Result<Value> {
    let v = graphql_request(query)?;
    if let Some(errors) = v.get("errors").and_then(|e| e.as_array())
        && !errors.is_empty()
    {
        anyhow::bail!("GitHub GraphQL error: {}", graphql_error_message(&v));
    }
    Ok(v)
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

/// A GraphQL response whose every error is a `NOT_FOUND` is a successful
/// partial answer: an aliased batch reports one missing id that way while
/// returning real data for the rest. Any other error class still fails.
pub fn graphql_partial(query: &str) -> Result<Value> {
    let v = graphql_request(query)?;
    if accepts_partial(&v) {
        return Ok(v);
    }
    anyhow::bail!("GitHub GraphQL error: {}", graphql_error_message(&v));
}

/// GET `{API}{path}`. `Ok(Some(json))` on 2xx, `Ok(None)` on 404 (a clean
/// "absent" the caller can act on), `Err` on any other status or transport error.
pub fn rest_get_opt(path: &str) -> Result<Option<Value>> {
    let _span = crate::timing::io_span("github REST", path).entered();
    let resp = agent()
        .get(&format!("{API}{path}"))
        .set("Authorization", &bearer()?)
        .set("User-Agent", UA)
        .set("Accept", "application/vnd.github+json")
        .call();
    match resp {
        Ok(r) => Ok(Some(r.into_json()?)),
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// GET `{API}{path}`, erroring on 404.
pub fn rest_get(path: &str) -> Result<Value> {
    rest_get_opt(path)?.context("GitHub returned 404")
}

// --- url parsing -----------------------------------------------------------

/// Parse the PR number out of a `…/pull/<n>` GitHub URL.
pub fn pr_number_from_url(url: &str) -> Option<u64> {
    let tail = url.split("/pull/").nth(1)?;
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

// --- slug ------------------------------------------------------------------

/// The host a git remote URL names, before any `~/.ssh/config` alias is
/// resolved: scp-like `[user@]host:path`, `scheme://[user@]host/path`, or
/// `None` for a URL that carries no host at all — a bare local path.
///
/// A one-character host is a Windows drive letter (`C:/src/repo`), which is
/// scp-like by shape because the colon precedes every slash. Git makes the
/// same exception, and reads it as a path.
pub fn remote_host(url: &str) -> Option<&str> {
    let u = url.trim();
    if let Some((_, rest)) = u.split_once("://") {
        let after_user = rest.rsplit('@').next().unwrap_or(rest);
        let host = after_user.split(['/', ':']).next().unwrap_or("");
        return (!host.is_empty()).then_some(host);
    }
    let (before_colon, _) = u.split_once(':')?;
    if before_colon.contains('/') {
        return None;
    }
    let host = before_colon.rsplit('@').next().unwrap_or(before_colon);
    (host.chars().count() > 1).then_some(host)
}

/// Whether a remote is carried over ssh, and so takes its host from
/// `~/.ssh/config`. Scp-like syntax is ssh by definition; a URL with a scheme
/// is ssh only when it says so.
fn is_ssh_form(url: &str) -> bool {
    match url.trim().split_once("://") {
        Some((scheme, _)) => scheme.eq_ignore_ascii_case("ssh"),
        None => true,
    }
}

/// Parse `ssh -G <host>`'s effective configuration for the hostname it would
/// actually connect to. Keywords come back lowercased, one per line.
fn ssh_hostname_from_dump(dump: &str) -> Option<String> {
    dump.lines()
        .find_map(|l| l.strip_prefix("hostname "))
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
}

/// The hostname behind an ssh `Host` alias, per the user's own ssh config.
/// `None` when ssh is absent or the alias resolves to nothing usable.
///
/// Bounded because `ssh -G` is not a plain config read: a `Match exec` block
/// runs its command while the config is parsed, and this sits on the session
/// hook path where a wedged child would wedge the hook.
fn ssh_hostname(alias: &str) -> Option<String> {
    let dump = crate::cmd::capture_bounded("ssh", &["-G", alias], SSH_CONFIG_TIMEOUT)?;
    ssh_hostname_from_dump(&dump)
}

/// Long enough for an `ssh -G` that shells out through `Match exec`, short
/// enough that a wedged one fails instead of hanging the caller.
const SSH_CONFIG_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether a remote reaches github.com, with `resolve` mapping an ssh alias to
/// the hostname ssh would connect to. Split from `ssh_hostname` so the host
/// rules are testable without an ssh config.
fn reaches_github(url: &str, resolve: &dyn Fn(&str) -> Option<String>) -> bool {
    let Some(host) = remote_host(url) else {
        return false;
    };
    if host.eq_ignore_ascii_case("github.com") {
        return true;
    }
    // An https host is literal. Resolving one through ssh config would let an
    // unrelated `Host` block decide where an https remote points.
    is_ssh_form(url) && resolve(host).is_some_and(|h| h.eq_ignore_ascii_case("github.com"))
}

/// Parse `owner/repo` from a GitHub remote URL (scp-like, `ssh://`, or https),
/// stripping a trailing `.git`. Pure → unit-tested.
pub fn slug_from_remote_url(url: &str) -> Option<String> {
    let u = url.trim();
    let rest = if let Some((_, r)) = u.split_once("://") {
        // ssh://git@github.com/owner/repo(.git), https://github.com/owner/repo
        r.split_once('/').map(|(_, p)| p)?
    } else {
        // [user@]host:owner/repo(.git), where `host` may be an ssh alias
        remote_host(u)?;
        u.split_once(':').map(|(_, p)| p)?
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let mut it = rest.split('/');
    let owner = it.next().filter(|s| !s.is_empty())?;
    let repo = it.next().filter(|s| !s.is_empty())?;
    Some(format!("{owner}/{repo}"))
}

/// A repository slug is exactly `owner/repo`, both segments non-empty and made
/// only of characters GitHub allows in a name. Configured slugs reach cache
/// filenames and `gh --repo` arguments, so a slug carrying a path separator or
/// `..` is rejected where it is resolved rather than sanitized downstream.
pub fn validate_slug(s: &str) -> Result<()> {
    fn ok_segment(seg: &str) -> bool {
        !seg.is_empty()
            && seg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && seg != "."
            && seg != ".."
    }
    let mut parts = s.split('/');
    let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) else {
        anyhow::bail!("`{s}` is not an owner/repo repository slug");
    };
    anyhow::ensure!(
        ok_segment(owner) && ok_segment(repo),
        "`{s}` is not an owner/repo repository slug"
    );
    Ok(())
}

/// Whether a git remote URL names github.com literally.
/// `slug_from_remote_url` parses any `host/owner/repo` shape without checking
/// the host, so a GitLab origin yields a slug and every downstream caller
/// would query github.com for a repository that is not the project's.
///
/// An ssh alias spells no host at all, so this says no for one; the callers
/// that can afford to ask ssh use `remote_reaches_github`.
pub fn is_github_remote(url: &str) -> bool {
    reaches_github(url, &|_| None)
}

/// Whether a git remote reaches github.com once `~/.ssh/config` has its say.
/// A `Host` alias substitutes the hostname wholesale — `gh:owner/repo.git` is
/// a github.com remote when ssh config maps `gh` to it — so the literal spelling
/// of an ssh remote cannot settle the question on its own.
pub fn remote_reaches_github(url: &str) -> bool {
    reaches_github(url, &|alias| ssh_hostname(alias))
}

/// The `origin` slug, only when origin is a github.com remote. This is the
/// single entry point for defaulting a repository from the remote, so the host
/// check cannot be skipped by a caller that declared its tracker and therefore
/// never ran detection.
pub fn github_origin_slug(cwd: &str) -> Result<String> {
    let url = crate::git::Git::at(Path::new(cwd))
        .args(["remote", "get-url", "origin"])
        .output()
        .context("reading the `origin` remote")?;
    anyhow::ensure!(
        remote_reaches_github(&url),
        "`origin` is not a github.com remote ({}); {}",
        url.trim(),
        unreachable_hint(&url)
    );
    slug_from_remote_url(&url)
        .with_context(|| format!("no owner/repo in the origin URL `{}`", url.trim()))
}

/// What to try when `origin` does not reach github.com. An ssh alias that
/// resolves elsewhere is the case worth naming: the remote looks nothing like
/// a hostname, so "not a github.com remote" reads as a bug rather than as an
/// ssh config that maps the alias somewhere else.
fn unreachable_hint(url: &str) -> String {
    match remote_host(url) {
        Some(host) if is_ssh_form(url) && !host.eq_ignore_ascii_case("github.com") => format!(
            "ssh config resolves `{host}` to {}, so set [github] issues_repo / pr_repo explicitly",
            ssh_hostname(host).unwrap_or_else(|| "nothing".to_string())
        ),
        _ => "set [github] issues_repo / pr_repo explicitly".to_string(),
    }
}

/// One resolved repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub slug: String,
}

impl Repo {
    /// The `gh --repo` spelling. The host is explicit because `--repo o/r`
    /// leaves `GH_HOST` free to select an enterprise host, which would send a
    /// token to a host it was not issued for.
    pub fn qualified(&self) -> String {
        format!("github.com/{}", self.slug)
    }
}

/// A repository named explicitly by a locator, as opposed to one defaulted
/// from `pr_repo` or an origin remote.
fn overridden_repo(slug: &str) -> Result<Repo> {
    validate_slug(slug)?;
    Ok(Repo {
        slug: slug.to_string(),
    })
}

/// A pull request, identified. `repo: None` means the input was a bare number
/// or `#42` and defaults to `pr_repo`; a URL fills it in and that repository
/// wins.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrLocator {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub number: u64,
}

impl PrLocator {
    /// The repository this locator names, or `pr_repo` when it names none.
    pub fn resolve(&self, repos: &Repos) -> Result<Repo> {
        match &self.repo {
            Some(slug) => overridden_repo(slug),
            None => repos.prs().cloned(),
        }
    }

    /// The repository this locator names, or `default` when it names none —
    /// for a caller that already holds the one repository a bare number would
    /// resolve to and has no `Repos` seam to ask.
    pub fn resolve_or(&self, default: &Repo) -> Result<Repo> {
        match &self.repo {
            Some(slug) => overridden_repo(slug),
            None => Ok(default.clone()),
        }
    }

    /// Parse `https://github.com/owner/repo/pull/N`.
    pub fn from_url(url: &str) -> Option<PrLocator> {
        let rest = url.split("github.com/").nth(1)?;
        let mut seg = rest.split('/');
        let owner = seg.next()?;
        let name = seg.next()?;
        if seg.next()? != "pull" {
            return None;
        }
        let number = seg.next()?.split(['?', '#']).next()?.parse().ok()?;
        Some(PrLocator {
            repo: Some(format!("{owner}/{name}")),
            number,
        })
    }
}

/// The repositories one command works against, resolved once and threaded to
/// every GitHub operation. Each key resolves independently and is required only
/// where it is used, so a Linear project with a fork workflow sets `pr_repo`
/// alone and is never asked for an `issues_repo` it will not read.
#[derive(Debug, Clone)]
pub struct Repos {
    issues: std::result::Result<Repo, String>,
    prs: std::result::Result<Repo, String>,
}

impl Repos {
    /// Resolve from config plus the `origin` remote. `pr_override` is `issue prs
    /// --repo`, one invocation's override of `pr_repo`.
    pub fn resolve(
        cfg: &devkit_config::GithubConfig,
        cwd: &str,
        pr_override: Option<&str>,
    ) -> Repos {
        // The origin lookup is skipped entirely when config supplies both keys,
        // so a project outside GitHub never pays for it and never fails on it.
        let need_origin =
            cfg.issues_repo.is_none() || (cfg.pr_repo.is_none() && pr_override.is_none());
        match need_origin.then(|| github_origin_slug(cwd)).transpose() {
            Ok(origin) => build(cfg, origin, pr_override, &missing_message),
            // The origin lookup itself is the more useful explanation than the
            // generic "set [github] <key>" message, so a key that would have
            // defaulted to it reports why the default failed instead.
            Err(e) => {
                let msg = format!("{e:#}");
                build(cfg, None, pr_override, &|_| msg.clone())
            }
        }
    }

    /// `resolve` with the origin slug supplied rather than read, so resolution
    /// is testable without a git remote.
    #[doc(hidden)]
    pub fn from_parts(
        cfg: &devkit_config::GithubConfig,
        origin: Option<String>,
        pr_override: Option<&str>,
    ) -> Repos {
        build(cfg, origin, pr_override, &missing_message)
    }

    /// The issues repository, or the error explaining which key would supply it.
    pub fn issues(&self) -> Result<&Repo> {
        self.issues.as_ref().map_err(|e| anyhow::anyhow!(e.clone()))
    }

    /// The pull-request repository, or the error explaining which key would
    /// supply it.
    pub fn prs(&self) -> Result<&Repo> {
        self.prs.as_ref().map_err(|e| anyhow::anyhow!(e.clone()))
    }
}

/// The default "no repository" error for `key`, used when there was no origin
/// failure to report instead.
fn missing_message(key: &str) -> String {
    format!(
        "no GitHub repository for {key}: set [github] {key} or give the project a \
         github.com `origin` remote"
    )
}

/// The one precedence chain both `resolve` and `from_parts` run: config, then
/// override (`prs` only), then the origin default, each key independent.
/// `missing` supplies the error a key reports when nothing resolves it — the
/// generic "set [github] key" message normally, or the origin lookup's own
/// error when that lookup is what failed.
fn build(
    cfg: &devkit_config::GithubConfig,
    origin: Option<String>,
    pr_override: Option<&str>,
    missing: &dyn Fn(&str) -> String,
) -> Repos {
    Repos {
        issues: cfg
            .issues_repo
            .clone()
            .or_else(|| origin.clone())
            .ok_or_else(|| missing("issues_repo"))
            .and_then(checked),
        prs: pr_override
            .map(str::to_string)
            .or_else(|| cfg.pr_repo.clone())
            .or(origin)
            .ok_or_else(|| missing("pr_repo"))
            .and_then(checked),
    }
}

/// Validate a resolved slug at the point of resolution, so a bad value is
/// reported against the key that carries it rather than failing later inside a
/// URL or a cache filename.
fn checked(slug: String) -> std::result::Result<Repo, String> {
    match validate_slug(&slug) {
        Ok(()) => Ok(Repo { slug }),
        Err(e) => Err(format!("{e:#}")),
    }
}

// --- typed reads -----------------------------------------------------------

/// Number + title + head branch of a PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrMeta {
    pub number: u64,
    pub title: String,
    pub head_ref_name: String,
}

/// URL + title + author login of a PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrFull {
    pub url: String,
    pub title: String,
    pub author_login: Option<String>,
}

/// A PR reduced to the fields worktree/status triage needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrBrief {
    pub number: u64,
    pub state: String, // MERGED | OPEN | CLOSED
    pub url: String,
    pub head_ref_name: String,
    /// The commit at the PR's head. This is what ties a PR to a worktree: a
    /// branch name does not, since two forks routinely propose the same name.
    pub head_ref_oid: String,
    /// The fork the head branch lives in, when the API reported one.
    pub head_repo_owner: Option<String>,
    /// Whether the PR is a draft. GitHub reports a draft's `state` as `OPEN`,
    /// so this is the only thing separating "still being written" from "waiting
    /// on a reviewer".
    pub is_draft: bool,
}

fn as_str(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn head_ref(v: &Value) -> String {
    v.get("head")
        .and_then(|h| h.get("ref"))
        .and_then(|r| r.as_str())
        .unwrap_or("")
        .to_string()
}

fn head_sha(v: &Value) -> String {
    v.get("head")
        .and_then(|h| h.get("sha"))
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string()
}

fn head_repo_owner(v: &Value) -> Option<String> {
    v.get("head")?
        .get("repo")?
        .get("owner")?
        .get("login")?
        .as_str()
        .map(String::from)
}

/// gh's `state` distinguishes MERGED; REST's `state` is only open/closed with a
/// separate `merged_at`. Reconstruct gh's value so callers ranking on MERGED
/// keep working.
fn gh_state(v: &Value) -> String {
    if v.get("merged_at").map(|m| !m.is_null()).unwrap_or(false) {
        return "MERGED".to_string();
    }
    match v.get("state").and_then(|s| s.as_str()).unwrap_or("") {
        "open" => "OPEN".to_string(),
        "closed" => "CLOSED".to_string(),
        other => other.to_uppercase(),
    }
}

fn parse_meta(v: &Value) -> Option<PrMeta> {
    Some(PrMeta {
        number: v.get("number")?.as_u64()?,
        title: as_str(v, "title"),
        head_ref_name: head_ref(v),
    })
}

fn parse_full(v: &Value) -> PrFull {
    PrFull {
        url: as_str(v, "html_url"),
        title: as_str(v, "title"),
        author_login: v
            .get("user")
            .and_then(|u| u.get("login"))
            .and_then(|l| l.as_str())
            .map(String::from),
    }
}

fn parse_brief(v: &Value) -> Option<PrBrief> {
    Some(PrBrief {
        number: v.get("number")?.as_u64()?,
        state: gh_state(v),
        url: as_str(v, "html_url"),
        head_ref_name: head_ref(v),
        head_ref_oid: head_sha(v),
        head_repo_owner: head_repo_owner(v),
        is_draft: v.get("draft").and_then(|d| d.as_bool()).unwrap_or(false),
    })
}

fn parse_requested_reviewers(v: &Value) -> Vec<String> {
    v.get("users")
        .and_then(|u| u.as_array())
        .into_iter()
        .flatten()
        .filter_map(|u| u.get("login").and_then(|l| l.as_str()).map(String::from))
        .collect()
}

/// Whether PR `n` exists in `slug` (`owner/repo`).
pub fn pr_exists(slug: &str, n: u64) -> Result<bool> {
    Ok(rest_get_opt(&format!("/repos/{slug}/pulls/{n}"))?.is_some())
}

/// Number/title/head-branch for PR `n`.
pub fn pr_meta(slug: &str, n: u64) -> Result<PrMeta> {
    parse_meta(&rest_get(&format!("/repos/{slug}/pulls/{n}"))?)
        .with_context(|| format!("unexpected PR shape for #{n}"))
}

/// URL/title/author for PR `n`.
pub fn pr_full(slug: &str, n: u64) -> Result<PrFull> {
    Ok(parse_full(&rest_get(&format!("/repos/{slug}/pulls/{n}"))?))
}

/// A single-PR REST response mapped to the triage shape. `None` is reserved
/// for a PR that does not exist (a 404, which `rest_get_opt` reports as no
/// body): a body that came back and could not be parsed is an error, since
/// "there is no such PR" is what closes a worktree's finished verdict.
fn brief_of_response(body: Option<Value>, n: u64, slug: &str) -> Result<Option<PrBrief>> {
    match body {
        None => Ok(None),
        Some(v) => {
            Ok(Some(parse_brief(&v).with_context(|| {
                format!("unexpected PR shape for #{n} in {slug}")
            })?))
        }
    }
}

/// The full triage shape (state, url, head branch and head oid) for PR `n` in
/// `repo`, or `None` if no such PR exists. The exact single-PR read behind
/// verifying a PR just created or checked out — neither carries the head oid
/// the creating/checkout response omits.
pub fn pr_by_number(repo: &Repo, n: u64) -> Result<Option<PrBrief>> {
    brief_of_response(
        rest_get_opt(&format!("/repos/{}/pulls/{n}", repo.slug))?,
        n,
        &repo.slug,
    )
}

/// One pull request from a batched read: `Ok(None)` is a repository or pull
/// request that does not resolve, `Err` a node that came back unparseable.
pub type PrLookup = Result<Option<PrBrief>>;

/// `(slug, number)` targets grouped by repository in first-seen order, each
/// group carrying its targets' indices. The query builder and the parser both
/// walk this, so they agree on every alias without passing a map between them.
fn group_by_repo(targets: &[(String, u64)]) -> Vec<(&str, Vec<usize>)> {
    let mut groups: Vec<(&str, Vec<usize>)> = Vec::new();
    for (i, (slug, _)) in targets.iter().enumerate() {
        match groups.iter_mut().find(|(s, _)| *s == slug) {
            Some((_, idx)) => idx.push(i),
            None => groups.push((slug, vec![i])),
        }
    }
    groups
}

/// One GraphQL round trip resolving many pull requests by number, aliased the
/// way this module's other batch queries are. Repeated repositories collapse
/// into one `repository` alias, so a cross-repository target costs an extra
/// alias rather than an extra round trip.
pub fn prs_by_number_query(targets: &[(String, u64)]) -> String {
    let fields = "number state url headRefName headRefOid isDraft \
                  headRepositoryOwner { login }";
    let repos = group_by_repo(targets)
        .into_iter()
        .enumerate()
        .map(|(g, (slug, idx))| {
            let (owner, name) = slug.split_once('/').unwrap_or((slug, ""));
            let prs = idx
                .iter()
                .map(|i| {
                    format!(
                        "p{i}: pullRequest(number: {}) {{ {fields} }}",
                        targets[*i].1
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!(
                "r{g}: repository(owner: {}, name: {}) {{ {prs} }}",
                serde_json::Value::from(owner),
                serde_json::Value::from(name),
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("query {{ {repos} }}")
}

/// Split a `prs_by_number_query` response back into one lookup per target, in
/// the order the targets were given. A null alias is GitHub reporting the
/// repository or pull request as absent (paired with a `NOT_FOUND` error);
/// a missing alias or an unparseable node is a malformed response, which is an
/// error rather than an absence.
pub fn parse_prs_by_number(resp: &Value, targets: &[(String, u64)]) -> Vec<PrLookup> {
    let mut out: Vec<PrLookup> = targets
        .iter()
        .map(|(slug, n)| Err(anyhow::anyhow!("no answer for #{n} in {slug}")))
        .collect();
    let Some(data) = resp.get("data").filter(|d| !d.is_null()) else {
        return out;
    };
    for (g, (slug, idx)) in group_by_repo(targets).into_iter().enumerate() {
        let key = format!("r{g}");
        let repo = match data.get(&key) {
            None => continue,
            Some(v) if v.is_null() => {
                for i in idx {
                    out[i] = Ok(None);
                }
                continue;
            }
            Some(v) => v,
        };
        for i in idx {
            let number = targets[i].1;
            let alias = format!("p{i}");
            out[i] = match repo.get(&alias) {
                None => Err(anyhow::anyhow!(
                    "no `{alias}` alias in the response for #{number} in {slug}"
                )),
                Some(v) if v.is_null() => Ok(None),
                Some(v) => parse_pr_node(v)
                    .map(Some)
                    .with_context(|| format!("unexpected PR shape for #{number} in {slug}")),
            };
        }
    }
    out
}

/// The exact pull requests `targets` names, in one GraphQL round trip. `Err`
/// is the whole request failing (no token, transport, a hard GraphQL error);
/// a per-target answer is a [`PrLookup`].
pub fn prs_by_number(targets: &[(String, u64)]) -> Result<Vec<PrLookup>> {
    if targets.is_empty() {
        return Ok(Vec::new());
    }
    let v = graphql_partial(&prs_by_number_query(targets))?;
    Ok(parse_prs_by_number(&v, targets))
}

/// [`pr_by_number`], erroring rather than returning `None` when the PR does
/// not exist.
pub fn pr_meta_full(repo: &Repo, n: u64) -> Result<PrBrief> {
    pr_by_number(repo, n)?.with_context(|| format!("PR #{n} not found in {}", repo.slug))
}

/// What a head-branch lookup found. An `Option` cannot distinguish "the
/// transport answered and there is no such PR" from "the transport failed", and
/// every caller collapsed both into a fallback that guessed.
#[derive(Debug, Clone)]
pub enum HeadLookup {
    Unique(PrBrief),
    NoMatch,
    Ambiguous(Vec<PrBrief>),
    Unavailable(String),
}

/// PRs whose head branch is `branch`, in any fork.
///
/// GraphQL rather than REST: REST documents `head` only as `user:ref-name`, and
/// the head owner cannot be derived — git allows a push URL distinct from the
/// fetch URL, `remote.pushDefault`, and per-branch push remotes, so `origin`
/// need not be where a branch was pushed. `headRefName` is a documented
/// argument that matches a fork's branch with no owner qualifier.
pub fn head_query(slug: &str, branch: &str) -> String {
    let (owner, name) = slug.split_once('/').unwrap_or((slug, ""));
    format!(
        r#"query {{ repository(owner: {owner}, name: {name}) {{
             pullRequests(headRefName: {branch}, first: 10,
                          states: [OPEN, CLOSED, MERGED]) {{
               totalCount
               nodes {{ number state url headRefName headRefOid isDraft
                        headRepositoryOwner {{ login }} }}
             }} }} }}"#,
        owner = serde_json::Value::from(owner),
        name = serde_json::Value::from(name),
        branch = serde_json::Value::from(branch),
    )
}

/// One GraphQL `PullRequest` node, as every query here selects it. `None` when
/// a field the triage shape needs is missing or of the wrong type.
fn parse_pr_node(n: &Value) -> Option<PrBrief> {
    Some(PrBrief {
        number: n["number"].as_u64()?,
        state: n["state"].as_str()?.to_string(),
        url: n["url"].as_str()?.to_string(),
        head_ref_name: n["headRefName"].as_str()?.to_string(),
        head_ref_oid: n["headRefOid"].as_str().unwrap_or("").to_string(),
        head_repo_owner: n["headRepositoryOwner"]["login"]
            .as_str()
            .map(str::to_string),
        is_draft: n["isDraft"].as_bool().unwrap_or(false),
    })
}

/// Parse a successful `head_query` envelope (no top-level `errors` — `graphql`
/// already turns those into an `Err` before a caller ever reaches this).
/// `totalCount` beyond the returned nodes is ambiguity, not a unique answer: a
/// winner outside the window would otherwise be silently dropped.
pub fn parse_head_lookup(resp: &Value) -> HeadLookup {
    let conn = &resp["data"]["repository"]["pullRequests"];
    let total = conn["totalCount"].as_u64().unwrap_or(0);
    let nodes: Vec<PrBrief> = conn["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_pr_node)
        .collect();
    match nodes.len() {
        0 => HeadLookup::NoMatch,
        1 if total <= 1 => HeadLookup::Unique(nodes.into_iter().next().expect("len == 1")),
        _ => HeadLookup::Ambiguous(nodes),
    }
}

/// Look up `branch`'s PR in `repo`. `Unavailable` is the only answer a caller
/// may respond to by trying another transport.
pub fn pr_by_head(repo: &Repo, branch: &str) -> HeadLookup {
    if token().is_none() {
        return HeadLookup::Unavailable("no GitHub token resolved".into());
    }
    match graphql(&head_query(&repo.slug, branch)) {
        Ok(v) => parse_head_lookup(&v),
        Err(e) => HeadLookup::Unavailable(format!("{e:#}")),
    }
}

/// Human logins currently requested as reviewers on PR `n`.
pub fn requested_reviewers(slug: &str, n: u64) -> Result<Vec<String>> {
    let v = rest_get(&format!("/repos/{slug}/pulls/{n}/requested_reviewers"))?;
    Ok(parse_requested_reviewers(&v))
}

/// Open/merge timestamps + line counts of one PR, for timeline charts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrTimeline {
    pub created_at: Option<String>,
    pub merged_at: Option<String>,
    pub additions: i64,
    pub deletions: i64,
}

fn timeline_query(slug: &str, qualifier: &str, after: Option<&str>) -> String {
    let cursor = match after {
        Some(c) => format!(", after: \"{c}\""),
        None => String::new(),
    };
    format!(
        "query {{ search(query: \"repo:{slug} is:pr {qualifier}\", type: ISSUE, first: 100{cursor}) \
{{ nodes {{ ... on PullRequest {{ createdAt mergedAt additions deletions }} }} \
pageInfo {{ hasNextPage endCursor }} }} }}"
    )
}

fn parse_timeline_page(v: &Value) -> (Vec<PrTimeline>, Option<String>) {
    let block = &v["data"]["search"];
    let items = block["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|n| PrTimeline {
            created_at: n
                .get("createdAt")
                .and_then(|x| x.as_str())
                .map(String::from),
            merged_at: n.get("mergedAt").and_then(|x| x.as_str()).map(String::from),
            additions: n.get("additions").and_then(|x| x.as_i64()).unwrap_or(0),
            deletions: n.get("deletions").and_then(|x| x.as_i64()).unwrap_or(0),
        })
        .collect();
    let next = match (
        block["pageInfo"]["hasNextPage"].as_bool(),
        block["pageInfo"]["endCursor"].as_str(),
    ) {
        (Some(true), Some(c)) => Some(c.to_string()),
        _ => None,
    };
    (items, next)
}

/// PRs matching `qualifier` (e.g. `author:@me`, `reviewed-by:@me`) in `slug`,
/// paginated up to `max`. GitHub search accepts `@me` in the GraphQL query.
pub fn pr_timeline(slug: &str, qualifier: &str, max: usize) -> Result<Vec<PrTimeline>> {
    let mut out = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let v = graphql(&timeline_query(slug, qualifier, after.as_deref()))?;
        let (items, next) = parse_timeline_page(&v);
        out.extend(items);
        match next {
            Some(c) if out.len() < max => after = Some(c),
            _ => break,
        }
    }
    out.truncate(max);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn pr_number_parsed_from_url() {
        assert_eq!(
            pr_number_from_url("https://github.com/org/repo/pull/3340"),
            Some(3340)
        );
        assert_eq!(
            pr_number_from_url("https://github.com/org/repo/issues/9"),
            None
        );
    }

    #[test]
    fn slug_parses_ssh_and_https() {
        for (url, want) in [
            ("git@github.com:acme/monorepo.git", "acme/monorepo"),
            ("git@github.com:acme/monorepo", "acme/monorepo"),
            ("https://github.com/acme/monorepo.git", "acme/monorepo"),
            ("https://github.com/acme/monorepo", "acme/monorepo"),
            ("https://github.com/acme/monorepo/", "acme/monorepo"),
            ("ssh://git@github.com/acme/monorepo.git", "acme/monorepo"),
        ] {
            assert_eq!(slug_from_remote_url(url).as_deref(), Some(want), "{url}");
        }
    }

    #[test]
    fn slug_rejects_garbage() {
        assert_eq!(slug_from_remote_url("not-a-url"), None);
        assert_eq!(slug_from_remote_url("https://github.com/onlyowner"), None);
        assert_eq!(slug_from_remote_url(""), None);
    }

    /// A `~/.ssh/config` `Host` alias replaces the hostname outright, so the
    /// remote carries no user and no recognizable host: `gh:owner/repo.git`.
    #[test]
    fn slug_parses_an_ssh_alias_host() {
        for (url, want) in [
            ("gh:acme/monorepo.git", "acme/monorepo"),
            ("gh:acme/monorepo", "acme/monorepo"),
            ("ssh://gh/acme/monorepo.git", "acme/monorepo"),
            ("me@gh:acme/monorepo.git", "acme/monorepo"),
        ] {
            assert_eq!(slug_from_remote_url(url).as_deref(), Some(want), "{url}");
        }
    }

    /// `C:/src/repo` is scp-like by shape — the colon precedes every slash —
    /// but a one-letter host is a Windows drive, and git reads it as a path.
    #[test]
    fn slug_rejects_a_windows_drive_path() {
        assert_eq!(slug_from_remote_url("C:/src/repo"), None);
        assert_eq!(remote_host("C:/src/repo"), None);
    }

    #[test]
    fn remote_host_reads_every_url_shape() {
        for (url, want) in [
            ("gh:acme/repo.git", Some("gh")),
            ("git@github.com:acme/repo.git", Some("github.com")),
            ("ssh://git@github.com/acme/repo", Some("github.com")),
            ("https://github.com/acme/repo", Some("github.com")),
            ("/srv/git/repo.git", None),
            ("", None),
        ] {
            assert_eq!(remote_host(url), want, "{url}");
        }
    }

    /// The alias is only a github.com remote once ssh config says so, and the
    /// answer follows the resolver rather than the spelling of the alias.
    #[test]
    fn an_ssh_alias_reaches_github_only_when_the_config_says_so() {
        let to_github = |_: &str| Some("github.com".to_string());
        let to_gitlab = |_: &str| Some("gitlab.com".to_string());
        let unresolved = |_: &str| None;

        assert!(reaches_github("gh:acme/repo.git", &to_github));
        assert!(reaches_github("ssh://gh/acme/repo.git", &to_github));
        assert!(!reaches_github("gh:acme/repo.git", &to_gitlab));
        assert!(!reaches_github("gh:acme/repo.git", &unresolved));
    }

    /// Only ssh consults `~/.ssh/config`; an https host is literal. Resolving
    /// one through ssh would let an unrelated `Host` block decide that an
    /// https remote points at github.com.
    #[test]
    fn an_https_host_ignores_ssh_config() {
        let to_github = |_: &str| Some("github.com".to_string());
        assert!(!reaches_github("https://gh/acme/repo.git", &to_github));
        assert!(!reaches_github("git://gh/acme/repo.git", &to_github));
    }

    #[test]
    fn hostname_is_read_from_the_ssh_config_dump() {
        let dump = "user git\nhostname github.com\nport 22\nhostkeyalias gh\n";
        assert_eq!(ssh_hostname_from_dump(dump).as_deref(), Some("github.com"));
        assert_eq!(ssh_hostname_from_dump("user git\nport 22\n"), None);
    }

    #[test]
    fn gh_state_reconstructs_merged() {
        assert_eq!(
            gh_state(&json!({"state": "open", "merged_at": null})),
            "OPEN"
        );
        assert_eq!(
            gh_state(&json!({"state": "closed", "merged_at": null})),
            "CLOSED"
        );
        assert_eq!(
            gh_state(&json!({"state": "closed", "merged_at": "2026-06-20T00:00:00Z"})),
            "MERGED"
        );
    }

    #[test]
    fn parse_brief_maps_rest_fields() {
        let v = json!({
            "number": 42, "state": "closed", "merged_at": "2026-01-01T00:00:00Z",
            "html_url": "https://github.com/a/b/pull/42",
            "head": { "ref": "you/eng-1-foo" }
        });
        let b = parse_brief(&v).unwrap();
        assert_eq!(b.number, 42);
        assert_eq!(b.state, "MERGED");
        assert_eq!(b.url, "https://github.com/a/b/pull/42");
        assert_eq!(b.head_ref_name, "you/eng-1-foo");
    }

    #[test]
    fn a_body_that_will_not_parse_is_not_an_absent_pr() {
        assert!(brief_of_response(None, 7, "o/r").unwrap().is_none());

        let ok = brief_of_response(
            Some(json!({ "number": 7, "state": "open", "html_url": "u7" })),
            7,
            "o/r",
        )
        .unwrap()
        .expect("a parsed PR");
        assert_eq!(ok.number, 7);

        let err = brief_of_response(Some(json!({ "state": "open" })), 7, "o/r")
            .unwrap_err()
            .to_string();
        assert!(err.contains('7') && err.contains("o/r"), "{err}");
    }

    fn targets() -> Vec<(String, u64)> {
        vec![
            ("o/r".to_string(), 12),
            ("o/r".to_string(), 13),
            ("me/fork".to_string(), 9),
            ("gone/repo".to_string(), 1),
        ]
    }

    #[test]
    fn a_batch_query_names_each_repository_once() {
        let q = prs_by_number_query(&targets());
        assert_eq!(q.matches("repository(").count(), 3, "{q}");
        assert!(
            q.contains(r#"r0: repository(owner: "o", name: "r")"#),
            "{q}"
        );
        assert!(q.contains("p0: pullRequest(number: 12)"), "{q}");
        assert!(q.contains("p1: pullRequest(number: 13)"), "{q}");
        assert!(q.contains("p2: pullRequest(number: 9)"), "{q}");
        assert!(q.contains("p3: pullRequest(number: 1)"), "{q}");
    }

    #[test]
    fn a_batch_response_separates_resolved_missing_and_malformed() {
        let resp = json!({
            "data": {
                "r0": {
                    "p0": {
                        "number": 12, "state": "OPEN",
                        "url": "https://github.com/o/r/pull/12",
                        "headRefName": "feat/x", "headRefOid": "cafe1234",
                        "headRepositoryOwner": { "login": "o" }
                    },
                    "p1": null
                },
                "r1": { "p2": { "number": 9 } },
                "r2": null
            },
            "errors": [{ "type": "NOT_FOUND", "path": ["repository"] }]
        });
        let got = parse_prs_by_number(&resp, &targets());

        let found = got[0].as_ref().unwrap().as_ref().expect("PR 12 resolved");
        assert_eq!(found.number, 12);
        assert_eq!(found.head_ref_oid, "cafe1234");
        assert!(got[1].as_ref().unwrap().is_none(), "a null PR is absent");
        assert!(got[2].is_err(), "an unparseable node is not an absence");
        assert!(
            got[3].as_ref().unwrap().is_none(),
            "a repository that does not resolve is absent"
        );
    }

    #[test]
    fn a_batch_response_with_no_data_fails_every_target() {
        let got = parse_prs_by_number(&json!({ "data": null }), &targets());
        assert!(got.iter().all(|r| r.is_err()), "{got:?}");
        assert_eq!(got.len(), 4);
    }

    #[test]
    fn parse_meta_and_full() {
        let v = json!({
            "number": 7, "title": "Fix", "html_url": "u7",
            "head": { "ref": "br" }, "user": { "login": "bob" }
        });
        assert_eq!(
            parse_meta(&v).unwrap(),
            PrMeta {
                number: 7,
                title: "Fix".into(),
                head_ref_name: "br".into()
            }
        );
        let f = parse_full(&v);
        assert_eq!(f.url, "u7");
        assert_eq!(f.title, "Fix");
        assert_eq!(f.author_login.as_deref(), Some("bob"));
    }

    #[test]
    fn requested_reviewers_reads_user_logins() {
        let v = json!({ "users": [{"login": "alice"}, {"login": "carol"}], "teams": [] });
        assert_eq!(parse_requested_reviewers(&v), vec!["alice", "carol"]);
        assert!(parse_requested_reviewers(&json!({})).is_empty());
    }

    #[test]
    fn timeline_page_parses_nodes_and_cursor() {
        let v = json!({ "data": { "search": {
            "nodes": [
                { "createdAt": "2026-01-01T00:00:00Z", "mergedAt": null, "additions": 5, "deletions": 2 },
                { "createdAt": "2026-02-01T00:00:00Z", "mergedAt": "2026-02-03T00:00:00Z", "additions": 1, "deletions": 0 }
            ],
            "pageInfo": { "hasNextPage": true, "endCursor": "CUR" }
        }}});
        let (items, next) = parse_timeline_page(&v);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].created_at.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(items[0].merged_at, None);
        assert_eq!(items[1].additions, 1);
        assert_eq!(next.as_deref(), Some("CUR"));
    }

    #[test]
    fn timeline_page_stops_without_next() {
        let v = json!({ "data": { "search": {
            "nodes": [],
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        }}});
        let (items, next) = parse_timeline_page(&v);
        assert!(items.is_empty());
        assert_eq!(next, None);
    }

    #[test]
    fn timeline_query_scopes_repo_and_qualifier() {
        let q = timeline_query("acme/mono", "author:@me", None);
        assert!(q.contains("repo:acme/mono is:pr author:@me"));
        assert!(!q.contains("after:"));
        assert!(timeline_query("a/b", "reviewed-by:@me", Some("X")).contains("after: \"X\""));
    }

    #[test]
    fn validate_slug_accepts_owner_repo() {
        assert!(validate_slug("K-Nette/BountyPop_GODOT").is_ok());
        assert!(validate_slug("a/b").is_ok());
        assert!(validate_slug("owner.name/repo.name").is_ok());
    }

    #[test]
    fn validate_slug_rejects_anything_that_could_escape_a_path() {
        for bad in [
            "",
            "owner",
            "owner/repo/extra",
            "../../etc",
            "owner/../..",
            "owner/",
            "/repo",
            "own er/repo",
            "owner/re po",
        ] {
            assert!(validate_slug(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn github_origin_rejects_a_non_github_host() {
        // `slug_from_remote_url` is host-blind by design — its other callers
        // already know they hold a GitHub URL. The host check lives here.
        assert!(is_github_remote("https://github.com/o/r.git"));
        assert!(is_github_remote("git@github.com:o/r.git"));
        assert!(is_github_remote("ssh://git@github.com/o/r"));
        assert!(!is_github_remote("https://gitlab.com/o/r.git"));
        assert!(!is_github_remote("git@bitbucket.org:o/r.git"));
        assert!(!is_github_remote("https://github.com.evil.test/o/r"));
    }

    fn cfg(issues: Option<&str>, prs: Option<&str>) -> devkit_config::GithubConfig {
        devkit_config::GithubConfig {
            issues_repo: issues.map(str::to_string),
            pr_repo: prs.map(str::to_string),
        }
    }

    #[test]
    fn repos_resolve_each_key_independently() {
        // Both configured: no origin is consulted at all, so a project whose code
        // lives outside GitHub still resolves.
        let r = Repos::from_parts(&cfg(Some("org/planning"), Some("up/app")), None, None);
        assert_eq!(r.issues().unwrap().slug, "org/planning");
        assert_eq!(r.prs().unwrap().slug, "up/app");

        // Only pr_repo configured and no origin: the PR paths work, and only an
        // operation needing the issues repository fails — naming the key.
        let r = Repos::from_parts(&cfg(None, Some("up/app")), None, None);
        assert_eq!(r.prs().unwrap().slug, "up/app");
        let err = r.issues().unwrap_err().to_string();
        assert!(err.contains("issues_repo"), "{err}");

        // Neither configured, origin available: both default to it.
        let r = Repos::from_parts(&cfg(None, None), Some("me/fork".into()), None);
        assert_eq!(r.issues().unwrap().slug, "me/fork");
        assert_eq!(r.prs().unwrap().slug, "me/fork");

        // A per-invocation override beats pr_repo.
        let r = Repos::from_parts(&cfg(None, Some("up/app")), None, Some("other/x"));
        assert_eq!(r.prs().unwrap().slug, "other/x");
    }

    #[test]
    fn repos_reject_a_configured_slug_that_is_not_owner_repo() {
        let r = Repos::from_parts(&cfg(Some("../../etc/passwd"), None), None, None);
        let err = r.issues().unwrap_err().to_string();
        assert!(err.contains("owner/repo"), "{err}");
    }

    #[test]
    fn a_locator_with_a_repository_outranks_the_configured_one() {
        let repos = Repos::from_parts(&cfg(None, Some("up/app")), None, None);

        let pasted = PrLocator {
            repo: Some("fork/app".into()),
            number: 42,
        };
        assert_eq!(pasted.resolve(&repos).unwrap().slug, "fork/app");

        let bare = PrLocator {
            repo: None,
            number: 42,
        };
        assert_eq!(bare.resolve(&repos).unwrap().slug, "up/app");
    }

    #[test]
    fn resolve_or_falls_back_to_the_given_default_not_pr_repo() {
        let default = Repo {
            slug: "me/fork".into(),
        };
        let bare = PrLocator {
            repo: None,
            number: 9,
        };
        assert_eq!(bare.resolve_or(&default).unwrap(), default);

        let pasted = PrLocator {
            repo: Some("other/app".into()),
            number: 9,
        };
        assert_eq!(pasted.resolve_or(&default).unwrap().slug, "other/app");
    }

    /// A locator's slug is parsed out of untrusted pasted text, so it faces the
    /// same shape check a configured one does before reaching a `--repo`
    /// argument or a cache path.
    #[test]
    fn a_locator_repository_that_is_not_owner_repo_is_rejected() {
        let repos = Repos::from_parts(&cfg(None, Some("up/app")), None, None);
        let loc = PrLocator {
            repo: Some("../../etc/passwd".into()),
            number: 42,
        };
        let err = loc.resolve(&repos).unwrap_err().to_string();
        assert!(err.contains("owner/repo"), "{err}");
    }

    #[test]
    fn a_repo_qualifies_itself_with_the_host() {
        let r = Repo { slug: "o/r".into() };
        // `--repo o/r` leaves GH_HOST free to pick an enterprise host, so every
        // `gh pr` argument names github.com explicitly.
        assert_eq!(r.qualified(), "github.com/o/r");
    }

    fn head_resp(nodes: &str, total: u32) -> serde_json::Value {
        serde_json::from_str(&format!(
            r#"{{"data":{{"repository":{{"pullRequests":{{"totalCount":{total},"nodes":[{nodes}]}}}}}}}}"#
        ))
        .unwrap()
    }

    const NODE_A: &str = r#"{"number":185,"state":"OPEN","url":"https://github.com/up/app/pull/185",
      "headRefName":"fix/glyph-overhang","headRefOid":"aaaa111",
      "headRepositoryOwner":{"login":"contributor"}}"#;
    const NODE_B: &str = r#"{"number":42,"state":"MERGED","url":"https://github.com/up/app/pull/42",
      "headRefName":"fix/glyph-overhang","headRefOid":"bbbb222",
      "headRepositoryOwner":{"login":"someone-else"}}"#;

    #[test]
    fn one_node_parses_to_unique() {
        let l = parse_head_lookup(&head_resp(NODE_A, 1));
        let HeadLookup::Unique(pr) = l else {
            panic!("expected Unique, got {l:?}")
        };
        assert_eq!(pr.number, 185);
        assert_eq!(pr.head_ref_oid, "aaaa111");
    }

    #[test]
    fn a_fork_head_still_parses_to_unique() {
        // The whole reason this moved off REST: the head owner differs from the
        // searched repository's owner and the match must still be found.
        let HeadLookup::Unique(pr) = parse_head_lookup(&head_resp(NODE_A, 1)) else {
            panic!("expected Unique")
        };
        assert_eq!(pr.head_repo_owner.as_deref(), Some("contributor"));
    }

    #[test]
    fn zero_nodes_parses_to_no_match() {
        assert!(matches!(
            parse_head_lookup(&head_resp("", 0)),
            HeadLookup::NoMatch
        ));
    }

    #[test]
    fn two_nodes_parse_to_ambiguous() {
        let l = parse_head_lookup(&head_resp(&format!("{NODE_A},{NODE_B}"), 2));
        let HeadLookup::Ambiguous(c) = l else {
            panic!("expected Ambiguous, got {l:?}")
        };
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn a_total_count_beyond_the_window_is_ambiguous_not_unique() {
        // One node returned but the server says there are three: ranking a
        // truncated set is exactly the false-unique this type exists to prevent.
        assert!(matches!(
            parse_head_lookup(&head_resp(NODE_A, 3)),
            HeadLookup::Ambiguous(_)
        ));
    }

    #[test]
    fn parse_pr_node_reads_is_draft() {
        let n = json!({
            "number": 7, "state": "OPEN", "url": "u7",
            "headRefName": "feat/x", "headRefOid": "abc123",
            "isDraft": true
        });
        assert!(parse_pr_node(&n).unwrap().is_draft);
    }

    #[test]
    fn a_node_without_is_draft_is_not_a_draft() {
        let n = json!({
            "number": 7, "state": "OPEN", "url": "u7",
            "headRefName": "feat/x", "headRefOid": "abc123"
        });
        assert!(!parse_pr_node(&n).unwrap().is_draft);
    }

    #[test]
    fn parse_brief_reads_the_rest_draft_key() {
        let v = json!({
            "number": 42, "state": "open",
            "html_url": "https://github.com/a/b/pull/42",
            "head": { "ref": "you/eng-1-foo" },
            "draft": true
        });
        assert!(parse_brief(&v).unwrap().is_draft);
    }

    #[test]
    fn every_pr_query_selects_is_draft() {
        assert!(prs_by_number_query(&targets()).contains("isDraft"));
        assert!(head_query("o/r", "feat/x").contains("isDraft"));
    }
}
