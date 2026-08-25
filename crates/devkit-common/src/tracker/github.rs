//! The GitHub Issues adapter.
//!
//! Mirrors `linear.rs`'s split: every operation is a `*_query` string builder, a
//! `parse_*` function over the response, and a networked wrapper. Only the
//! wrappers touch the network, so each parser tests against a recorded response
//! and nothing here needs a token under test.

use super::{AssignedIssue, IssueDetails, IssueRef, PrRef, State, StateKind, Tracker, TrackerKind};
use crate::github::{self, Repo};
use anyhow::{Context, Result};
use std::collections::HashMap;

/// GitHub's `(state, stateReason)` pair, in devkit's vocabulary.
///
/// `OPEN` maps to `Started` rather than `Unstarted`: GitHub gives no signal
/// separating a backlog issue from one in progress, and deriving one from
/// assignee presence would invent bands the data cannot support.
pub fn map_state(state: &str, reason: Option<&str>) -> State {
    let (kind, name) = match (state, reason) {
        ("OPEN", _) => (StateKind::Started, "Open"),
        ("CLOSED", Some("NOT_PLANNED")) => (StateKind::Canceled, "Not planned"),
        ("CLOSED", Some("DUPLICATE")) => (StateKind::Canceled, "Duplicate"),
        ("CLOSED", _) => (StateKind::Completed, "Done"),
        _ => (StateKind::Unstarted, "Unknown"),
    };
    State {
        kind,
        name: name.into(),
        color: None,
    }
}

// --- one issue's details ----------------------------------------------------

/// GraphQL fetching everything [`IssueDetails`] carries for one issue.
pub fn issue_query(slug: &str, number: u64) -> String {
    let (owner, name) = slug.split_once('/').unwrap_or((slug, ""));
    format!(
        r#"query {{ repository(owner: {o}, name: {n}) {{ issue(number: {number}) {{
             title url body state stateReason
             assignees(first: 10) {{ pageInfo {{ hasNextPage }} nodes {{ login }} }}
             labels(first: 20) {{ pageInfo {{ hasNextPage }} nodes {{ name }} }}
           }} }} }}"#,
        o = serde_json::Value::from(owner),
        n = serde_json::Value::from(name),
    )
}

/// The details from an `issue_query` response. `None` when the repository or
/// issue does not exist.
///
/// Neither `assignees` nor `labels` is paginated: a connection truncated by
/// its window reads as a partial list with `…` appended, rather than as a
/// complete one that happens to be short.
pub fn parse_issue(resp: &serde_json::Value, id: &str) -> Option<IssueDetails> {
    let node = &resp["data"]["repository"]["issue"];
    let title = node["title"].as_str()?.to_string();
    let state = map_state(
        node["state"].as_str().unwrap_or(""),
        node["stateReason"].as_str(),
    );
    let mut assignees: Vec<&str> = node["assignees"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n["login"].as_str())
        .collect();
    if node["assignees"]["pageInfo"]["hasNextPage"]
        .as_bool()
        .unwrap_or(false)
    {
        assignees.push("…");
    }
    let assignee = assignees.join(", ");
    let mut labels: Vec<String> = node["labels"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n["name"].as_str().map(String::from))
        .collect();
    if node["labels"]["pageInfo"]["hasNextPage"]
        .as_bool()
        .unwrap_or(false)
    {
        labels.push("…".to_string());
    }
    Some(IssueDetails {
        id: id.to_string(),
        title,
        url: node["url"].as_str().unwrap_or("").to_string(),
        description: node["body"].as_str().unwrap_or("").to_string(),
        state: state.name,
        assignee,
        priority: String::new(),
        estimate: String::new(),
        labels,
        parent: String::new(),
        project: String::new(),
    })
}

// --- batched states ---------------------------------------------------------

/// Build the batched GraphQL query for the given issue numbers, aliasing one
/// `issue(number: …)` field per id. Ids that do not parse as a plain issue
/// number are dropped: every alias rides in one request, so one malformed
/// alias would cost the states of all the others. Returns `None` when no id
/// survived.
pub fn states_query(slug: &str, ids: &[String]) -> Option<(String, HashMap<String, String>)> {
    let (owner, name) = slug.split_once('/').unwrap_or((slug, ""));
    let mut aliases = HashMap::new();
    let mut parts = Vec::new();
    for id in ids {
        let Ok(n) = id.parse::<u64>() else { continue };
        let alias = format!("i{}", parts.len());
        aliases.insert(alias.clone(), id.clone());
        parts.push(format!(
            "{alias}: issue(number: {n}) {{ state stateReason }}"
        ));
    }
    if parts.is_empty() {
        return None;
    }
    Some((
        format!(
            "query {{ repository(owner: {o}, name: {n}) {{ {parts} }} }}",
            o = serde_json::Value::from(owner),
            n = serde_json::Value::from(name),
            parts = parts.join(" "),
        ),
        aliases,
    ))
}

/// From a `states_query` response: id → state, keyed through `aliases`. An
/// alias with no `state` (a deleted issue behind a stale number) has no entry.
pub fn parse_states(
    resp: &serde_json::Value,
    aliases: &HashMap<String, String>,
) -> HashMap<String, State> {
    let mut out = HashMap::new();
    let Some(repo) = resp["data"]["repository"].as_object() else {
        return out;
    };
    for (alias, block) in repo {
        let (Some(id), Some(state)) = (aliases.get(alias), block["state"].as_str()) else {
            continue;
        };
        out.insert(id.clone(), map_state(state, block["stateReason"].as_str()));
    }
    out
}

// --- the issue's linked pull request ----------------------------------------

/// One PR linked to an issue, with the repository it lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedPr {
    pub number: u64,
    pub state: String,
    pub url: String,
    pub repo: String,
}

/// The outcome of choosing among an issue's linked PRs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkedChoice {
    None,
    One(PrRef),
    /// Candidates tied on state and spanning repositories, where numbers have
    /// no shared ordering.
    Ambiguous(Vec<PrRef>),
    /// The connection had more nodes than the window returned.
    Truncated,
}

/// `closedByPullRequestsReferences` answers directly, in one field.
///
/// `ConnectedEvent` never fires (it records a manual Development-sidebar link
/// nobody uses), and `willCloseTarget` goes false once the issue closes,
/// losing the PR for exactly the closed issues the finished verdict reads.
pub fn issue_pr_query(slug: &str, number: u64) -> String {
    let (owner, name) = slug.split_once('/').unwrap_or((slug, ""));
    format!(
        r#"query {{ repository(owner: {o}, name: {n}) {{ issue(number: {number}) {{
             closedByPullRequestsReferences(first: 10, includeClosedPrs: true,
                                            orderByState: true) {{
               pageInfo {{ hasNextPage }}
               nodes {{ number state url repository {{ nameWithOwner }} }}
             }} }} }} }}"#,
        o = serde_json::Value::from(owner),
        n = serde_json::Value::from(name),
    )
}

/// State first — merged, then open, then closed — and within the top state
/// group by number, highest first.
fn state_rank(s: &str) -> u8 {
    match s {
        "MERGED" => 3,
        "OPEN" => 2,
        "CLOSED" => 1,
        _ => 0,
    }
}

/// Choose among an issue's linked PRs.
pub fn rank_linked(prs: &[LinkedPr]) -> LinkedChoice {
    let Some(top) = prs.iter().map(|p| state_rank(&p.state)).max() else {
        return LinkedChoice::None;
    };
    let group: Vec<&LinkedPr> = prs.iter().filter(|p| state_rank(&p.state) == top).collect();
    let spans_repos = group.iter().any(|p| p.repo != group[0].repo);
    if group.len() > 1 && spans_repos {
        return LinkedChoice::Ambiguous(
            group
                .iter()
                .map(|p| PrRef {
                    url: p.url.clone(),
                    number: p.number,
                })
                .collect(),
        );
    }
    let winner = group
        .into_iter()
        .max_by_key(|p| p.number)
        .expect("group is non-empty");
    LinkedChoice::One(PrRef {
        url: winner.url.clone(),
        number: winner.number,
    })
}

/// Parse and rank in one step. A truncated connection refuses.
pub fn parse_issue_pr(resp: &serde_json::Value) -> LinkedChoice {
    let conn = &resp["data"]["repository"]["issue"]["closedByPullRequestsReferences"];
    if conn["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false) {
        return LinkedChoice::Truncated;
    }
    let prs: Vec<LinkedPr> = conn["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| {
            Some(LinkedPr {
                number: n["number"].as_u64()?,
                state: n["state"].as_str()?.to_string(),
                url: n["url"].as_str()?.to_string(),
                repo: n["repository"]["nameWithOwner"].as_str()?.to_string(),
            })
        })
        .collect();
    rank_linked(&prs)
}

// --- assigned issues, with their state-transition history ------------------

/// GraphQL for one page of issues assigned to `login`, with each issue's
/// closed/reopened history nested in the same round trip — so walking pages
/// of issues stays one paginated round trip per page.
///
/// `filterBy.assignee` takes a concrete login: GitHub's `@me` shorthand exists
/// only in `search`, not in `Repository.issues`, so the caller resolves the
/// viewer's own login first rather than passing the repository owner.
pub fn assigned_query(slug: &str, login: &str, after: Option<&str>) -> String {
    let (owner, name) = slug.split_once('/').unwrap_or((slug, ""));
    let cursor = after
        .map(|c| format!(", after: {}", serde_json::Value::from(c)))
        .unwrap_or_default();
    format!(
        r#"query {{ repository(owner: {o}, name: {n}) {{
             issues(first: 20{cursor}, filterBy: {{ assignee: {login} }}) {{
               pageInfo {{ hasNextPage endCursor }}
               nodes {{ number createdAt state stateReason
                 timelineItems(first: 50, itemTypes: [CLOSED_EVENT, REOPENED_EVENT]) {{
                   pageInfo {{ hasNextPage endCursor }}
                   nodes {{ __typename
                            ... on ClosedEvent {{ createdAt stateReason }}
                            ... on ReopenedEvent {{ createdAt }} }}
                 }} }} }} }} }}"#,
        o = serde_json::Value::from(owner),
        n = serde_json::Value::from(name),
        login = serde_json::Value::from(login),
    )
}

/// A `(when, from, to)` state transition parsed from one timeline event.
type Transition = (String, Option<State>, Option<State>);

/// One `timelineItems.nodes[]` entry as a `(when, from, to)` transition, or
/// `None` for an event type outside the two requested.
fn parse_timeline_transition(n: &serde_json::Value) -> Option<Transition> {
    let created_at = n["createdAt"].as_str()?.to_string();
    match n["__typename"].as_str()? {
        "ClosedEvent" => Some((
            created_at,
            Some(map_state("OPEN", None)),
            Some(map_state("CLOSED", n["stateReason"].as_str())),
        )),
        "ReopenedEvent" => Some((
            created_at,
            Some(map_state("CLOSED", None)),
            Some(map_state("OPEN", None)),
        )),
        _ => None,
    }
}

/// One page of [`assigned_query`], plus which issues' nested timeline was
/// truncated. A connection nested inside a paginated one does not paginate
/// with its parent, so a truncated inner list is reported as `(issue number,
/// cursor)` rather than silently dropped — the caller fetches the rest with a
/// follow-up per-issue query.
pub fn parse_assigned(resp: &serde_json::Value) -> (Vec<AssignedIssue>, Vec<(String, String)>) {
    let mut issues = Vec::new();
    let mut more = Vec::new();
    for n in resp["data"]["repository"]["issues"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let Some(number) = n["number"].as_u64() else {
            continue;
        };
        let id = number.to_string();
        let timeline = &n["timelineItems"];
        let history: Vec<_> = timeline["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(parse_timeline_transition)
            .collect();
        if timeline["pageInfo"]["hasNextPage"]
            .as_bool()
            .unwrap_or(false)
            && let Some(cursor) = timeline["pageInfo"]["endCursor"].as_str()
        {
            more.push((id.clone(), cursor.to_string()));
        }
        issues.push(AssignedIssue {
            identifier: id,
            created_at: n["createdAt"].as_str().unwrap_or("").to_string(),
            state: map_state(n["state"].as_str().unwrap_or(""), n["stateReason"].as_str()),
            history,
        });
    }
    (issues, more)
}

/// One page of a single issue's remaining `timelineItems`, continuing past a
/// cursor an outer [`assigned_query`] page could not fit.
fn timeline_page_query(slug: &str, number: u64, after: &str) -> String {
    let (owner, name) = slug.split_once('/').unwrap_or((slug, ""));
    format!(
        r#"query {{ repository(owner: {o}, name: {n}) {{ issue(number: {number}) {{
             timelineItems(first: 50, after: {after}, itemTypes: [CLOSED_EVENT, REOPENED_EVENT]) {{
               pageInfo {{ hasNextPage endCursor }}
               nodes {{ __typename
                        ... on ClosedEvent {{ createdAt stateReason }}
                        ... on ReopenedEvent {{ createdAt }} }}
             }} }} }} }}"#,
        o = serde_json::Value::from(owner),
        n = serde_json::Value::from(name),
        after = serde_json::Value::from(after),
    )
}

/// One page of a single issue's remaining `timelineItems`: the transitions on
/// the page, plus the next cursor when the connection has one.
pub fn parse_timeline_page(resp: &serde_json::Value) -> (Vec<Transition>, Option<String>) {
    let block = &resp["data"]["repository"]["issue"]["timelineItems"];
    let transitions = block["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_timeline_transition)
        .collect();
    let next = match (
        block["pageInfo"]["hasNextPage"].as_bool(),
        block["pageInfo"]["endCursor"].as_str(),
    ) {
        (Some(true), Some(c)) => Some(c.to_string()),
        _ => None,
    };
    (transitions, next)
}

/// Fetch the rest of one issue's timeline past `cursor`, appending each page's
/// transitions onto the matching entry in `issues`.
fn fill_remaining_timeline(
    slug: &str,
    number: &str,
    mut cursor: String,
    issues: &mut [AssignedIssue],
) -> Result<()> {
    let n: u64 = number
        .parse()
        .with_context(|| format!("bad issue number {number}"))?;
    loop {
        let resp = github::graphql(&timeline_page_query(slug, n, &cursor))?;
        let (extra, next) = parse_timeline_page(&resp);
        if let Some(issue) = issues.iter_mut().find(|i| i.identifier == number) {
            issue.history.extend(extra);
        }
        match next {
            Some(c) => cursor = c,
            None => return Ok(()),
        }
    }
}

/// The authenticated user's own login. `filterBy.assignee` needs a concrete
/// value; GitHub's `@me` shorthand does not extend to it.
fn viewer_login() -> Result<String> {
    let resp = github::graphql("query { viewer { login } }")?;
    resp["data"]["viewer"]["login"]
        .as_str()
        .map(String::from)
        .context("no viewer login in GitHub response")
}

// --- each PR's closing issues ------------------------------------------------

/// Batch of aliased `resource(url:)` lookups, one per PR URL, chunked to keep
/// any single request bounded. `resource` resolves a URL to a node without
/// needing its owner/repo split out, so PRs from unrelated repositories batch
/// together in one request.
pub fn issues_for_prs_queries(urls: &[String]) -> Vec<(String, HashMap<String, String>)> {
    urls.chunks(25)
        .map(|chunk| {
            let mut aliases = HashMap::new();
            let mut parts = Vec::new();
            for (i, url) in chunk.iter().enumerate() {
                let alias = format!("p{i}");
                aliases.insert(alias.clone(), url.clone());
                parts.push(format!(
                    r#"{alias}: resource(url: {u}) {{ ... on PullRequest {{
                         closingIssuesReferences(first: 20) {{
                           pageInfo {{ hasNextPage }}
                           nodes {{ number repository {{ nameWithOwner }} }}
                         }} }} }}"#,
                    u = serde_json::Value::from(url.as_str()),
                ));
            }
            (format!("query {{ {} }}", parts.join(" ")), aliases)
        })
        .collect()
}

/// From one `issues_for_prs_queries` response: PR url → the ids of the issues
/// it closes. An issue in `slug`, the tracker's own issues repository, is a
/// bare number; one anywhere else is `owner/name#number`, since GitHub lets a
/// PR close an issue across a repository boundary and a bare number there
/// would name a different issue. The match is case-insensitive: GitHub returns
/// a repository's canonical casing, while `slug` came verbatim from config or
/// an origin remote. A node with no `repository` stays bare — a missing field
/// is not evidence of a different repository.
///
/// A connection reporting `hasNextPage` is dropped rather than kept partial —
/// a partial link list is worse than none, since it feeds a column that is
/// better blank than wrong. A URL with no closing issues gets no entry.
pub fn parse_issues_for_prs(
    resp: &serde_json::Value,
    aliases: &HashMap<String, String>,
    slug: &str,
) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    let Some(data) = resp["data"].as_object() else {
        return out;
    };
    for (alias, block) in data {
        let Some(url) = aliases.get(alias) else {
            continue;
        };
        let conn = &block["closingIssuesReferences"];
        if conn["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false) {
            continue;
        }
        let ids: Vec<String> = conn["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|n| {
                let number = n["number"].as_u64()?;
                Some(match n["repository"]["nameWithOwner"].as_str() {
                    Some(other) if !other.eq_ignore_ascii_case(slug) => {
                        format!("{other}#{number}")
                    }
                    _ => number.to_string(),
                })
            })
            .collect();
        if !ids.is_empty() {
            out.insert(url.clone(), ids);
        }
    }
    out
}

// --- the repository's earliest issue -----------------------------------------

/// The single oldest issue in the repository, by creation date.
pub fn timeline_origin_query(slug: &str) -> String {
    let (owner, name) = slug.split_once('/').unwrap_or((slug, ""));
    format!(
        r#"query {{ repository(owner: {o}, name: {n}) {{
             issues(first: 1, orderBy: {{ field: CREATED_AT, direction: ASC }}) {{
               nodes {{ createdAt }}
             }} }} }}"#,
        o = serde_json::Value::from(owner),
        n = serde_json::Value::from(name),
    )
}

/// The earliest issue's `createdAt` from a `timeline_origin_query` response,
/// or `None` when the repository has no issues at all.
pub fn parse_timeline_origin(resp: &serde_json::Value) -> Option<String> {
    resp["data"]["repository"]["issues"]["nodes"][0]["createdAt"]
        .as_str()
        .map(String::from)
}

// --- issue URLs / ids --------------------------------------------------------

/// The `owner/repo` and issue number in a `.../issues/<n>` GitHub URL, or
/// `None` when the string is not that shape.
fn parse_issue_url(s: &str) -> Option<(String, u64)> {
    let rest = s
        .trim()
        .strip_prefix("https://github.com/")
        .or_else(|| s.trim().strip_prefix("http://github.com/"))?;
    let mut it = rest.split('/');
    let owner = it.next().filter(|s| !s.is_empty())?;
    let name = it.next().filter(|s| !s.is_empty())?;
    if it.next()? != "issues" {
        return None;
    }
    let tail = it.next()?;
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    let number = digits.parse().ok()?;
    Some((format!("{owner}/{name}"), number))
}

// --- the adapter -------------------------------------------------------------

pub struct GithubTracker {
    repo: Repo,
}

impl GithubTracker {
    pub fn new(repo: Repo) -> GithubTracker {
        GithubTracker { repo }
    }
}

impl Tracker for GithubTracker {
    fn kind(&self) -> TrackerKind {
        TrackerKind::Github
    }

    /// Readiness needs only a resolved token: the issues repository was
    /// already resolved to construct this tracker, so there is nothing left
    /// to check that a project naming its own repositories would still be
    /// missing.
    fn ready(&self) -> bool {
        github::token().is_some()
    }

    fn issue_ref(&self, input: &str) -> Result<IssueRef> {
        let s = input.trim();
        if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
            return Ok(IssueRef {
                id: s.into(),
                slug: None,
            });
        }
        let (repo, number) = parse_issue_url(s)
            .with_context(|| format!("unrecognized GitHub issue identifier: {s}"))?;
        anyhow::ensure!(
            repo.eq_ignore_ascii_case(&self.repo.slug),
            "issue {number} is in {repo}, but this project's issues repository is {}",
            self.repo.slug
        );
        Ok(IssueRef {
            id: number.to_string(),
            slug: None,
        })
    }

    fn title(&self, id: &str) -> Result<Option<String>> {
        Ok(self.details(id)?.map(|d| d.title))
    }

    fn details(&self, id: &str) -> Result<Option<IssueDetails>> {
        let n: u64 = id
            .parse()
            .with_context(|| format!("bad issue number {id}"))?;
        let resp = github::graphql_partial(&issue_query(&self.repo.slug, n))?;
        Ok(parse_issue(&resp, id))
    }

    fn states(&self, ids: &[String]) -> HashMap<String, State> {
        let Some((query, aliases)) = states_query(&self.repo.slug, ids) else {
            return HashMap::new();
        };
        match github::graphql_partial(&query) {
            Ok(resp) => parse_states(&resp, &aliases),
            Err(e) => {
                eprintln!("GitHub lookup failed: {e:#}");
                HashMap::new()
            }
        }
    }

    fn issue_pr(&self, id: &str) -> Result<Option<PrRef>> {
        let n: u64 = id
            .parse()
            .with_context(|| format!("bad issue number {id}"))?;
        let resp = github::graphql(&issue_pr_query(&self.repo.slug, n))?;
        match parse_issue_pr(&resp) {
            LinkedChoice::None => Ok(None),
            LinkedChoice::One(p) => Ok(Some(p)),
            LinkedChoice::Ambiguous(c) => anyhow::bail!(
                "issue {id} has several linked PRs in different repositories: {}",
                c.iter()
                    .map(|p| p.url.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            LinkedChoice::Truncated => {
                anyhow::bail!("issue {id} has more linked PRs than one page holds")
            }
        }
    }

    /// Empty: on a GitHub project a bare number is a PR, and that is the
    /// tracker's answer rather than the absence of an environment variable.
    fn candidates(&self, _n: u64) -> Result<Vec<IssueRef>> {
        Ok(Vec::new())
    }

    fn issues_for_prs(&self, urls: &[String]) -> HashMap<String, Vec<String>> {
        let mut out = HashMap::new();
        for (query, aliases) in issues_for_prs_queries(urls) {
            match github::graphql(&query) {
                Ok(resp) => out.extend(parse_issues_for_prs(&resp, &aliases, &self.repo.slug)),
                Err(e) => {
                    eprintln!("GitHub PR-link lookup failed: {e:#}");
                    break;
                }
            }
        }
        out
    }

    fn assigned_history(&self, on_page: &mut dyn FnMut(usize)) -> Result<Vec<AssignedIssue>> {
        let login = viewer_login()?;
        let mut out: Vec<AssignedIssue> = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let resp = github::graphql(&assigned_query(&self.repo.slug, &login, after.as_deref()))?;
            let (mut issues, more) = parse_assigned(&resp);
            for (number, cursor) in more {
                fill_remaining_timeline(&self.repo.slug, &number, cursor, &mut issues)?;
            }
            out.append(&mut issues);
            on_page(out.len());
            let page = &resp["data"]["repository"]["issues"]["pageInfo"];
            match (page["hasNextPage"].as_bool(), page["endCursor"].as_str()) {
                (Some(true), Some(c)) => after = Some(c.to_string()),
                _ => return Ok(out),
            }
        }
    }

    /// The repository's own oldest issue, rather than the viewer's account
    /// creation date: the dashboard timeline is a project's history, and a
    /// contributor's account routinely predates the project by years.
    fn timeline_origin(&self) -> Result<Option<String>> {
        let resp = github::graphql(&timeline_origin_query(&self.repo.slug))?;
        Ok(parse_timeline_origin(&resp))
    }

    /// An `owner/name#number` id names an issue outside this tracker's issues
    /// repository — `issues_for_prs` emits that form for a PR closing an issue
    /// across a repository boundary — and links to the repository it names. A
    /// bare number is an issue here.
    fn issue_url(&self, id: &str) -> Option<String> {
        let (slug, number) = id.split_once('#').unwrap_or((&self.repo.slug, id));
        Some(format!("https://github.com/{slug}/issues/{number}"))
    }

    fn check(&self) -> Result<String> {
        github::token()
            .context("no GitHub token (set GH_TOKEN/GITHUB_TOKEN or run `gh auth login`)")?;
        let login = viewer_login()?;
        Ok(format!("github: {login} ({})", self.repo.slug))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(slug: &str) -> Repo {
        Repo {
            slug: slug.to_string(),
            origin: github::Origin::Configured,
        }
    }

    fn linked(number: u64, state: &str, repo: &str) -> LinkedPr {
        LinkedPr {
            number,
            state: state.to_string(),
            url: format!("https://github.com/{repo}/pull/{number}"),
            repo: repo.to_string(),
        }
    }

    fn fixture(name: &str) -> serde_json::Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/tracker/fixtures")
            .join(name);
        let data = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading fixture {}: {e}", path.display()));
        serde_json::from_str(&data)
            .unwrap_or_else(|e| panic!("parsing fixture {}: {e}", path.display()))
    }

    #[test]
    fn every_state_and_reason_pair_maps() {
        // NOT_PLANNED and DUPLICATE are synthetic: neither probed repository holds
        // one. stateReason is a closed enum the API documents, and a wrong mapping
        // degrades to a state label rather than a crash.
        for (state, reason, kind, name) in [
            ("OPEN", None, StateKind::Started, "Open"),
            ("CLOSED", Some("COMPLETED"), StateKind::Completed, "Done"),
            (
                "CLOSED",
                Some("NOT_PLANNED"),
                StateKind::Canceled,
                "Not planned",
            ),
            (
                "CLOSED",
                Some("DUPLICATE"),
                StateKind::Canceled,
                "Duplicate",
            ),
            ("CLOSED", None, StateKind::Completed, "Done"),
        ] {
            let s = map_state(state, reason);
            assert_eq!(s.kind, kind, "{state}/{reason:?}");
            assert_eq!(s.name, name, "{state}/{reason:?}");
        }
    }

    #[test]
    fn an_open_issue_parses_its_state_and_assignee() {
        let d = parse_issue(&fixture("gh_issue_open.json"), "6").unwrap();
        assert_eq!(d.id, "6");
        assert_eq!(d.title, "Bug: crash on startup");
        assert_eq!(d.state, "Open");
        assert_eq!(d.assignee, "contributor");
        assert_eq!(d.labels, vec!["bug".to_string(), "P1".to_string()]);
    }

    #[test]
    fn a_truncated_assignees_or_labels_connection_is_marked_not_dropped() {
        // Neither connection paginates; a list wider than the window must stay
        // visible as incomplete rather than silently reading as the whole list.
        let resp = serde_json::json!({ "data": { "repository": { "issue": {
            "title": "t", "state": "OPEN",
            "assignees": {
                "pageInfo": { "hasNextPage": true },
                "nodes": [{ "login": "alice" }]
            },
            "labels": {
                "pageInfo": { "hasNextPage": true },
                "nodes": [{ "name": "bug" }]
            }
        } } } });
        let d = parse_issue(&resp, "1").unwrap();
        assert_eq!(d.assignee, "alice, …");
        assert_eq!(d.labels, vec!["bug".to_string(), "…".to_string()]);
    }

    #[test]
    fn a_closed_issue_with_no_assignee_parses_to_empty_fields() {
        let d = parse_issue(&fixture("gh_issue_closed.json"), "12").unwrap();
        assert_eq!(d.state, "Done");
        assert_eq!(d.assignee, "");
        assert!(d.labels.is_empty());
    }

    #[test]
    fn states_query_aliases_each_number() {
        let (q, a) = states_query("me/widget", &["6".into(), "12".into()]).unwrap();
        assert!(q.contains("issue(number: 6)"));
        assert!(q.contains("issue(number: 12)"));
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn states_query_drops_non_numeric_ids() {
        let (q, a) = states_query("me/widget", &["not-a-number".into(), "6".into()]).unwrap();
        assert!(q.contains("issue(number: 6)"));
        assert_eq!(a.len(), 1);
    }

    #[test]
    fn no_numeric_ids_no_query() {
        assert!(states_query("me/widget", &["nope".into()]).is_none());
    }

    #[test]
    fn parse_states_maps_aliases_to_ids() {
        let resp = serde_json::json!({
            "data": { "repository": {
                "i0": { "state": "OPEN", "stateReason": null },
                "i1": { "state": "CLOSED", "stateReason": "COMPLETED" }
            } }
        });
        let mut aliases = HashMap::new();
        aliases.insert("i0".to_string(), "6".to_string());
        aliases.insert("i1".to_string(), "12".to_string());
        let states = parse_states(&resp, &aliases);
        assert_eq!(states["6"].kind, StateKind::Started);
        assert_eq!(states["12"].kind, StateKind::Completed);
    }

    #[test]
    fn a_cross_repository_link_is_returned_not_filtered() {
        // A linked PR is routinely in another repository — the ordinary fork
        // workflow. Filtering to PRs in the same repo would have reported
        // these issues as having no PR at all.
        let resp = fixture("gh_issue_cross_repo.json");
        let LinkedChoice::One(pr) = parse_issue_pr(&resp) else {
            panic!("expected one linked PR")
        };
        assert_eq!(pr.url, "https://github.com/upstream/widget/pull/185");
        assert_eq!(pr.number, 185);
    }

    #[test]
    fn no_link_parses_to_none() {
        assert!(matches!(
            parse_issue_pr(&fixture("gh_issue_no_pr.json")),
            LinkedChoice::None
        ));
    }

    #[test]
    fn an_issue_only_cross_referenced_by_another_issue_has_no_linked_pr() {
        // The query asks only for closedByPullRequestsReferences; a mention
        // from another issue never appears there, so it parses the same as no
        // link at all.
        assert!(matches!(
            parse_issue_pr(&fixture("gh_issue_only_issue_xref.json")),
            LinkedChoice::None
        ));
    }

    #[test]
    fn a_truncated_connection_is_refused_rather_than_ranked() {
        // A ranked window is worthless if the winner sits outside it, and a tie
        // that looks unique only because the second candidate was truncated is
        // worse than a visible tie.
        let mut resp = fixture("gh_issue_cross_repo.json");
        resp["data"]["repository"]["issue"]["closedByPullRequestsReferences"]["pageInfo"]["hasNextPage"] =
            serde_json::Value::Bool(true);
        assert!(matches!(parse_issue_pr(&resp), LinkedChoice::Truncated));
    }

    #[test]
    fn two_merged_prs_in_one_repository_rank_by_number() {
        // Number ordering only means something inside one repository, and there
        // it is a total order — the higher number is the later attempt, not a
        // tie.
        let c = rank_linked(&[
            linked(10, "MERGED", "me/widget"),
            linked(12, "MERGED", "me/widget"),
        ]);
        let LinkedChoice::One(pr) = c else {
            panic!("expected a ranked winner, got {c:?}")
        };
        assert_eq!(pr.number, 12);
    }

    #[test]
    fn two_merged_prs_across_repositories_are_ambiguous() {
        // #5 upstream is not "older" than #900 in a fork: the numbers are
        // unrelated.
        let c = rank_linked(&[
            linked(5, "MERGED", "upstream/widget"),
            linked(900, "MERGED", "me/widget"),
        ]);
        assert!(
            matches!(c, LinkedChoice::Ambiguous(ref v) if v.len() == 2),
            "{c:?}"
        );
    }

    #[test]
    fn a_merged_pr_beats_an_open_one() {
        let c = rank_linked(&[
            linked(3, "OPEN", "me/widget"),
            linked(1, "MERGED", "me/widget"),
        ]);
        let LinkedChoice::One(pr) = c else { panic!() };
        assert_eq!(pr.number, 1);
    }

    #[test]
    fn assigned_history_filters_on_the_viewer_login_not_the_repository_owner() {
        // filterBy takes a concrete login and has no @me. In the probed
        // repository every assigned issue belongs to the contributor, so
        // filtering on the repository owner returned nothing at all.
        let q = assigned_query("K-Nette/Widget", "contributor", None);
        assert!(q.contains(r#"assignee: "contributor""#), "{q}");
        assert!(!q.contains(r#"assignee: "K-Nette""#), "{q}");
        // The timeline nests inside the same query, so it stays one paginated
        // round trip per page.
        assert!(
            q.contains("CLOSED_EVENT") && q.contains("REOPENED_EVENT"),
            "{q}"
        );
    }

    #[test]
    fn a_nested_timeline_walk_appends_pages_in_order_and_terminates() {
        // fill_remaining_timeline loops parse_timeline_page across a real
        // multi-page walk: page one still has more, page two ends it.
        let (mut transitions, next) = parse_timeline_page(&fixture("gh_timeline_page_1.json"));
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].0, "2026-03-01T00:00:00Z");
        assert_eq!(next.as_deref(), Some("cursorY"));

        let (page_two, next) = parse_timeline_page(&fixture("gh_timeline_page_2.json"));
        transitions.extend(page_two);
        assert_eq!(next, None);

        assert_eq!(transitions.len(), 2);
        assert_eq!(transitions[0].0, "2026-03-01T00:00:00Z");
        assert_eq!(transitions[1].0, "2026-03-02T00:00:00Z");
    }

    #[test]
    fn a_truncated_nested_timeline_is_paginated_not_cut() {
        // A connection nested inside a paginated one does not paginate with its
        // parent, so walking the outer pages truncates each inner list silently
        // — and a chart missing transitions looks entirely normal.
        let resp = fixture("gh_assigned_history.json");
        let (issues, more) = parse_assigned(&resp);
        assert_eq!(issues.len(), 2);
        assert_eq!(more, vec![("7".to_string(), "cursorX".to_string())]);
    }

    #[test]
    fn an_issue_url_outside_the_configured_repository_is_refused() {
        // IssueRef is shared with Linear, and widening it for a field only
        // GitHub fills would push GitHub's repository question into Linear's
        // type. The tracker is scoped to one repository by construction, so an
        // issue outside it is unanswerable rather than merely inconvenient.
        let t = GithubTracker::new(repo("me/widget"));
        let err = t
            .issue_ref("https://github.com/other/thing/issues/9")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("other/thing") && err.contains("me/widget"),
            "{err}"
        );
        // A `Defaulted` origin never wrote a `[github] issues_repo` key, so the
        // message must not send the reader to edit a setting they don't have.
        assert!(!err.contains("[github]"), "{err}");

        assert_eq!(t.issue_ref("9").unwrap().id, "9");
        assert_eq!(
            t.issue_ref("https://github.com/me/widget/issues/9")
                .unwrap()
                .id,
            "9"
        );
    }

    #[test]
    fn an_issue_url_matches_the_configured_repository_case_blind() {
        // A slug comes verbatim from `[github] issues_repo` or an origin
        // remote, while a pasted URL carries GitHub's canonical casing. GitHub
        // treats the two as one repository, so the comparison must too.
        let t = GithubTracker::new(repo("acme/widget"));
        assert_eq!(
            t.issue_ref("https://github.com/acme/Widget/issues/42")
                .unwrap()
                .id,
            "42"
        );
    }

    #[test]
    fn issues_for_prs_query_resolves_each_url_through_the_resource_field() {
        let urls = vec![
            "https://github.com/o/r/pull/1".to_string(),
            "https://github.com/o/r/pull/2".to_string(),
        ];
        let batches = issues_for_prs_queries(&urls);
        assert_eq!(batches.len(), 1);
        let (q, aliases) = &batches[0];
        assert!(q.contains("resource(url:"), "{q}");
        assert!(q.contains("closingIssuesReferences"), "{q}");
        assert_eq!(aliases.len(), 2);
    }

    #[test]
    fn issues_for_prs_queries_chunk_at_25() {
        let urls: Vec<String> = (0..30)
            .map(|i| format!("https://github.com/o/r/pull/{i}"))
            .collect();
        let batches = issues_for_prs_queries(&urls);
        assert_eq!(batches.len(), 2);
        assert!(issues_for_prs_queries(&[]).is_empty());
    }

    #[test]
    fn parse_issues_for_prs_collects_closing_issue_numbers() {
        let resp = serde_json::json!({
            "data": {
                "p0": { "closingIssuesReferences": {
                    "pageInfo": { "hasNextPage": false },
                    "nodes": [
                        { "number": 6, "repository": { "nameWithOwner": "o/r" } },
                        { "number": 7, "repository": { "nameWithOwner": "o/r" } }
                    ]
                } },
                "p1": { "closingIssuesReferences": {
                    "pageInfo": { "hasNextPage": false },
                    "nodes": []
                } }
            }
        });
        let mut aliases = HashMap::new();
        aliases.insert(
            "p0".to_string(),
            "https://github.com/o/r/pull/1".to_string(),
        );
        aliases.insert(
            "p1".to_string(),
            "https://github.com/o/r/pull/2".to_string(),
        );
        let got = parse_issues_for_prs(&resp, &aliases, "o/r");
        assert_eq!(
            got["https://github.com/o/r/pull/1"],
            vec!["6".to_string(), "7".to_string()]
        );
        assert!(!got.contains_key("https://github.com/o/r/pull/2"));
    }

    #[test]
    fn a_closing_issue_outside_the_tracker_repository_carries_its_own() {
        let resp = serde_json::json!({
            "data": { "p0": { "closingIssuesReferences": {
                "pageInfo": { "hasNextPage": false },
                "nodes": [
                    { "number": 6, "repository": { "nameWithOwner": "o/r" } },
                    { "number": 42, "repository": { "nameWithOwner": "other/repo" } }
                ]
            } } }
        });
        let mut aliases = HashMap::new();
        aliases.insert(
            "p0".to_string(),
            "https://github.com/o/r/pull/1".to_string(),
        );
        let got = parse_issues_for_prs(&resp, &aliases, "o/r");
        assert_eq!(
            got["https://github.com/o/r/pull/1"],
            vec!["6".to_string(), "other/repo#42".to_string()]
        );
    }

    #[test]
    fn a_closing_issue_repository_matches_its_slug_case_insensitively() {
        // GitHub returns the canonical casing of a repository name, while the
        // slug came verbatim from `[github] issues_repo` or an origin remote.
        // Comparing them literally would qualify every issue in the project's
        // own repository for anyone whose config disagrees on case.
        let resp = serde_json::json!({
            "data": { "p0": { "closingIssuesReferences": {
                "pageInfo": { "hasNextPage": false },
                "nodes": [{ "number": 6, "repository": { "nameWithOwner": "O/R" } }]
            } } }
        });
        let mut aliases = HashMap::new();
        aliases.insert(
            "p0".to_string(),
            "https://github.com/o/r/pull/1".to_string(),
        );
        let got = parse_issues_for_prs(&resp, &aliases, "o/r");
        assert_eq!(got["https://github.com/o/r/pull/1"], vec!["6".to_string()]);
    }

    #[test]
    fn a_closing_issue_node_without_a_repository_stays_bare() {
        let resp = serde_json::json!({
            "data": { "p0": { "closingIssuesReferences": {
                "pageInfo": { "hasNextPage": false },
                "nodes": [{ "number": 6 }]
            } } }
        });
        let mut aliases = HashMap::new();
        aliases.insert(
            "p0".to_string(),
            "https://github.com/o/r/pull/1".to_string(),
        );
        let got = parse_issues_for_prs(&resp, &aliases, "o/r");
        assert_eq!(got["https://github.com/o/r/pull/1"], vec!["6".to_string()]);
    }

    #[test]
    fn issue_url_follows_a_qualified_id_to_its_own_repository() {
        let t = GithubTracker::new(repo("me/widget"));
        assert_eq!(
            t.issue_url("42").as_deref(),
            Some("https://github.com/me/widget/issues/42")
        );
        assert_eq!(
            t.issue_url("other/repo#42").as_deref(),
            Some("https://github.com/other/repo/issues/42")
        );
    }

    #[test]
    fn issues_for_prs_query_asks_for_each_closing_issues_repository() {
        let urls = vec!["https://github.com/o/r/pull/1".to_string()];
        let (q, _) = issues_for_prs_queries(&urls).remove(0);
        assert!(q.contains("nameWithOwner"), "{q}");
    }

    #[test]
    fn parse_issues_for_prs_ignores_unknown_aliases() {
        let resp = serde_json::json!({ "data": { "zzz": { "closingIssuesReferences": {
            "pageInfo": { "hasNextPage": false }, "nodes": [{ "number": 6 }]
        } } } });
        assert!(parse_issues_for_prs(&resp, &HashMap::new(), "o/r").is_empty());
    }

    #[test]
    fn a_truncated_closing_issues_connection_reports_incomplete_not_partial() {
        // A partial link list is worse than none: it feeds a column that is
        // better blank than wrong, so a truncated connection is dropped
        // entirely rather than reported as this PR's whole answer.
        let resp = serde_json::json!({
            "data": { "p0": { "closingIssuesReferences": {
                "pageInfo": { "hasNextPage": true },
                "nodes": [{ "number": 6 }]
            } } }
        });
        let mut aliases = HashMap::new();
        aliases.insert(
            "p0".to_string(),
            "https://github.com/o/r/pull/1".to_string(),
        );
        assert!(parse_issues_for_prs(&resp, &aliases, "o/r").is_empty());
    }

    #[test]
    fn timeline_origin_query_orders_issues_by_created_at_ascending() {
        let q = timeline_origin_query("me/widget");
        assert!(
            q.contains("orderBy: { field: CREATED_AT, direction: ASC }"),
            "{q}"
        );
        assert!(q.contains("first: 1"), "{q}");
    }

    #[test]
    fn timeline_origin_reads_the_earliest_issues_created_at() {
        let resp = serde_json::json!({ "data": { "repository": { "issues": {
            "nodes": [{ "createdAt": "2020-01-01T00:00:00Z" }]
        } } } });
        assert_eq!(
            parse_timeline_origin(&resp),
            Some("2020-01-01T00:00:00Z".to_string())
        );
    }

    #[test]
    fn no_issues_yields_no_timeline_origin() {
        let resp = serde_json::json!({ "data": { "repository": { "issues": { "nodes": [] } } } });
        assert_eq!(parse_timeline_origin(&resp), None);
    }
}
