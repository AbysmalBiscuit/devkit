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

/// POST a raw GraphQL query to `api.github.com/graphql`. The response envelope
/// is returned whole (`{ "data": … }`); a non-empty `errors` array is an error.
pub fn graphql(query: &str) -> Result<Value> {
    let _span = crate::timing::io_span("github graphql", "graphql").entered();
    let v: Value = agent()
        .post(&format!("{API}/graphql"))
        .set("Authorization", &bearer()?)
        .set("User-Agent", UA)
        .send_json(ureq::json!({ "query": query }))?
        .into_json()?;
    if let Some(errors) = v.get("errors").and_then(|e| e.as_array())
        && !errors.is_empty()
    {
        let msg = errors
            .first()
            .and_then(|e| e["message"].as_str())
            .unwrap_or("unknown GraphQL error");
        anyhow::bail!("GitHub GraphQL error: {msg}");
    }
    Ok(v)
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
        let origin = need_origin.then(|| github_origin_slug(cwd)).transpose();
        let origin = match origin {
            Ok(v) => v,
            Err(e) => {
                let msg = format!("{e:#}");
                return Repos {
                    issues: cfg
                        .issues_repo
                        .clone()
                        .map(|s| (s, Origin::Configured))
                        .ok_or_else(|| msg.clone())
                        .and_then(checked),
                    prs: pr_override
                        .map(|s| (s.to_string(), Origin::Overridden))
                        .or_else(|| cfg.pr_repo.clone().map(|s| (s, Origin::Configured)))
                        .ok_or(msg)
                        .and_then(checked),
                };
            }
        };
        Repos::from_parts(cfg, origin, pr_override)
    }

    /// `resolve` with the origin slug supplied rather than read, so resolution
    /// is testable without a git remote.
    #[doc(hidden)]
    pub fn from_parts(
        cfg: &devkit_config::GithubConfig,
        origin: Option<String>,
        pr_override: Option<&str>,
    ) -> Repos {
        let missing = |key: &str| {
            format!(
                "no GitHub repository for {key}: set [github] {key} or give the project a \
                 github.com `origin` remote"
            )
        };
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

/// The most recent PR whose head branch is `branch` (any state), or `None`.
/// `branch` is qualified with the repo owner, matching devkit's in-repo branches.
pub fn pr_by_head(slug: &str, branch: &str) -> Result<Option<PrBrief>> {
    let owner = slug.split('/').next().unwrap_or("");
    let path = format!(
        "/repos/{slug}/pulls?head={owner}:{branch}&state=all&per_page=1&sort=created&direction=desc"
    );
    let v = rest_get(&path)?;
    Ok(v.as_array().and_then(|a| a.first()).and_then(parse_brief))
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
}
