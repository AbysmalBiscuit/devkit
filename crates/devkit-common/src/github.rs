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

fn resolve_token() -> Option<String> {
    for key in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(v) = std::env::var(key) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    // One `gh` spawn, cached for the process — amortized across every HTTP call.
    // `--hostname` is explicit: with `GH_HOST` set, an unqualified call returns
    // an enterprise token, which the callers below would then send to
    // api.github.com.
    crate::cmd::capture("gh", &["auth", "token", "--hostname", "github.com"], None)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The GitHub token, resolved once per process: env first, then `gh auth token`.
/// `None` when neither is available — callers then use their `gh` fallback.
pub fn token() -> Option<&'static str> {
    static T: OnceLock<Option<String>> = OnceLock::new();
    T.get_or_init(resolve_token).as_deref()
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

/// GET a paginated REST list, following `per_page=100` pages until a short page
/// or `max` items. `path_with_query` may already carry a `?query`.
fn rest_get_paged(path_with_query: &str, max: usize) -> Result<Vec<Value>> {
    let sep = if path_with_query.contains('?') {
        '&'
    } else {
        '?'
    };
    let mut out = Vec::new();
    let mut page = 1u32;
    loop {
        let p = format!("{path_with_query}{sep}per_page=100&page={page}");
        let Some(v) = rest_get_opt(&p)? else { break };
        let arr = match v.as_array() {
            Some(a) => a.clone(),
            None => break,
        };
        let n = arr.len();
        out.extend(arr);
        if n < 100 || out.len() >= max {
            break;
        }
        page += 1;
    }
    out.truncate(max);
    Ok(out)
}

// --- url parsing -----------------------------------------------------------

/// Parse the PR number out of a `…/pull/<n>` GitHub URL.
pub fn pr_number_from_url(url: &str) -> Option<u64> {
    let tail = url.split("/pull/").nth(1)?;
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

// --- slug ------------------------------------------------------------------

/// Parse `owner/repo` from a GitHub remote URL (ssh, `ssh://`, or https),
/// stripping a trailing `.git`. Pure → unit-tested.
pub fn slug_from_remote_url(url: &str) -> Option<String> {
    let u = url.trim();
    let rest = if let Some(r) = u.strip_prefix("git@") {
        // git@github.com:owner/repo(.git)
        r.split_once(':').map(|(_, p)| p)?
    } else if let Some(r) = u.strip_prefix("ssh://") {
        // ssh://git@github.com/owner/repo(.git)
        r.split_once('/').map(|(_, p)| p)?
    } else {
        // https://github.com/owner/repo(.git)
        let r = u
            .strip_prefix("https://")
            .or_else(|| u.strip_prefix("http://"))?;
        r.split_once('/').map(|(_, p)| p)?
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

/// Whether a git remote URL points at github.com. `slug_from_remote_url` parses
/// any `host/owner/repo` shape without checking the host, so a GitLab origin
/// yields a slug and every downstream caller would query github.com for a
/// repository that is not the project's.
pub fn is_github_remote(url: &str) -> bool {
    let u = url.trim();
    let host = if let Some(rest) = u.strip_prefix("git@") {
        rest.split(':').next().unwrap_or("")
    } else if let Some(rest) = u.split("://").nth(1) {
        let after_user = rest.rsplit('@').next().unwrap_or(rest);
        after_user.split(['/', ':']).next().unwrap_or("")
    } else {
        ""
    };
    host.eq_ignore_ascii_case("github.com")
}

/// The `origin` slug, only when origin is a github.com remote. This is the
/// single entry point for defaulting a repository from the remote, so the host
/// check cannot be skipped by a caller that declared its tracker and therefore
/// never ran detection.
pub fn github_origin_slug(cwd: &str) -> Result<String> {
    let url = crate::cmd::git(&["remote", "get-url", "origin"], cwd)
        .context("reading the `origin` remote")?;
    anyhow::ensure!(
        is_github_remote(&url),
        "`origin` is not a github.com remote ({}); set [github] issues_repo / pr_repo explicitly",
        url.trim()
    );
    slug_from_remote_url(&url)
        .with_context(|| format!("no owner/repo in the origin URL `{}`", url.trim()))
}

/// Where a repository slug came from. A configured or overridden slug is a
/// decision the project made; a defaulted one was read from the remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Configured,
    Overridden,
    Defaulted,
}

/// One resolved repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub slug: String,
    pub origin: Origin,
}

impl Repo {
    /// The `gh --repo` spelling. The host is explicit because `--repo o/r`
    /// leaves `GH_HOST` free to select an enterprise host, which would send a
    /// token to a host it was not issued for.
    pub fn qualified(&self) -> String {
        format!("github.com/{}", self.slug)
    }
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
            Some(slug) => {
                validate_slug(slug)?;
                Ok(Repo {
                    slug: slug.clone(),
                    origin: Origin::Overridden,
                })
            }
            None => repos.prs().cloned(),
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
            .map(|s| (s, Origin::Configured))
            .or_else(|| origin.clone().map(|s| (s, Origin::Defaulted)))
            .ok_or_else(|| missing("issues_repo"))
            .and_then(checked),
        prs: pr_override
            .map(|s| (s.to_string(), Origin::Overridden))
            .or_else(|| cfg.pr_repo.clone().map(|s| (s, Origin::Configured)))
            .or_else(|| origin.map(|s| (s, Origin::Defaulted)))
            .ok_or_else(|| missing("pr_repo"))
            .and_then(checked),
    }
}

/// Validate a resolved slug at the point of resolution, so a bad value is
/// reported against the key that carries it rather than failing later inside a
/// URL or a cache filename.
fn checked((slug, origin): (String, Origin)) -> std::result::Result<Repo, String> {
    match validate_slug(&slug) {
        Ok(()) => Ok(Repo { slug, origin }),
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
               nodes {{ number state url headRefName headRefOid
                        headRepositoryOwner {{ login }} }}
             }} }} }}"#,
        owner = serde_json::Value::from(owner),
        name = serde_json::Value::from(name),
        branch = serde_json::Value::from(branch),
    )
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
        .filter_map(|n| {
            Some(PrBrief {
                number: n["number"].as_u64()?,
                state: n["state"].as_str()?.to_string(),
                url: n["url"].as_str()?.to_string(),
                head_ref_name: n["headRefName"].as_str()?.to_string(),
                head_ref_oid: n["headRefOid"].as_str().unwrap_or("").to_string(),
                head_repo_owner: n["headRepositoryOwner"]["login"]
                    .as_str()
                    .map(str::to_string),
            })
        })
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

/// Every PR in `slug` (any state), up to `max`, for worktree/status matching.
pub fn list_prs(slug: &str, max: usize) -> Result<Vec<PrBrief>> {
    let arr = rest_get_paged(&format!("/repos/{slug}/pulls?state=all"), max)?;
    Ok(arr.iter().filter_map(parse_brief).collect())
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
        assert_eq!(r.issues().unwrap().origin, Origin::Configured);
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
        assert_eq!(r.issues().unwrap().origin, Origin::Defaulted);
        assert_eq!(r.prs().unwrap().slug, "me/fork");

        // A per-invocation override beats pr_repo and is marked as such.
        let r = Repos::from_parts(&cfg(None, Some("up/app")), None, Some("other/x"));
        assert_eq!(r.prs().unwrap().slug, "other/x");
        assert_eq!(r.prs().unwrap().origin, Origin::Overridden);
    }

    #[test]
    fn repos_reject_a_configured_slug_that_is_not_owner_repo() {
        let r = Repos::from_parts(&cfg(Some("../../etc/passwd"), None), None, None);
        let err = r.issues().unwrap_err().to_string();
        assert!(err.contains("owner/repo"), "{err}");
    }

    #[test]
    fn a_repo_qualifies_itself_with_the_host() {
        let r = Repo {
            slug: "o/r".into(),
            origin: Origin::Defaulted,
        };
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
}
