//! Linear behind the tracker seam: the GraphQL free functions, plus the
//! [`LinearTracker`] adapter that presents them as a [`Tracker`].

use super::{AssignedIssue, IssueRef, PrRef, State, Tracker, TrackerKind};
use anyhow::{Context, Result};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearIdentity {
    /// `organization.urlKey` — also persisted as `linear_workspace`.
    pub workspace_url_key: String,
    pub org_name: String,
    pub viewer_email: String,
}

/// A GitHub PR linked to a Linear issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearPr {
    pub url: String,
    pub number: u64,
}

/// A Linear issue candidate from a by-number lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearIssueRef {
    pub id: String, // "ENG-42"
    pub title: String,
}

/// Split a `TEAM-NUMBER` id into its uppercased team key and issue number, or
/// None when it is not a Linear id.
///
/// Both parts are spliced into GraphQL literals, so this is also the gate that
/// keeps a query well-formed: the team key must be alphanumeric (no quote to
/// break out of the string), and the number must be a canonical `Int` — a
/// leading zero (`number: { eq: 01 }`, from a branch like `env-config-01`) is a
/// syntax error that Linear rejects with a 500, taking every other alias in a
/// batched query down with it.
fn parse_id(id: &str) -> Option<(String, u64)> {
    let (team, num) = id.split_once('-')?;
    if !team.starts_with(|c: char| c.is_ascii_alphabetic())
        || !team.chars().all(|c| c.is_ascii_alphanumeric())
        || num.starts_with('0')
    {
        return None;
    }
    Some((team.to_uppercase(), num.parse().ok()?))
}

/// GraphQL fetching one issue's title + GitHub PR attachments. Returns None
/// for ids that are not in `TEAM-NUMBER` form.
pub fn issue_pr_query(id: &str) -> Option<String> {
    let (team, num) = parse_id(id)?;
    Some(format!(
        "query {{ issues(filter: {{ team: {{ key: {{ eq: \"{team}\" }} }}, number: {{ eq: {num} }} }}) \
         {{ nodes {{ title attachments {{ nodes {{ url }} }} }} }} }}",
    ))
}

/// From an `issue_pr_query` response, the first GitHub PR attachment + the title.
pub fn parse_issue_pr(resp: &serde_json::Value) -> (Option<LinearPr>, String) {
    let node = &resp["data"]["issues"]["nodes"][0];
    let title = node["title"].as_str().unwrap_or("").to_string();
    let pr = node["attachments"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["url"].as_str())
        .find(|u| u.contains("github.com") && u.contains("/pull/"))
        .and_then(|u| {
            crate::github::pr_number_from_url(u).map(|number| LinearPr {
                url: u.to_string(),
                number,
            })
        });
    (pr, title)
}

/// Resolve a Linear id to its attached GitHub PR + the issue title.
pub fn issue_pr(id: &str, key: &str) -> Result<(Option<LinearPr>, String)> {
    let query = issue_pr_query(id).context("not a TEAM-NUMBER Linear id")?;
    let resp = post_graphql(&query, key, "issue_pr")?;
    Ok(parse_issue_pr(&resp))
}

/// GraphQL fetching one issue's title alone. Returns None for ids that are not
/// in `TEAM-NUMBER` form.
pub fn issue_title_query(id: &str) -> Option<String> {
    let (team, num) = parse_id(id)?;
    Some(format!(
        "query {{ issues(filter: {{ team: {{ key: {{ eq: \"{team}\" }} }}, number: {{ eq: {num} }} }}) \
         {{ nodes {{ title }} }} }}",
    ))
}

/// The title from an `issue_title_query` response. None when the issue does not
/// exist or carries no title.
pub fn parse_issue_title(resp: &serde_json::Value) -> Option<String> {
    resp["data"]["issues"]["nodes"][0]["title"]
        .as_str()
        .filter(|t| !t.trim().is_empty())
        .map(str::to_string)
}

/// Resolve a Linear id to its issue title.
pub fn issue_title(id: &str, key: &str) -> Result<Option<String>> {
    let query = issue_title_query(id).context("not a TEAM-NUMBER Linear id")?;
    let resp = post_graphql(&query, key, "issue_title")?;
    Ok(parse_issue_title(&resp))
}

/// One issue's Linear-side facts, as `issue setup --summary` writes them into a
/// summary file. Every optional field is `None` when Linear has nothing there,
/// so a template can tell "no assignee" from an empty name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueDetails {
    pub identifier: String,
    pub title: String,
    pub url: String,
    /// Markdown, verbatim. Empty when the issue has no description.
    pub description: String,
    pub state: Option<String>,
    pub assignee: Option<String>,
    /// Linear's own words for the priority ("High", "No priority").
    pub priority: Option<String>,
    pub estimate: Option<String>,
    pub labels: Vec<String>,
    /// `IDENT \u{2014} title` of the parent issue.
    pub parent: Option<String>,
    pub project: Option<String>,
}

/// GraphQL fetching everything [`IssueDetails`] carries. Returns None for ids
/// that are not in `TEAM-NUMBER` form.
pub fn issue_details_query(id: &str) -> Option<String> {
    let (team, num) = parse_id(id)?;
    Some(format!(
        "query {{ issues(filter: {{ team: {{ key: {{ eq: \"{team}\" }} }}, number: {{ eq: {num} }} }}) \
         {{ nodes {{ identifier title url description priorityLabel estimate \
         state {{ name }} assignee {{ name }} labels {{ nodes {{ name }} }} \
         parent {{ identifier title }} project {{ name }} }} }} }}",
    ))
}

/// A non-empty string at `node[key]`, or None.
fn text(node: &serde_json::Value, key: &str) -> Option<String> {
    node[key]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The details from an `issue_details_query` response. None when the issue does
/// not exist.
pub fn parse_issue_details(resp: &serde_json::Value) -> Option<IssueDetails> {
    let n = &resp["data"]["issues"]["nodes"][0];
    let identifier = text(n, "identifier")?;
    Some(IssueDetails {
        identifier,
        title: text(n, "title").unwrap_or_default(),
        url: text(n, "url").unwrap_or_default(),
        description: n["description"].as_str().unwrap_or_default().to_string(),
        state: text(&n["state"], "name"),
        assignee: text(&n["assignee"], "name"),
        priority: text(n, "priorityLabel"),
        estimate: n["estimate"].as_f64().map(|e| {
            // Linear returns points as a float; whole points read better unsuffixed.
            if e.fract() == 0.0 {
                format!("{}", e as i64)
            } else {
                e.to_string()
            }
        }),
        labels: n["labels"]["nodes"]
            .as_array()
            .map(|ls| ls.iter().filter_map(|l| text(l, "name")).collect())
            .unwrap_or_default(),
        parent: match (
            text(&n["parent"], "identifier"),
            text(&n["parent"], "title"),
        ) {
            (Some(id), Some(t)) => Some(format!("{id} \u{2014} {t}")),
            (Some(id), None) => Some(id),
            _ => None,
        },
        project: text(&n["project"], "name"),
    })
}

/// Resolve a Linear id to its [`IssueDetails`]. `Ok(None)` means Linear has no
/// such issue; an unreachable API or a rejected key is an error.
pub fn issue_details(id: &str, key: &str) -> Result<Option<IssueDetails>> {
    let query = issue_details_query(id).context("not a TEAM-NUMBER Linear id")?;
    let resp = post_graphql(&query, key, "issue_details")?;
    Ok(parse_issue_details(&resp))
}

/// GraphQL for every issue (any team) with `number == n`.
pub fn issues_by_number_query(n: u64) -> String {
    format!(
        "query {{ issues(filter: {{ number: {{ eq: {} }} }}) \
         {{ nodes {{ identifier title }} }} }}",
        n
    )
}

/// Parse the candidates from an `issues_by_number_query` response.
pub fn parse_number_candidates(resp: &serde_json::Value) -> Vec<LinearIssueRef> {
    resp["data"]["issues"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| {
            Some(LinearIssueRef {
                id: n["identifier"].as_str()?.to_string(),
                title: n["title"].as_str().unwrap_or("").to_string(),
            })
        })
        .collect()
}

/// Look up every Linear issue whose number is `n`, across all teams.
pub fn issues_by_number(n: u64, key: &str) -> Result<Vec<LinearIssueRef>> {
    let resp = post_graphql(&issues_by_number_query(n), key, "issues_by_number")?;
    Ok(parse_number_candidates(&resp))
}

/// The single transport for every Linear GraphQL call: POST the body, decode the
/// JSON envelope. `detail` labels the call for timing (see [`crate::timing`]).
/// GraphQL-level error interpretation stays with each caller — this preserves
/// the raw `ureq` error so `validate` can downcast to distinguish an unreachable
/// host from a rejected key.
fn send(body: serde_json::Value, key: &str, detail: &str) -> Result<serde_json::Value> {
    let _span = crate::timing::io_span("linear graphql", detail).entered();
    let v: serde_json::Value = ureq::post("https://api.linear.app/graphql")
        .set("Authorization", key)
        .send_json(body)?
        .into_json()?;
    Ok(v)
}

fn post_graphql(query: &str, key: &str, detail: &str) -> Result<serde_json::Value> {
    let v = send(ureq::json!({ "query": query }), key, detail)?;
    if let Some(errors) = v.get("errors").and_then(|e| e.as_array())
        && !errors.is_empty()
    {
        let msg = errors
            .first()
            .and_then(|e| e["message"].as_str())
            .unwrap_or("unknown GraphQL error");
        anyhow::bail!("Linear API error: {msg}");
    }
    Ok(v)
}

/// Validate `key` against Linear, returning the caller's identity. The ureq
/// error is preserved as the top-level error (no `.context`) so a caller can
/// downcast it to distinguish an unreachable host from a rejected key.
pub fn validate(key: &str) -> Result<LinearIdentity> {
    let resp = send(
        ureq::json!({
            "query": "query { viewer { email } organization { urlKey name } }"
        }),
        key,
        "validate",
    )?;
    parse_identity(&resp)
}

fn parse_identity(resp: &serde_json::Value) -> Result<LinearIdentity> {
    if let Some(errors) = resp.get("errors").and_then(|e| e.as_array()) {
        let msg = errors
            .first()
            .and_then(|e| e["message"].as_str())
            .unwrap_or("unknown error");
        anyhow::bail!("invalid Linear API key: {msg}");
    }
    let org = &resp["data"]["organization"];
    let viewer = &resp["data"]["viewer"];
    let url_key = org["urlKey"]
        .as_str()
        .context("invalid Linear API key: no organization in response")?;
    Ok(LinearIdentity {
        workspace_url_key: url_key.to_string(),
        org_name: org["name"].as_str().unwrap_or("").to_string(),
        viewer_email: viewer["email"].as_str().unwrap_or("").to_string(),
    })
}

/// Build the batched GraphQL query for the given `ENG-1234` ids. Pure → testable.
///
/// Ids that are not Linear ids (see [`parse_id`]) are dropped: every alias rides
/// in one request, so one malformed alias would cost the states of all the others.
/// A dropped id simply has no entry in the response map — its state is unknown.
pub fn build_query(ids: &[String]) -> Option<(String, HashMap<String, String>)> {
    let mut aliases = HashMap::new();
    let mut parts = Vec::new();
    for id in ids {
        let Some((team, num)) = parse_id(id) else {
            continue;
        };
        let alias = format!("i{}", parts.len());
        aliases.insert(alias.clone(), id.clone());
        parts.push(format!(
            "{alias}: issues(filter: {{ team: {{ key: {{ eq: \"{team}\" }} }}, number: {{ eq: {num} }} }}) {{ nodes {{ identifier state {{ type name color }} }} }}",
        ));
    }
    if parts.is_empty() {
        return None;
    }
    Some((format!("query {{ {} }}", parts.join(" ")), aliases))
}

/// Query Linear; returns id → state. Empty map if no key/ids or on network error.
pub fn states(ids: &[String], key: Option<&str>) -> HashMap<String, State> {
    let (Some(key), Some((query, aliases))) = (key, build_query(ids)) else {
        return HashMap::new();
    };
    match fetch(&query, &aliases, key) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Linear lookup failed: {e}");
            HashMap::new()
        }
    }
}

/// GraphQL payloads resolving GitHub PR URLs to their linked Linear issues,
/// 25 URLs per request to stay under Linear's query-complexity budget. Each
/// entry is (query, variables, alias → url). Pure → testable. URLs ride in
/// GraphQL variables, never spliced into the query string.
pub fn issues_for_prs_queries(
    urls: &[String],
) -> Vec<(String, serde_json::Value, HashMap<String, String>)> {
    urls.chunks(25)
        .map(|chunk| {
            let mut decls = Vec::new();
            let mut parts = Vec::new();
            let mut vars = serde_json::Map::new();
            let mut aliases = HashMap::new();
            for (i, url) in chunk.iter().enumerate() {
                decls.push(format!("$u{i}: String!"));
                parts.push(format!(
                    "a{i}: attachmentsForURL(url: $u{i}) {{ nodes {{ issue {{ identifier }} }} }}"
                ));
                vars.insert(format!("u{i}"), serde_json::Value::String(url.clone()));
                aliases.insert(format!("a{i}"), url.clone());
            }
            let query = format!("query({}) {{ {} }}", decls.join(", "), parts.join(" "));
            (query, serde_json::Value::Object(vars), aliases)
        })
        .collect()
}

/// From one `issues_for_prs_queries` response: url → linked issue ids.
/// Attachments without an issue are skipped; ids are deduped per PR (an
/// issue can attach to the same PR more than once). URLs with no linked
/// issue get no entry.
pub fn parse_issues_for_prs(
    resp: &serde_json::Value,
    aliases: &HashMap<String, String>,
) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    let Some(data) = resp.get("data").and_then(|d| d.as_object()) else {
        return out;
    };
    for (alias, block) in data {
        let Some(url) = aliases.get(alias) else {
            continue;
        };
        let mut ids: Vec<String> = Vec::new();
        for node in block["nodes"].as_array().into_iter().flatten() {
            if let Some(id) = node["issue"]["identifier"].as_str()
                && !ids.iter().any(|have| have == id)
            {
                ids.push(id.to_string());
            }
        }
        if !ids.is_empty() {
            out.insert(url.clone(), ids);
        }
    }
    out
}

/// Linked Linear issues for each PR URL. Fail-soft like [`states`]: empty
/// map with no key or no URLs; on error, one stderr line and whatever
/// chunks resolved before it. A URL absent from the map has no known links.
pub fn issues_for_prs(urls: &[String], key: Option<&str>) -> HashMap<String, Vec<String>> {
    let Some(key) = key else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for (query, vars, aliases) in issues_for_prs_queries(urls) {
        match send(
            ureq::json!({ "query": query, "variables": vars }),
            key,
            "issues_for_prs",
        ) {
            Ok(resp) => out.extend(parse_issues_for_prs(&resp, &aliases)),
            Err(e) => {
                eprintln!("Linear PR-link lookup failed: {e}");
                break;
            }
        }
    }
    out
}

/// The workspace url slug for building `linear.app/<slug>/issue/<id>` links.
///
/// Prefers `$LINEAR_WORKSPACE` (no network); otherwise asks the Linear API with
/// `$LINEAR_API_KEY`. Returns None when neither is available or the lookup fails
/// — issue ids then render as plain, unlinked text.
pub fn workspace_url_key() -> Option<String> {
    if let Some(slug) = crate::secrets::resolve("LINEAR_WORKSPACE") {
        return Some(slug);
    }
    let key = crate::secrets::resolve("LINEAR_API_KEY")?;
    fetch_url_key(&key).ok().flatten()
}

fn fetch_url_key(key: &str) -> Result<Option<String>> {
    let resp = send(
        ureq::json!({ "query": "query { organization { urlKey } }" }),
        key,
        "workspace_url_key",
    )?;
    Ok(resp["data"]["organization"]["urlKey"]
        .as_str()
        .map(String::from))
}

fn fetch(
    query: &str,
    aliases: &HashMap<String, String>,
    key: &str,
) -> Result<HashMap<String, State>> {
    let resp = send(ureq::json!({ "query": query }), key, "states")?;
    let mut out = HashMap::new();
    if let Some(data) = resp.get("data").and_then(|d| d.as_object()) {
        for (alias, block) in data {
            if let (Some(id), Some(node)) = (
                aliases.get(alias),
                block.get("nodes").and_then(|n| n.get(0)),
            ) {
                out.insert(id.clone(), parse_state(&node["state"]));
            }
        }
    }
    Ok(out)
}

/// GraphQL for one page of issues assigned to me, with state + transition history.
fn assigned_query(after: Option<&str>) -> String {
    let cursor = match after {
        Some(c) => format!(", after: \"{c}\""),
        None => String::new(),
    };
    format!(
        "query {{ issues(first: 50{cursor}, filter: {{ assignee: {{ isMe: {{ eq: true }} }} }}) \
         {{ nodes {{ identifier createdAt \
         state {{ name type color }} \
         history(first: 50) {{ nodes {{ createdAt \
         fromState {{ name type color }} toState {{ name type color }} }} }} }} \
         pageInfo {{ hasNextPage endCursor }} }} }}"
    )
}

/// Every issue assigned to me, paginated. Empty on no key / network error.
pub fn assigned_issue_history(key: &str) -> Result<Vec<AssignedIssue>> {
    assigned_issue_history_with_progress(key, |_| {})
}

/// As [`assigned_issue_history`], calling `on_page` with the running total after
/// each fetched page — lets a caller show a rising count while pages stream in.
/// One `issues.nodes[]` entry from [`assigned_query`]. Pure → testable.
fn parse_assigned_node(n: &serde_json::Value) -> AssignedIssue {
    let history = n["history"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|h| {
            (
                h["createdAt"].as_str().unwrap_or("").to_string(),
                optional_state(&h["fromState"]),
                optional_state(&h["toState"]),
            )
        })
        .collect();
    AssignedIssue {
        identifier: n["identifier"].as_str().unwrap_or("").to_string(),
        created_at: n["createdAt"].as_str().unwrap_or("").to_string(),
        state: parse_state(&n["state"]),
        history,
    }
}

/// A transition endpoint, or `None` when the block is null — an issue's first
/// state has nothing before it, and an absent state is not an Unstarted one.
fn optional_state(v: &serde_json::Value) -> Option<State> {
    (!v.is_null()).then(|| parse_state(v))
}

pub fn assigned_issue_history_with_progress(
    key: &str,
    mut on_page: impl FnMut(usize),
) -> Result<Vec<AssignedIssue>> {
    let mut out = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let resp = send(
            ureq::json!({ "query": assigned_query(after.as_deref()) }),
            key,
            "assigned_history",
        )?;
        let block = &resp["data"]["issues"];
        if let Some(nodes) = block["nodes"].as_array() {
            out.extend(nodes.iter().map(parse_assigned_node));
        }
        on_page(out.len());
        // Continue only with a real cursor; a `hasNextPage` without an
        // `endCursor` would otherwise re-fetch the first page forever.
        match (
            block["pageInfo"]["hasNextPage"].as_bool(),
            block["pageInfo"]["endCursor"].as_str(),
        ) {
            (Some(true), Some(cursor)) => after = Some(cursor.to_string()),
            _ => return Ok(out),
        }
    }
}

/// createdAt of my Linear account — the timeline origin.
pub fn viewer_created_at(key: &str) -> Result<String> {
    let resp = send(
        ureq::json!({ "query": "query { viewer { createdAt } }" }),
        key,
        "viewer",
    )?;
    resp["data"]["viewer"]["createdAt"]
        .as_str()
        .map(String::from)
        .context("viewer.createdAt missing from Linear response")
}

// --- the Tracker adapter ---------------------------------------------------

/// Linear's `state { type name color }` block as devkit's [`State`].
fn parse_state(v: &serde_json::Value) -> State {
    State {
        kind: v["type"]
            .as_str()
            .unwrap_or("")
            .parse()
            .expect("infallible"),
        name: v["name"].as_str().unwrap_or("").to_string(),
        color: v["color"].as_str().map(str::to_string),
    }
}

/// Lowercase, collapse non-alphanumerics to single dashes, trim dashes.
fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// The id and title slug in a Linear issue URL's `…/issue/<ID>/<title-slug>`
/// path. `None` when there is no `issue/<ID>` pair to read.
///
/// Both values come from their path position. Scanning for the first
/// letters-dash-digits run instead would read a workspace named `acme-2` as
/// the issue id.
fn url_ref(url: &str) -> Option<IssueRef> {
    let path = url.trim();
    let path = path.split_once('#').map_or(path, |(head, _)| head);
    let path = path.split_once('?').map_or(path, |(head, _)| head);
    let mut segments = path
        .split('/')
        .skip_while(|s| !s.eq_ignore_ascii_case("issue"));
    let id = segments.nth(1).filter(|s| !s.is_empty())?;
    Some(IssueRef {
        id: id.to_uppercase(),
        slug: segments.next().map(slugify).filter(|s| !s.is_empty()),
    })
}

impl From<IssueDetails> for super::IssueDetails {
    /// Linear's optional fields flatten to empty strings: the summary template
    /// branches on emptiness, not on presence.
    fn from(d: IssueDetails) -> Self {
        super::IssueDetails {
            id: d.identifier,
            title: d.title,
            url: d.url,
            description: d.description,
            state: d.state.unwrap_or_default(),
            assignee: d.assignee.unwrap_or_default(),
            priority: d.priority.unwrap_or_default(),
            estimate: d.estimate.unwrap_or_default(),
            labels: d.labels,
            parent: d.parent.unwrap_or_default(),
            project: d.project.unwrap_or_default(),
        }
    }
}

/// Linear behind the tracker seam. Holds the API key resolved once at
/// construction; `None` means every call degrades to empty rather than erroring,
/// which is what keeps `issue status` useful on a machine with no key.
pub struct LinearTracker {
    key: Option<String>,
}

impl LinearTracker {
    pub fn new(key: Option<String>) -> Self {
        Self { key }
    }
}

impl Tracker for LinearTracker {
    fn kind(&self) -> TrackerKind {
        TrackerKind::Linear
    }

    fn ready(&self) -> bool {
        self.key.is_some()
    }

    fn issue_ref(&self, input: &str) -> IssueRef {
        let trimmed = input.trim();
        if trimmed.contains("linear.app")
            && let Some(parsed) = url_ref(trimmed)
        {
            return parsed;
        }
        IssueRef {
            id: trimmed.to_uppercase(),
            slug: None,
        }
    }

    fn title(&self, id: &str) -> Result<Option<String>> {
        match &self.key {
            Some(k) => issue_title(id, k),
            None => Ok(None),
        }
    }

    fn details(&self, id: &str) -> Result<Option<super::IssueDetails>> {
        match &self.key {
            Some(k) => Ok(issue_details(id, k)?.map(Into::into)),
            None => Ok(None),
        }
    }

    fn states(&self, ids: &[String]) -> HashMap<String, State> {
        states(ids, self.key.as_deref())
    }

    fn issue_pr(&self, id: &str) -> Result<Option<PrRef>> {
        match &self.key {
            Some(k) => Ok(issue_pr(id, k)?.0.map(|p| PrRef {
                url: p.url,
                number: p.number,
            })),
            None => Ok(None),
        }
    }

    fn candidates(&self, n: u64) -> Result<Vec<IssueRef>> {
        match &self.key {
            Some(k) => Ok(issues_by_number(n, k)?
                .into_iter()
                .map(|c| IssueRef {
                    id: c.id,
                    slug: None,
                })
                .collect()),
            None => Ok(Vec::new()),
        }
    }

    fn issues_for_prs(&self, urls: &[String]) -> HashMap<String, Vec<String>> {
        issues_for_prs(urls, self.key.as_deref())
    }

    fn assigned_history(&self, on_page: &mut dyn FnMut(usize)) -> Result<Vec<AssignedIssue>> {
        match &self.key {
            Some(k) => assigned_issue_history_with_progress(k, on_page),
            None => Ok(Vec::new()),
        }
    }

    fn timeline_origin(&self) -> Result<Option<String>> {
        match &self.key {
            Some(k) => viewer_created_at(k).map(Some),
            None => Ok(None),
        }
    }

    fn issue_url(&self, id: &str) -> Option<String> {
        let ws = workspace_url_key()?;
        Some(format!("https://linear.app/{ws}/issue/{id}"))
    }

    fn check(&self) -> Result<String> {
        let key = self
            .key
            .as_deref()
            .context("no Linear API key — run `devkit auth linear`")?;
        let id = validate(key)?;
        Ok(format!("linear: {} ({})", id.org_name, id.viewer_email))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::StateKind;
    #[test]
    fn query_aliases_each_id() {
        let (q, a) = build_query(&["ENG-1".into(), "ABC-22".into()]).unwrap();
        assert!(q.contains("number: { eq: 1 }"));
        assert!(q.contains("number: { eq: 22 }"));
        assert_eq!(a.len(), 2);
    }
    #[test]
    fn empty_ids_no_query() {
        assert!(build_query(&[]).is_none());
    }
    #[test]
    fn leading_zero_id_is_dropped_not_spliced() {
        let (q, a) = build_query(&["CONFIG-01".into(), "ENG-7".into()]).unwrap();
        assert!(!q.contains("eq: 01"), "leading zero is invalid GraphQL Int");
        assert!(q.contains("number: { eq: 7 }"));
        assert_eq!(a.len(), 1);
        assert!(a.values().all(|id| id == "ENG-7"));
    }
    #[test]
    fn unparseable_id_does_not_poison_the_batch() {
        let (q, a) = build_query(&["nodash".into(), "ENG-7".into(), "X-".into()]).unwrap();
        assert!(q.contains("number: { eq: 7 }"));
        assert_eq!(a.len(), 1);
    }
    #[test]
    fn all_ids_unparseable_no_query() {
        assert!(build_query(&["UNKNOWN".into(), "CONFIG-01".into()]).is_none());
    }
    #[test]
    fn issue_pr_query_rejects_leading_zero() {
        assert!(issue_pr_query("CONFIG-01").is_none());
    }
    #[test]
    fn assigned_query_paginates() {
        assert!(assigned_query(None).contains("issues(first: 50"));
        assert!(assigned_query(None).contains("assignee: { isMe: { eq: true } }"));
        assert!(assigned_query(Some("CUR")).contains("after: \"CUR\""));
    }
    #[test]
    fn assigned_history_no_op_wrapper_exists() {
        // Compile-time guarantee that the no-op wrapper still delegates to the
        // progress variant with the same return type.
        fn _assert_sig(k: &str) -> Result<Vec<AssignedIssue>> {
            assigned_issue_history(k)
        }
        fn _assert_progress(k: &str) -> Result<Vec<AssignedIssue>> {
            assigned_issue_history_with_progress(k, |_n| {})
        }
        let _ = (_assert_sig, _assert_progress);
    }

    #[test]
    fn linear_identity_parsed() {
        let v = serde_json::json!({
            "data": { "viewer": { "email": "me@x.io" },
                      "organization": { "urlKey": "adaptyv", "name": "Adaptyv" } }
        });
        let id = parse_identity(&v).unwrap();
        assert_eq!(id.workspace_url_key, "adaptyv");
        assert_eq!(id.org_name, "Adaptyv");
        assert_eq!(id.viewer_email, "me@x.io");
    }

    #[test]
    fn linear_errors_body_is_invalid() {
        let v = serde_json::json!({ "errors": [{ "message": "authentication failed" }] });
        let e = parse_identity(&v).unwrap_err();
        assert!(e.to_string().contains("invalid Linear API key"));
    }

    #[test]
    fn linear_missing_org_is_invalid() {
        let v = serde_json::json!({ "data": { "viewer": { "email": "" }, "organization": {} } });
        assert!(parse_identity(&v).is_err());
    }

    #[test]
    fn issue_pr_query_filters_team_and_number() {
        let q = issue_pr_query("ENG-42").unwrap();
        assert!(q.contains("key: { eq: \"ENG\" }"));
        assert!(q.contains("number: { eq: 42 }"));
        assert!(q.contains("attachments"));
        assert!(issue_pr_query("nodash").is_none());
    }

    #[test]
    fn issue_title_query_asks_only_for_the_title() {
        let q = issue_title_query("ENG-42").unwrap();
        assert!(q.contains("key: { eq: \"ENG\" }"));
        assert!(q.contains("number: { eq: 42 }"));
        assert!(q.contains("title"));
        assert!(!q.contains("attachments"));
        assert!(issue_title_query("nodash").is_none());
    }

    #[test]
    fn parse_issue_title_reads_the_first_node() {
        let v = serde_json::json!({"data": {"issues": {"nodes": [{"title": "Fix BLI export"}]}}});
        assert_eq!(parse_issue_title(&v).as_deref(), Some("Fix BLI export"));
    }

    #[test]
    fn parse_issue_title_none_for_unknown_or_untitled_issue() {
        let empty = serde_json::json!({"data": {"issues": {"nodes": []}}});
        assert_eq!(parse_issue_title(&empty), None);
        let blank = serde_json::json!({"data": {"issues": {"nodes": [{"title": ""}]}}});
        assert_eq!(parse_issue_title(&blank), None);
    }

    #[test]
    fn parse_issue_pr_finds_github_attachment() {
        let v = serde_json::json!({"data": {"issues": {"nodes": [{
            "title": "Fix login",
            "attachments": {"nodes": [
                {"url": "https://example.com/doc"},
                {"url": "https://github.com/org/repo/pull/3340"}
            ]}
        }]}}});
        let (pr, title) = parse_issue_pr(&v);
        assert_eq!(title, "Fix login");
        assert_eq!(pr.unwrap().number, 3340);
    }

    #[test]
    fn parse_issue_pr_no_attachment_is_none() {
        let v = serde_json::json!({"data": {"issues": {"nodes": [{
            "title": "No PR yet", "attachments": {"nodes": []}
        }]}}});
        let (pr, title) = parse_issue_pr(&v);
        assert!(pr.is_none());
        assert_eq!(title, "No PR yet");
    }

    #[test]
    fn parse_issue_pr_empty_nodes_is_none() {
        let v = serde_json::json!({"data": {"issues": {"nodes": []}}});
        let (pr, title) = parse_issue_pr(&v);
        assert!(pr.is_none());
        assert_eq!(title, "");
    }

    #[test]
    fn parse_number_candidates_collects_ids_and_titles() {
        let v = serde_json::json!({"data": {"issues": {"nodes": [
            {"identifier": "ENG-3340", "title": "A"},
            {"identifier": "OPS-3340", "title": "B"}
        ]}}});
        let got = parse_number_candidates(&v);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, "ENG-3340");
        assert_eq!(got[1].title, "B");
    }

    #[test]
    fn issues_for_prs_queries_use_variables() {
        let urls = vec![
            "https://github.com/o/r/pull/1".to_string(),
            "https://github.com/o/r/pull/2".to_string(),
        ];
        let batches = issues_for_prs_queries(&urls);
        assert_eq!(batches.len(), 1);
        let (q, vars, aliases) = &batches[0];
        assert!(q.contains("a0: attachmentsForURL(url: $u0)"), "{q}");
        assert!(q.contains("$u1: String!"), "{q}");
        assert!(
            !q.contains("github.com"),
            "urls must ride in variables, not the query: {q}"
        );
        assert_eq!(vars["u1"], "https://github.com/o/r/pull/2");
        assert_eq!(aliases["a0"], "https://github.com/o/r/pull/1");
    }

    #[test]
    fn issues_for_prs_queries_chunk_at_25() {
        let urls: Vec<String> = (0..26)
            .map(|i| format!("https://github.com/o/r/pull/{i}"))
            .collect();
        let batches = issues_for_prs_queries(&urls);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[1].2.len(), 1, "second chunk carries the 26th url");
        assert!(issues_for_prs_queries(&[]).is_empty());
    }

    #[test]
    fn parse_issues_for_prs_collects_and_dedups() {
        let aliases = HashMap::from([
            ("a0".to_string(), "u0".to_string()),
            ("a1".to_string(), "u1".to_string()),
        ]);
        let resp = serde_json::json!({ "data": {
            "a0": { "nodes": [
                { "issue": { "identifier": "SWE-6" } },
                { "issue": null },
                { "issue": { "identifier": "SWE-7" } },
                { "issue": { "identifier": "SWE-6" } }
            ]},
            "a1": { "nodes": [ { "issue": null } ] }
        }});
        let got = parse_issues_for_prs(&resp, &aliases);
        assert_eq!(got["u0"], vec!["SWE-6", "SWE-7"]);
        assert!(
            !got.contains_key("u1"),
            "all-null attachments mean no links"
        );
    }

    #[test]
    fn parse_issues_for_prs_ignores_unknown_aliases() {
        let resp = serde_json::json!({ "data": {
            "zz": { "nodes": [ { "issue": { "identifier": "X-1" } } ] }
        }});
        assert!(parse_issues_for_prs(&resp, &HashMap::new()).is_empty());
    }

    #[test]
    fn issue_details_query_targets_team_and_number() {
        let q = issue_details_query("ENG-42").unwrap();
        assert!(q.contains("key: { eq: \"ENG\" }"));
        assert!(q.contains("number: { eq: 42 }"));
        assert!(q.contains("description"));
        assert!(q.contains("priorityLabel"));
    }

    #[test]
    fn issue_details_query_rejects_leading_zero() {
        assert!(issue_details_query("CONFIG-01").is_none());
    }

    #[test]
    fn issue_details_parsed_in_full() {
        let v = serde_json::json!({ "data": { "issues": { "nodes": [{
            "identifier": "ENG-42",
            "title": "Fix the login redirect",
            "url": "https://linear.app/acme/issue/ENG-42/fix-the-login-redirect",
            "description": "Steps:\n1. click\n",
            "state": { "name": "Todo" },
            "assignee": { "name": "Lev" },
            "priorityLabel": "High",
            "estimate": 3,
            "labels": { "nodes": [{ "name": "auth" }, { "name": "web" }] },
            "parent": { "identifier": "ENG-1", "title": "Login epic" },
            "project": { "name": "Q3 hardening" }
        }] } } });
        let d = parse_issue_details(&v).unwrap();
        assert_eq!(d.identifier, "ENG-42");
        assert_eq!(d.title, "Fix the login redirect");
        assert_eq!(d.description, "Steps:\n1. click\n");
        assert_eq!(d.state.as_deref(), Some("Todo"));
        assert_eq!(d.assignee.as_deref(), Some("Lev"));
        assert_eq!(d.priority.as_deref(), Some("High"));
        assert_eq!(d.estimate.as_deref(), Some("3"));
        assert_eq!(d.labels, vec!["auth".to_string(), "web".to_string()]);
        assert_eq!(d.parent.as_deref(), Some("ENG-1 \u{2014} Login epic"));
        assert_eq!(d.project.as_deref(), Some("Q3 hardening"));
    }

    #[test]
    fn issue_details_missing_fields_are_none_not_empty_strings() {
        let v = serde_json::json!({ "data": { "issues": { "nodes": [{
            "identifier": "ENG-7",
            "title": "Bare issue",
            "url": "https://linear.app/acme/issue/ENG-7/bare-issue",
            "description": serde_json::Value::Null,
            "state": { "name": "Backlog" },
            "assignee": serde_json::Value::Null,
            "priorityLabel": "No priority",
            "estimate": serde_json::Value::Null,
            "labels": { "nodes": [] },
            "parent": serde_json::Value::Null,
            "project": serde_json::Value::Null
        }] } } });
        let d = parse_issue_details(&v).unwrap();
        assert_eq!(d.description, "");
        assert!(d.assignee.is_none());
        assert!(d.estimate.is_none());
        assert!(d.parent.is_none());
        assert!(d.project.is_none());
        assert!(d.labels.is_empty());
    }

    #[test]
    fn issue_details_absent_issue_is_none() {
        let v = serde_json::json!({ "data": { "issues": { "nodes": [] } } });
        assert!(parse_issue_details(&v).is_none());
    }

    #[test]
    fn linear_maps_its_state_types_onto_state_kinds() {
        let s = parse_state(&serde_json::json!({
            "type": "started", "name": "In Progress", "color": "#f2c94c"
        }));
        assert_eq!(s.kind, StateKind::Started);
        assert_eq!(s.name, "In Progress");
        assert_eq!(s.color.as_deref(), Some("#f2c94c"));
    }

    #[test]
    fn linear_uppercases_a_bare_id() {
        let t = LinearTracker::new(Some("k".into()));
        assert_eq!(t.issue_ref("eng-42").id, "ENG-42");
    }

    #[test]
    fn linear_reads_an_id_and_slug_from_a_url_by_path_position() {
        let t = LinearTracker::new(Some("k".into()));
        let r = t.issue_ref("https://linear.app/acme-2/issue/ENG-42/fix-the-login");
        assert_eq!(
            r.id, "ENG-42",
            "a workspace named acme-2 is not the issue id"
        );
        assert_eq!(r.slug.as_deref(), Some("fix-the-login"));
    }

    #[test]
    fn a_keyless_linear_tracker_is_not_ready_and_answers_empty() {
        let t = LinearTracker::new(None);
        assert!(!t.ready());
        assert!(t.states(&["ENG-1".into()]).is_empty());
        assert!(t.title("ENG-1").unwrap().is_none());
    }

    #[test]
    fn the_batch_state_query_asks_for_a_colour() {
        let (q, _) = build_query(&["ENG-1".into()]).expect("a non-empty id list builds a query");
        assert!(
            q.contains("color"),
            "State.color needs the field selected: {q}"
        );
    }

    /// A history entry can carry a null `fromState` (the issue's very first
    /// state has nothing before it). That must stay `None`: mapped through
    /// `parse_state` it would become a real Unstarted state and shift the
    /// dashboard's bands.
    #[test]
    fn a_transition_without_a_from_state_stays_none() {
        let n = serde_json::json!({
            "identifier": "ENG-1",
            "createdAt": "2026-01-01T00:00:00Z",
            "state": { "name": "Done", "type": "completed", "color": "#0f0" },
            "history": { "nodes": [{
                "createdAt": "2026-01-02T00:00:00Z",
                "fromState": serde_json::Value::Null,
                "toState": { "name": "Todo", "type": "unstarted", "color": "#888" }
            }] }
        });
        let iss = parse_assigned_node(&n);
        let (when, from, to) = &iss.history[0];
        assert_eq!(when, "2026-01-02T00:00:00Z");
        assert!(from.is_none(), "a null fromState is no state at all");
        assert_eq!(to.as_ref().expect("toState is present").name, "Todo");
    }
}
