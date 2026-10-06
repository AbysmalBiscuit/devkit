//! GitHub as a forge: github.com, or GitHub Enterprise Server through its host.
//!
//! Single-PR reads and writes go over REST, directly when a token resolves and
//! through `gh api` when it does not, so they work where GitHub refuses
//! GraphQL. Finding a PR by branch and opening one go through GraphQL or `gh`
//! first, which find and push to forks, and fall back to REST. Checkout goes
//! through `gh`, which owns the git-level work the API does not do.

use std::{collections::HashMap, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use super::{
    CheckRun, CheckState, Checks, Forge, ForgeKind, HeadLookup, NewPr, OpenPr, OpenPrPage, PrBrief,
    PrLocator, PrLookup, PrTimeline, Repo, Review, ReviewDecision, ReviewState, Reviewers, Role,
    Section, locate_on,
};
use crate::{
    cmd::{gh_capture, gh_json_in},
    forge::rest::encode,
    github::{Api, Method},
};

pub struct GithubForge {
    api: Api,
}

impl GithubForge {
    pub fn new(host: &str) -> GithubForge {
        GithubForge {
            api: Api::new(host),
        }
    }

    pub fn api(&self) -> &Api {
        &self.api
    }
}

// --- REST parsing ------------------------------------------------------------

fn as_str(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn login(v: &Value) -> Option<String> {
    v.get("login")?.as_str().map(String::from)
}

/// gh's `state` distinguishes MERGED; REST's `state` is only open/closed with a
/// separate `merged_at`. Reconstruct gh's value so callers ranking on MERGED
/// keep working.
fn gh_state(v: &Value) -> String {
    if v.get("merged_at").is_some_and(|m| !m.is_null()) {
        return "MERGED".to_string();
    }
    match v.get("state").and_then(|s| s.as_str()).unwrap_or("") {
        "open" => "OPEN".to_string(),
        "closed" => "CLOSED".to_string(),
        other => other.to_uppercase(),
    }
}

/// A single-PR REST body in the shape devkit reads.
fn parse_brief(v: &Value) -> Option<PrBrief> {
    let head = v.get("head");
    Some(PrBrief {
        number: v.get("number")?.as_u64()?,
        state: gh_state(v),
        url: as_str(v, "html_url"),
        title: as_str(v, "title"),
        head_ref_name: head.map(|h| as_str(h, "ref")).unwrap_or_default(),
        head_ref_oid: head.map(|h| as_str(h, "sha")).unwrap_or_default(),
        head_repo_owner: head
            .and_then(|h| h.get("repo"))
            .and_then(|r| r.get("owner"))
            .and_then(login),
        is_draft: v.get("draft").and_then(|d| d.as_bool()).unwrap_or(false),
        author_login: v.get("user").and_then(login),
    })
}

/// A single-PR REST response mapped to the triage shape. `None` is reserved
/// for a PR that does not exist (a 404, which [`Api::rest`] reports as
/// `None`): a body that came back and could not be parsed is an error, since
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

fn parse_requested_reviewers(v: &Value) -> Vec<String> {
    v.get("users")
        .and_then(|u| u.as_array())
        .into_iter()
        .flatten()
        .filter_map(login)
        .collect()
}

fn parse_submitted_reviewers(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in v.as_array().into_iter().flatten() {
        let Some(l) = r.get("user").and_then(login) else {
            continue;
        };
        if !out.contains(&l) {
            out.push(l);
        }
    }
    out
}

// --- GraphQL PR nodes --------------------------------------------------------

/// The fields every PR node query here selects, and [`parse_pr_node`] reads.
const NODE_FIELDS: &str = "number state url title headRefName headRefOid isDraft \
                           author { login } headRepositoryOwner { login }";

/// One GraphQL `PullRequest` node. `None` when a field the triage shape needs
/// is missing or of the wrong type.
fn parse_pr_node(n: &Value) -> Option<PrBrief> {
    Some(PrBrief {
        number: n["number"].as_u64()?,
        state: n["state"].as_str()?.to_string(),
        url: n["url"].as_str()?.to_string(),
        title: n["title"].as_str().unwrap_or("").to_string(),
        head_ref_name: n["headRefName"].as_str()?.to_string(),
        head_ref_oid: n["headRefOid"].as_str().unwrap_or("").to_string(),
        head_repo_owner: n["headRepositoryOwner"]["login"]
            .as_str()
            .map(str::to_string),
        is_draft: n["isDraft"].as_bool().unwrap_or(false),
        author_login: n["author"]["login"].as_str().map(str::to_string),
    })
}

fn owner_name(slug: &str) -> (Value, Value) {
    let (owner, name) = slug.split_once('/').unwrap_or((slug, ""));
    (Value::from(owner), Value::from(name))
}

/// The `pullRequests` connection selecting every PR whose head is `branch`.
///
/// GraphQL rather than REST: REST documents `head` only as `user:ref-name`, and
/// the head owner cannot be derived. Git allows a push URL distinct from the
/// fetch URL, `remote.pushDefault`, and per-branch push remotes, so `origin`
/// need not be where a branch was pushed. `headRefName` is a documented
/// argument that matches a fork's branch with no owner qualifier.
fn head_connection(branch: &str) -> String {
    format!(
        "pullRequests(headRefName: {}, first: 10, states: [OPEN, CLOSED, MERGED]) \
         {{ totalCount nodes {{ {NODE_FIELDS} }} }}",
        Value::from(branch)
    )
}

/// One GraphQL round trip resolving every branch's PRs, one alias per branch.
fn heads_query(slug: &str, branches: &[String]) -> String {
    let (owner, name) = owner_name(slug);
    let aliases = branches
        .iter()
        .enumerate()
        .map(|(i, b)| format!("b{i}: {}", head_connection(b)))
        .collect::<Vec<_>>()
        .join(" ");
    format!("query {{ repository(owner: {owner}, name: {name}) {{ {aliases} }} }}")
}

/// One `pullRequests` connection as a lookup. `totalCount` beyond the returned
/// nodes is ambiguity, not a unique answer: a winner outside the window would
/// otherwise be silently dropped.
fn parse_connection(conn: &Value) -> HeadLookup {
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

/// Split a `heads_query` response back into one lookup per branch.
fn parse_heads(resp: &Value, branches: &[String]) -> HashMap<String, HeadLookup> {
    branches
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let key = format!("b{i}");
            let conn = &resp["data"]["repository"][&key];
            // A present-but-null or absent alias is a malformed response, not
            // evidence the branch has no PR: the latter is what `issue end`
            // reads before deleting a worktree.
            let lookup = if conn.is_null() {
                HeadLookup::Unavailable(format!(
                    "no `{key}` alias in the GraphQL response for branch `{b}`"
                ))
            } else {
                parse_connection(conn)
            };
            (b.clone(), lookup)
        })
        .collect()
}

/// `(slug, number)` targets grouped by repository in first-seen order, each
/// group carrying its targets' indices. The query builder and the parser both
/// walk this, so they agree on every alias without passing a map between them.
fn group_by_repo(targets: &[(Repo, u64)]) -> Vec<(&str, Vec<usize>)> {
    let mut groups: Vec<(&str, Vec<usize>)> = Vec::new();
    for (i, (repo, _)) in targets.iter().enumerate() {
        match groups.iter_mut().find(|(s, _)| *s == repo.slug) {
            Some((_, idx)) => idx.push(i),
            None => groups.push((&repo.slug, vec![i])),
        }
    }
    groups
}

/// One GraphQL round trip resolving many pull requests by number. Repeated
/// repositories collapse into one `repository` alias, so a cross-repository
/// target costs an extra alias rather than an extra round trip.
fn prs_by_number_query(targets: &[(Repo, u64)]) -> String {
    let repos = group_by_repo(targets)
        .into_iter()
        .enumerate()
        .map(|(g, (slug, idx))| {
            let (owner, name) = owner_name(slug);
            let prs = idx
                .iter()
                .map(|i| {
                    format!(
                        "p{i}: pullRequest(number: {}) {{ {NODE_FIELDS} }}",
                        targets[*i].1
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("r{g}: repository(owner: {owner}, name: {name}) {{ {prs} }}")
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("query {{ {repos} }}")
}

/// Split a `prs_by_number_query` response back into one lookup per target, in
/// the order the targets were given. A null alias is GitHub reporting the
/// repository or pull request as absent (paired with a `NOT_FOUND` error); a
/// missing alias or an unparseable node is a malformed response, which is an
/// error rather than an absence.
fn parse_prs_by_number(resp: &Value, targets: &[(Repo, u64)]) -> Vec<PrLookup> {
    let mut out: Vec<PrLookup> = targets
        .iter()
        .map(|(repo, n)| Err(anyhow::anyhow!("no answer for #{n} in {}", repo.slug)))
        .collect();
    let Some(data) = resp.get("data").filter(|d| !d.is_null()) else {
        return out;
    };
    for (g, (slug, idx)) in group_by_repo(targets).into_iter().enumerate() {
        let repo = match data.get(format!("r{g}")) {
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

// --- timeline ----------------------------------------------------------------

fn qualifier(role: Role) -> &'static str {
    match role {
        Role::Author => "author:@me",
        Role::Reviewer => "reviewed-by:@me",
    }
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

/// `createdAt`/`mergedAt`/`additions`/`deletions`, the same names on a GraphQL
/// node and in `gh pr list --json`.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct TimelineNode {
    created_at: Option<String>,
    merged_at: Option<String>,
    additions: i64,
    deletions: i64,
}

impl From<TimelineNode> for PrTimeline {
    fn from(n: TimelineNode) -> PrTimeline {
        PrTimeline {
            created_at: n.created_at,
            merged_at: n.merged_at,
            additions: n.additions,
            deletions: n.deletions,
        }
    }
}

fn next_cursor(page_info: &Value) -> Option<String> {
    match (
        page_info["hasNextPage"].as_bool(),
        page_info["endCursor"].as_str(),
    ) {
        (Some(true), Some(c)) => Some(c.to_string()),
        _ => None,
    }
}

fn parse_timeline_page(v: &Value) -> (Vec<PrTimeline>, Option<String>) {
    let block = &v["data"]["search"];
    let items = block["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|n| TimelineNode::deserialize(n).unwrap_or_default().into())
        .collect();
    (items, next_cursor(&block["pageInfo"]))
}

// --- open PRs ----------------------------------------------------------------

const OPEN_PR_FIELDS: &str = "number url title headRefName isDraft reviewDecision mergeable \
author { login } \
commits(last: 1) { nodes { commit { statusCheckRollup { state \
contexts(first: 100) { nodes { \
__typename \
... on CheckRun { name status conclusion startedAt } \
... on StatusContext { context state } } } } } } } \
reviews(last: 100) { nodes { author { login } state submittedAt } } \
reviewRequests(first: 100) { nodes { requestedReviewer { ... on User { login } } } }";

fn section_qualifier(section: Section) -> &'static str {
    match section {
        Section::Mine => "author:@me",
        Section::ReviewRequested => "review-requested:@me",
        Section::ReviewedBy => "reviewed-by:@me",
    }
}

/// One page of one section. Small pages keep each request inside GitHub's
/// GraphQL time budget: one request carrying all three sections at
/// `first: 100` asks GitHub to resolve ~90k nodes and times out (HTTP 504) on
/// a repo with many open PRs. The nested per-PR selections stay at 100
/// because the verdict logic reduces over the full set.
fn open_prs_query(slug: &str, section: Section, size: u32, after: Option<&str>) -> String {
    let cursor = match after {
        Some(c) => format!(", after: \"{c}\""),
        None => String::new(),
    };
    format!(
        "query {{ viewer {{ login }} \
search(query: \"repo:{slug} is:pr is:open {}\", type: ISSUE, first: {size}{cursor}) \
{{ pageInfo {{ hasNextPage endCursor }} nodes {{ ... on PullRequest {{ {OPEN_PR_FIELDS} }} }} }} }}",
        section_qualifier(section)
    )
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ActorLogin {
    login: String,
}

/// Deserialize a possibly-null string as the empty string. GitHub returns
/// `submittedAt: null` for a PENDING review, which a bare `String` rejects;
/// `#[serde(default)]` only covers a missing field, not an explicit null.
fn null_as_empty<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ReviewNode {
    author: ActorLogin,
    state: String,
    #[serde(rename = "submittedAt", deserialize_with = "null_as_empty")]
    submitted_at: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ReqNode {
    #[serde(rename = "requestedReviewer")]
    requested_reviewer: Option<ActorLogin>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Rollup {
    state: String,
    contexts: Nodes<RollupContext>,
}

/// One status-check entry under a commit's rollup. GitHub returns a union of
/// `CheckRun` (Actions etc., carrying `name` + `conclusion` + `status`) and
/// `StatusContext` (external statuses, carrying `context` + `state`); both
/// shapes deserialize into this flattened node, with empty fields for the
/// absent half.
#[derive(Deserialize, Default)]
#[serde(default)]
struct RollupContext {
    name: String,
    status: String,
    conclusion: Option<String>,
    #[serde(rename = "startedAt")]
    started_at: Option<String>,
    context: String,
    state: String,
}

const FAIL: [&str; 4] = ["FAILURE", "ERROR", "TIMED_OUT", "CANCELLED"];

/// A rollup or StatusContext state as a check state.
fn status_state(s: &str) -> CheckState {
    match s {
        "SUCCESS" => CheckState::Passed,
        s if FAIL.contains(&s) => CheckState::Failed,
        _ => CheckState::Running,
    }
}

impl RollupContext {
    /// The check's display name, from whichever union half populated it.
    fn name(&self) -> &str {
        if self.name.is_empty() {
            &self.context
        } else {
            &self.name
        }
    }

    fn state(&self) -> CheckState {
        if self.name.is_empty() && !self.context.is_empty() {
            return status_state(&self.state);
        }
        // CheckRun: a non-terminal status is still running; otherwise judge
        // the conclusion. A completed run with no conclusion is running.
        if !self.status.is_empty() && self.status != "COMPLETED" {
            return CheckState::Running;
        }
        match self.conclusion.as_deref() {
            Some("SUCCESS" | "NEUTRAL" | "SKIPPED") => CheckState::Passed,
            // FAILURE, TIMED_OUT, CANCELLED, ACTION_REQUIRED, STARTUP_FAILURE, STALE
            Some(_) => CheckState::Failed,
            None => CheckState::Running,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CommitInner {
    #[serde(rename = "statusCheckRollup")]
    status_check_rollup: Option<Rollup>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CommitNode {
    commit: CommitInner,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct OpenPrNode {
    number: u64,
    url: String,
    title: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "isDraft")]
    is_draft: bool,
    #[serde(rename = "reviewDecision")]
    review_decision: Option<String>,
    mergeable: String,
    author: ActorLogin,
    commits: Nodes<CommitNode>,
    reviews: Nodes<ReviewNode>,
    #[serde(rename = "reviewRequests")]
    review_requests: Nodes<ReqNode>,
}

fn review_state(s: &str) -> ReviewState {
    match s {
        "APPROVED" => ReviewState::Approved,
        "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
        "COMMENTED" => ReviewState::Commented,
        "DISMISSED" => ReviewState::Dismissed,
        _ => ReviewState::Pending,
    }
}

impl From<OpenPrNode> for OpenPr {
    fn from(n: OpenPrNode) -> OpenPr {
        let rollup = n
            .commits
            .nodes
            .into_iter()
            .next()
            .and_then(|c| c.commit.status_check_rollup);
        OpenPr {
            number: n.number,
            url: n.url,
            title: n.title,
            head_ref_name: n.head_ref_name,
            is_draft: n.is_draft,
            review_decision: match n.review_decision.as_deref() {
                Some("APPROVED") => Some(ReviewDecision::Approved),
                Some("CHANGES_REQUESTED") => Some(ReviewDecision::ChangesRequested),
                Some("REVIEW_REQUIRED") => Some(ReviewDecision::ReviewRequired),
                _ => None,
            },
            conflicting: n.mergeable == "CONFLICTING",
            author: n.author.login,
            checks: rollup.map(|r| Checks {
                overall: status_state(&r.state),
                runs: r
                    .contexts
                    .nodes
                    .iter()
                    .map(|c| CheckRun {
                        name: c.name().to_string(),
                        state: c.state(),
                        started_at: c.started_at.clone(),
                    })
                    .collect(),
            }),
            reviews: n
                .reviews
                .nodes
                .into_iter()
                .map(|r| Review {
                    author: r.author.login,
                    state: review_state(&r.state),
                    submitted_at: r.submitted_at,
                })
                .collect(),
            review_requests: n
                .review_requests
                .nodes
                .into_iter()
                .filter_map(|r| r.requested_reviewer.map(|a| a.login))
                .filter(|l| !l.is_empty())
                .collect(),
        }
    }
}

/// One `search` node of an open-PR query as the forge-neutral shape.
pub fn open_pr_from_node(node: &Value) -> Result<OpenPr> {
    Ok(OpenPrNode::deserialize(node)
        .context("unexpected PR shape in the search response")?
        .into())
}

/// An open-PR search response as one page.
fn parse_open_prs_page(v: &Value) -> Result<OpenPrPage> {
    let search = &v["data"]["search"];
    let prs = search["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(open_pr_from_node)
        .collect::<Result<Vec<_>>>()?;
    Ok(OpenPrPage {
        viewer: v["data"]["viewer"]["login"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        prs,
        next: next_cursor(&search["pageInfo"]),
    })
}

// --- gh fallbacks ------------------------------------------------------------

/// `gh` failed because GitHub refused GraphQL outright (HTTP 403), as the
/// Claude Code cloud proxy does, rather than over anything in the request.
fn graphql_refused(e: &anyhow::Error) -> bool {
    crate::cmd::failed_stderr(e).is_some_and(|stderr| {
        let stderr = stderr.to_lowercase();
        stderr.contains("graphql") && stderr.contains("403")
    })
}

/// The `--json` fields a `gh` PR read selects, matching [`GhPr`].
const GH_PR_FIELDS: &str = "number,state,url,title,headRefName,headRefOid,isDraft,author";

/// One PR as `gh pr list/view --json` reports it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPr {
    number: u64,
    state: String,
    url: String,
    #[serde(default)]
    title: String,
    head_ref_name: String,
    #[serde(default)]
    head_ref_oid: String,
    #[serde(default)]
    is_draft: bool,
    /// Absent, or with no login, for an account that no longer exists.
    #[serde(default)]
    author: Option<GhLogin>,
}

#[derive(Deserialize)]
struct GhLogin {
    #[serde(default)]
    login: Option<String>,
}

impl From<GhPr> for PrBrief {
    fn from(p: GhPr) -> PrBrief {
        PrBrief {
            number: p.number,
            state: p.state,
            url: p.url,
            title: p.title,
            head_ref_name: p.head_ref_name,
            head_ref_oid: p.head_ref_oid,
            head_repo_owner: None,
            is_draft: p.is_draft,
            author_login: p.author.and_then(|a| a.login),
        }
    }
}

impl GithubForge {
    fn gh_prs_by_head(&self, repo: &Repo, branch: &str) -> Result<HeadLookup> {
        let found: Vec<GhPr> = gh_json_in(
            &[
                "pr",
                "list",
                "--head",
                branch,
                "--state",
                "all",
                "--json",
                GH_PR_FIELDS,
            ],
            repo,
            ".",
        )?;
        Ok(HeadLookup::of(found.into_iter().map(Into::into).collect()))
    }

    fn rest_found(&self, path: &str) -> Result<Value> {
        self.api
            .rest(Method::GET, path, None)?
            .with_context(|| format!("GitHub returned 404 for {path}"))
    }

    /// Every PR whose head is `branch` in `repo` itself. REST qualifies a head
    /// by its owner, so a fork's branch is not found here.
    fn rest_prs_by_head(&self, repo: &Repo, branch: &str) -> Result<HeadLookup> {
        let (owner, _) = repo.slug.split_once('/').unwrap_or((&repo.slug, ""));
        let v = self.rest_found(&format!(
            "/repos/{}/pulls?head={}&state=all&per_page=100",
            repo.slug,
            encode(&format!("{owner}:{branch}"))
        ))?;
        let found = v
            .as_array()
            .context("a PR list that is not an array")?
            .iter()
            .map(|p| parse_brief(p).context("unexpected PR shape in the PR list"))
            .collect::<Result<Vec<_>>>()?;
        Ok(HeadLookup::of(found))
    }

    /// `POST /pulls`, then the reviewer request. A ready PR is opened ready,
    /// so no GraphQL-only draft flip follows.
    fn rest_create(&self, repo: &Repo, pr: &NewPr<'_>) -> Result<String> {
        let created = self
            .api
            .rest(
                Method::POST,
                &format!("/repos/{}/pulls", repo.slug),
                Some(&serde_json::json!({
                    "title": pr.title,
                    "head": pr.head,
                    "base": pr.base,
                    "body": pr.body,
                    "draft": pr.draft,
                })),
            )?
            .context("GitHub returned 404 creating the PR")?;
        let brief = parse_brief(&created).context("unexpected PR shape from the create")?;
        if !pr.reviewers.is_empty() {
            self.add_reviewers(repo, brief.number, pr.reviewers)
                .with_context(|| format!("{} is open", brief.url))?;
        }
        Ok(brief.url)
    }

    fn graphql_or_gh(&self, query: &str) -> Result<Value> {
        self.api
            .graphql(query)
            .or_else(|_| self.api.gh_graphql(query))
    }
}

impl Forge for GithubForge {
    fn kind(&self) -> ForgeKind {
        ForgeKind::Github
    }

    fn host(&self) -> &str {
        self.api.host()
    }

    fn ready(&self) -> bool {
        self.api.token().is_some()
    }

    /// The viewer over REST, through `gh` when no token resolves.
    fn check(&self) -> Result<String> {
        let user = self
            .rest_found("/user")
            .with_context(|| format!("no GitHub access ({})", self.api.token_hint()))?;
        let login = login(&user).context("no viewer login in GitHub response")?;
        Ok(format!("github: {login} ({})", self.api.host()))
    }

    fn locate(&self, url: &str) -> Option<PrLocator> {
        locate_on(url, self.api.host(), "/pull/")
    }

    fn pr(&self, repo: &Repo, n: u64) -> Result<Option<PrBrief>> {
        let body = self
            .api
            .rest(
                Method::GET,
                &format!("/repos/{}/pulls/{n}", repo.slug),
                None,
            )
            .with_context(|| format!("reading PR #{n} in {}", repo.slug))?;
        brief_of_response(body, n, &repo.slug)
    }

    fn prs(&self, targets: &[(Repo, u64)]) -> Result<Vec<PrLookup>> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        anyhow::ensure!(self.api.token().is_some(), "no GitHub token resolved");
        let v = self.api.graphql_partial(&prs_by_number_query(targets))?;
        Ok(parse_prs_by_number(&v, targets))
    }

    /// GraphQL when a token resolves, else `gh pr list --head`, else REST,
    /// which finds no fork's branch. Only a transport that could not answer
    /// moves on: a definite "no PR" is an answer and is trusted, or the
    /// fallback re-asks a resolved question and can return a different PR.
    fn pr_by_head(&self, repo: &Repo, branch: &str) -> HeadLookup {
        let api = match self.api.token() {
            None => "no GitHub token resolved".to_string(),
            Some(_) => {
                let (owner, name) = owner_name(&repo.slug);
                let query = format!(
                    "query {{ repository(owner: {owner}, name: {name}) {{ {} }} }}",
                    head_connection(branch)
                );
                match self.api.graphql(&query) {
                    Ok(v) => return parse_connection(&v["data"]["repository"]["pullRequests"]),
                    Err(e) => format!("{e:#}"),
                }
            }
        };
        let gh = match self.gh_prs_by_head(repo, branch) {
            Ok(found) => return found,
            Err(e) => format!("{e:#}"),
        };
        self.rest_prs_by_head(repo, branch)
            .unwrap_or_else(|e| HeadLookup::Unavailable(format!("{api}; gh: {gh}; REST: {e:#}")))
    }

    /// One GraphQL round trip for every branch. No `gh` fallback: a status
    /// report over many worktrees marks each branch unknown instead of
    /// spawning `gh` once per branch.
    fn prs_by_head(&self, repo: &Repo, branches: &[String]) -> HashMap<String, HeadLookup> {
        let unavailable = |reason: &str| {
            branches
                .iter()
                .map(|b| (b.clone(), HeadLookup::Unavailable(reason.to_string())))
                .collect()
        };
        if branches.is_empty() {
            return HashMap::new();
        }
        if self.api.token().is_none() {
            return unavailable("no GitHub token resolved");
        }
        match self.api.graphql(&heads_query(&repo.slug, branches)) {
            Ok(v) => parse_heads(&v, branches),
            Err(e) => unavailable(&format!("{e:#}")),
        }
    }

    fn attaches_media(&self) -> bool {
        true
    }

    /// `gh pr create` picks the head from the checkout it runs in, which is
    /// how it finds a branch pushed to a fork. It also resolves each
    /// attachment's path against `cwd`. Where GitHub refuses the GraphQL it
    /// runs on, the PR is opened over REST instead, which uploads nothing.
    fn create(&self, repo: &Repo, pr: &NewPr<'_>, cwd: &Path) -> Result<String> {
        let joined = pr.reviewers.join(",");
        let mut args = vec![
            "pr", "create", "--base", pr.base, "--title", pr.title, "--body", pr.body,
        ];
        if !pr.reviewers.is_empty() {
            args.extend(["--reviewer", &joined]);
        }
        if pr.draft {
            args.push("--draft");
        }
        for file in pr.attachments {
            args.extend(["--attach", file]);
        }
        let out = match gh_capture(&args, repo, &cwd.to_string_lossy()) {
            Ok(out) => out,
            Err(e) if graphql_refused(&e) && pr.attachments.is_empty() => {
                return self.rest_create(repo, pr);
            }
            Err(e) => return Err(e).context("gh pr create failed"),
        };
        out.lines()
            .rev()
            .find(|l| l.contains("://"))
            .map(|l| l.trim().to_string())
            .context("could not parse a PR URL from `gh pr create` output")
    }

    fn mark_ready(&self, repo: &Repo, n: u64) -> Result<()> {
        gh_capture(&["pr", "ready", &n.to_string()], repo, ".").context("gh pr ready failed")?;
        Ok(())
    }

    fn add_reviewers(&self, repo: &Repo, n: u64, logins: &[String]) -> Result<()> {
        self.api
            .rest(
                Method::POST,
                &format!("/repos/{}/pulls/{n}/requested_reviewers", repo.slug),
                Some(&serde_json::json!({ "reviewers": logins })),
            )
            .and_then(|found| found.with_context(|| format!("PR #{n} not found in {}", repo.slug)))
            .context("requesting reviewers failed")?;
        Ok(())
    }

    fn reviewers(&self, repo: &Repo, n: u64) -> Result<Reviewers> {
        let pulls = format!("/repos/{}/pulls/{n}", repo.slug);
        Ok(Reviewers {
            requested: parse_requested_reviewers(
                &self.rest_found(&format!("{pulls}/requested_reviewers"))?,
            ),
            submitted: parse_submitted_reviewers(&self.rest_found(&format!("{pulls}/reviews"))?),
        })
    }

    fn checkout(&self, repo: &Repo, pr: &PrBrief, dir: &Path) -> Result<()> {
        gh_capture(
            &["pr", "checkout", &pr.number.to_string()],
            repo,
            &dir.to_string_lossy(),
        )?;
        Ok(())
    }

    fn open_prs(
        &self,
        repo: &Repo,
        section: Section,
        page_size: u32,
        cursor: Option<&str>,
    ) -> Result<OpenPrPage> {
        let v = self.graphql_or_gh(&open_prs_query(&repo.slug, section, page_size, cursor))?;
        parse_open_prs_page(&v)
    }

    /// Over GraphQL when a token resolves, paginated up to `max`; otherwise
    /// `gh pr list --search`.
    fn timeline(&self, repo: &Repo, role: Role, max: usize) -> Result<Vec<PrTimeline>> {
        if self.api.token().is_none() {
            let found: Vec<TimelineNode> = gh_json_in(
                &[
                    "pr",
                    "list",
                    "--search",
                    qualifier(role),
                    "--state",
                    "all",
                    "--limit",
                    &max.to_string(),
                    "--json",
                    "createdAt,mergedAt,additions,deletions",
                ],
                repo,
                ".",
            )?;
            return Ok(found.into_iter().map(Into::into).collect());
        }
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let v = self.api.graphql(&timeline_query(
                &repo.slug,
                qualifier(role),
                after.as_deref(),
            ))?;
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
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn repo(slug: &str) -> Repo {
        Repo {
            host: "github.com".into(),
            slug: slug.into(),
        }
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
            "html_url": "https://github.com/a/b/pull/42", "title": "Fix",
            "head": { "ref": "you/eng-1-foo", "sha": "abc", "repo": { "owner": { "login": "you" } } },
            "user": { "login": "bob" }, "draft": true
        });
        let b = parse_brief(&v).unwrap();
        assert_eq!(b.number, 42);
        assert_eq!(b.state, "MERGED");
        assert_eq!(b.url, "https://github.com/a/b/pull/42");
        assert_eq!(b.title, "Fix");
        assert_eq!(b.head_ref_name, "you/eng-1-foo");
        assert_eq!(b.head_ref_oid, "abc");
        assert_eq!(b.head_repo_owner.as_deref(), Some("you"));
        assert_eq!(b.author_login.as_deref(), Some("bob"));
        assert!(b.is_draft);
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

    #[test]
    fn requested_reviewers_reads_user_logins() {
        let v = json!({ "users": [{"login": "alice"}, {"login": "carol"}], "teams": [] });
        assert_eq!(parse_requested_reviewers(&v), vec!["alice", "carol"]);
        assert!(parse_requested_reviewers(&json!({})).is_empty());
    }

    #[test]
    fn parse_submitted_reviewers_dedupes_and_skips_empty_logins() {
        let v = json!([
            { "user": { "login": "igoracc" }, "state": "COMMENTED" },
            { "user": { "login": "igoracc" }, "state": "APPROVED" },
            { "user": null, "state": "APPROVED" }
        ]);
        assert_eq!(parse_submitted_reviewers(&v), vec!["igoracc".to_string()]);
    }

    fn targets() -> Vec<(Repo, u64)> {
        vec![
            (repo("o/r"), 12),
            (repo("o/r"), 13),
            (repo("me/fork"), 9),
            (repo("gone/repo"), 1),
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
        for alias in [
            "p0: pullRequest(number: 12)",
            "p1: pullRequest(number: 13)",
            "p2: pullRequest(number: 9)",
            "p3: pullRequest(number: 1)",
        ] {
            assert!(q.contains(alias), "{q}");
        }
    }

    #[test]
    fn a_batch_response_separates_resolved_missing_and_malformed() {
        let resp = json!({
            "data": {
                "r0": {
                    "p0": {
                        "number": 12, "state": "OPEN", "title": "t",
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

    fn conn(nodes: &str, total: u32) -> Value {
        serde_json::from_str(&format!(r#"{{"totalCount":{total},"nodes":[{nodes}]}}"#)).unwrap()
    }

    const NODE_A: &str = r#"{"number":185,"state":"OPEN","url":"https://github.com/up/app/pull/185",
      "headRefName":"fix/glyph-overhang","headRefOid":"aaaa111",
      "headRepositoryOwner":{"login":"contributor"}}"#;
    const NODE_B: &str = r#"{"number":42,"state":"MERGED","url":"https://github.com/up/app/pull/42",
      "headRefName":"fix/glyph-overhang","headRefOid":"bbbb222",
      "headRepositoryOwner":{"login":"someone-else"}}"#;

    /// A fork's head still parses to a unique answer: the head owner differs
    /// from the searched repository's owner and the match must still be
    /// found, which is why the lookup is GraphQL rather than REST.
    #[test]
    fn one_node_parses_to_unique_even_from_a_fork() {
        let HeadLookup::Unique(pr) = parse_connection(&conn(NODE_A, 1)) else {
            panic!("expected Unique")
        };
        assert_eq!(pr.number, 185);
        assert_eq!(pr.head_ref_oid, "aaaa111");
        assert_eq!(pr.head_repo_owner.as_deref(), Some("contributor"));
    }

    #[test]
    fn zero_nodes_parse_to_no_match_and_two_to_ambiguous() {
        assert!(matches!(
            parse_connection(&conn("", 0)),
            HeadLookup::NoMatch
        ));
        assert!(matches!(
            parse_connection(&conn(&format!("{NODE_A},{NODE_B}"), 2)),
            HeadLookup::Ambiguous(c) if c.len() == 2
        ));
    }

    /// One node returned but the server says there are three: ranking a
    /// truncated set is exactly the false-unique this type exists to prevent.
    #[test]
    fn a_total_count_beyond_the_window_is_ambiguous_not_unique() {
        assert!(matches!(
            parse_connection(&conn(NODE_A, 3)),
            HeadLookup::Ambiguous(_)
        ));
    }

    #[test]
    fn heads_are_batched_one_alias_per_branch() {
        let q = heads_query("o/r", &["feat/a".into(), "fix/b".into()]);
        assert!(
            q.contains("b0: pullRequests(headRefName: \"feat/a\""),
            "{q}"
        );
        assert!(q.contains("b1: pullRequests(headRefName: \"fix/b\""), "{q}");
        assert_eq!(q.matches("repository(").count(), 1, "one round trip");
    }

    /// A malformed or truncated response is a lookup that could not be made,
    /// not evidence the branch has no PR: the latter is what `issue end` reads
    /// before deleting a worktree.
    #[test]
    fn a_missing_alias_is_unavailable_not_no_match() {
        let resp = json!({"data":{"repository":{"b0":{"totalCount":0,"nodes":[]}}}});
        let got = parse_heads(&resp, &["feat/a".into(), "fix/b".into()]);
        assert!(matches!(got["feat/a"], HeadLookup::NoMatch));
        assert!(matches!(&got["fix/b"], HeadLookup::Unavailable(r) if r.contains("fix/b")));
    }

    #[test]
    fn every_pr_query_selects_the_fields_the_brief_reads() {
        for q in [
            prs_by_number_query(&targets()),
            heads_query("o/r", &["feat/x".into()]),
        ] {
            for field in ["isDraft", "title", "headRefOid"] {
                assert!(q.contains(field), "{field} missing: {q}");
            }
        }
    }

    #[test]
    fn parse_pr_node_reads_the_draft_flag() {
        let n = json!({
            "number": 7, "state": "OPEN", "url": "u7",
            "headRefName": "feat/x", "headRefOid": "abc123", "isDraft": true
        });
        assert!(parse_pr_node(&n).unwrap().is_draft);
        let n = json!({
            "number": 7, "state": "OPEN", "url": "u7",
            "headRefName": "feat/x", "headRefOid": "abc123"
        });
        assert!(!parse_pr_node(&n).unwrap().is_draft);
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

        let v = json!({ "data": { "search": {
            "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null }
        }}});
        assert_eq!(parse_timeline_page(&v), (Vec::new(), None));
    }

    #[test]
    fn timeline_query_scopes_repo_and_qualifier() {
        let q = timeline_query("acme/mono", "author:@me", None);
        assert!(q.contains("repo:acme/mono is:pr author:@me"));
        assert!(!q.contains("after:"));
        assert!(timeline_query("a/b", "reviewed-by:@me", Some("X")).contains("after: \"X\""));
    }

    #[test]
    fn open_prs_query_carries_size_section_and_cursor() {
        let q = open_prs_query("o/r", Section::ReviewRequested, 25, Some("C"));
        assert!(
            q.contains("repo:o/r is:pr is:open review-requested:@me"),
            "{q}"
        );
        assert!(q.contains("first: 25"), "{q}");
        assert!(q.contains("after: \"C\""), "{q}");
    }

    #[test]
    fn an_open_pr_node_maps_onto_the_neutral_shape() {
        let pr = open_pr_from_node(&json!({
            "number": 10, "url": "u10", "title": "t", "headRefName": "lev/eng-1-foo",
            "isDraft": false, "reviewDecision": "APPROVED", "mergeable": "CONFLICTING",
            "author": {"login": "me"},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {
                "state": "FAILURE",
                "contexts": {"nodes": [
                    {"name": "build", "status": "COMPLETED", "conclusion": "FAILURE", "startedAt": "2026-01-01T00:00:00Z"},
                    {"context": "deploy", "state": "PENDING"}
                ]}
            }}}]},
            "reviews": {"nodes": [{"author": {"login": "alice"}, "state": "APPROVED", "submittedAt": null}]},
            "reviewRequests": {"nodes": [{"requestedReviewer": {"login": "bob"}}, {"requestedReviewer": {}}]}
        }))
        .unwrap();
        assert_eq!(pr.review_decision, Some(ReviewDecision::Approved));
        assert!(pr.conflicting);
        let checks = pr.checks.unwrap();
        assert_eq!(checks.overall, CheckState::Failed);
        assert_eq!(checks.runs[0].name, "build");
        assert_eq!(checks.runs[0].state, CheckState::Failed);
        assert_eq!(checks.runs[1].name, "deploy");
        assert_eq!(checks.runs[1].state, CheckState::Running);
        assert_eq!(pr.reviews[0].state, ReviewState::Approved);
        assert_eq!(pr.reviews[0].submitted_at, "");
        assert_eq!(pr.review_requests, vec!["bob"]);
    }

    #[test]
    fn an_enterprise_pr_url_is_located_on_its_host_only() {
        let f = GithubForge::new("ghe.acme.test");
        let loc = f.locate("https://ghe.acme.test/o/r/pull/7").unwrap();
        assert_eq!(loc.repo.as_deref(), Some("o/r"));
        assert!(f.locate("https://github.com/o/r/pull/7").is_none());
    }
}
