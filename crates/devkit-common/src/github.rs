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
    crate::cmd::capture("gh", &["auth", "token"], None)
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
    } else if let Some(r) = u
        .strip_prefix("https://")
        .or_else(|| u.strip_prefix("http://"))
    {
        r.split_once('/').map(|(_, p)| p)?
    } else {
        return None;
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let mut it = rest.split('/');
    let owner = it.next().filter(|s| !s.is_empty())?;
    let repo = it.next().filter(|s| !s.is_empty())?;
    Some(format!("{owner}/{repo}"))
}

/// `owner/repo` for the repo at `cwd`, from its `origin` remote URL. No HTTP —
/// this replaces a `gh repo view` spawn.
pub fn repo_slug(cwd: &str) -> Result<String> {
    let url = crate::cmd::git(&["remote", "get-url", "origin"], cwd)?;
    slug_from_remote_url(&url)
        .with_context(|| format!("cannot parse owner/repo from origin remote: {}", url.trim()))
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
}
