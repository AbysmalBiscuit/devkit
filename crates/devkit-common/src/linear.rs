use anyhow::{Context, Result};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearState {
    pub kind: String, // completed | started | unstarted | backlog | triage | canceled
    pub name: String, // "Done"
}

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

/// Parse the PR number out of a `…/pull/<n>` GitHub URL.
pub fn pr_number_from_url(url: &str) -> Option<u64> {
    let tail = url.split("/pull/").nth(1)?;
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// GraphQL fetching one issue's title + GitHub PR attachments. Returns None
/// for ids that are not in `TEAM-NUMBER` form.
pub fn issue_pr_query(id: &str) -> Option<String> {
    let (team, num) = id.split_once('-')?;
    Some(format!(
        "query {{ issues(filter: {{ team: {{ key: {{ eq: \"{}\" }} }}, number: {{ eq: {} }} }}) \
         {{ nodes {{ title attachments {{ nodes {{ url }} }} }} }} }}",
        team.to_uppercase(),
        num
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
            pr_number_from_url(u).map(|number| LinearPr {
                url: u.to_string(),
                number,
            })
        });
    (pr, title)
}

/// Resolve a Linear id to its attached GitHub PR + the issue title.
pub fn issue_pr(id: &str, key: &str) -> Result<(Option<LinearPr>, String)> {
    let query = issue_pr_query(id).context("not a TEAM-NUMBER Linear id")?;
    let resp = post_graphql(&query, key)?;
    Ok(parse_issue_pr(&resp))
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
    let resp = post_graphql(&issues_by_number_query(n), key)?;
    Ok(parse_number_candidates(&resp))
}

fn post_graphql(query: &str, key: &str) -> Result<serde_json::Value> {
    let v: serde_json::Value = ureq::post("https://api.linear.app/graphql")
        .set("Authorization", key)
        .send_json(ureq::json!({ "query": query }))?
        .into_json()?;
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
    let resp: serde_json::Value = ureq::post("https://api.linear.app/graphql")
        .set("Authorization", key)
        .send_json(ureq::json!({
            "query": "query { viewer { email } organization { urlKey name } }"
        }))?
        .into_json()?;
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
pub fn build_query(ids: &[String]) -> Option<(String, HashMap<String, String>)> {
    let mut aliases = HashMap::new();
    let mut parts = Vec::new();
    for (idx, id) in ids.iter().enumerate() {
        let (team, num) = id.split_once('-')?;
        let alias = format!("i{idx}");
        aliases.insert(alias.clone(), id.clone());
        parts.push(format!(
            "{alias}: issues(filter: {{ team: {{ key: {{ eq: \"{}\" }} }}, number: {{ eq: {} }} }}) {{ nodes {{ identifier state {{ type name }} }} }}",
            team.to_uppercase(), num
        ));
    }
    if parts.is_empty() {
        return None;
    }
    Some((format!("query {{ {} }}", parts.join(" ")), aliases))
}

/// Query Linear; returns id → state. Empty map if no key/ids or on network error.
pub fn states(ids: &[String], key: Option<&str>) -> HashMap<String, LinearState> {
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
    let resp: serde_json::Value = ureq::post("https://api.linear.app/graphql")
        .set("Authorization", key)
        .send_json(ureq::json!({ "query": "query { organization { urlKey } }" }))?
        .into_json()?;
    Ok(resp["data"]["organization"]["urlKey"]
        .as_str()
        .map(String::from))
}

fn fetch(
    query: &str,
    aliases: &HashMap<String, String>,
    key: &str,
) -> Result<HashMap<String, LinearState>> {
    let resp: serde_json::Value = ureq::post("https://api.linear.app/graphql")
        .set("Authorization", key)
        .send_json(ureq::json!({ "query": query }))?
        .into_json()?;
    let mut out = HashMap::new();
    if let Some(data) = resp.get("data").and_then(|d| d.as_object()) {
        for (alias, block) in data {
            if let (Some(id), Some(node)) = (
                aliases.get(alias),
                block.get("nodes").and_then(|n| n.get(0)),
            ) {
                let st = &node["state"];
                out.insert(
                    id.clone(),
                    LinearState {
                        kind: st["type"].as_str().unwrap_or("").to_string(),
                        name: st["name"].as_str().unwrap_or("").to_string(),
                    },
                );
            }
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StateRef {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub color: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AssignedIssue {
    pub identifier: String,
    pub created_at: String,
    pub state: StateRef,
    /// (createdAt, fromState, toState) for each recorded transition, unsorted.
    pub history: Vec<(String, Option<StateRef>, Option<StateRef>)>,
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
pub fn assigned_issue_history_with_progress(
    key: &str,
    mut on_page: impl FnMut(usize),
) -> Result<Vec<AssignedIssue>> {
    let mut out = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let resp: serde_json::Value = ureq::post("https://api.linear.app/graphql")
            .set("Authorization", key)
            .send_json(ureq::json!({ "query": assigned_query(after.as_deref()) }))?
            .into_json()?;
        let block = &resp["data"]["issues"];
        if let Some(nodes) = block["nodes"].as_array() {
            for n in nodes {
                let state: StateRef = serde_json::from_value(n["state"].clone())?;
                let mut history = Vec::new();
                if let Some(hn) = n["history"]["nodes"].as_array() {
                    for h in hn {
                        let from = serde_json::from_value(h["fromState"].clone()).ok();
                        let to = serde_json::from_value(h["toState"].clone()).ok();
                        let when = h["createdAt"].as_str().unwrap_or("").to_string();
                        history.push((when, from, to));
                    }
                }
                out.push(AssignedIssue {
                    identifier: n["identifier"].as_str().unwrap_or("").to_string(),
                    created_at: n["createdAt"].as_str().unwrap_or("").to_string(),
                    state,
                    history,
                });
            }
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
    let resp: serde_json::Value = ureq::post("https://api.linear.app/graphql")
        .set("Authorization", key)
        .send_json(ureq::json!({ "query": "query { viewer { createdAt } }" }))?
        .into_json()?;
    resp["data"]["viewer"]["createdAt"]
        .as_str()
        .map(String::from)
        .context("viewer.createdAt missing from Linear response")
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn issue_pr_query_filters_team_and_number() {
        let q = issue_pr_query("ENG-42").unwrap();
        assert!(q.contains("key: { eq: \"ENG\" }"));
        assert!(q.contains("number: { eq: 42 }"));
        assert!(q.contains("attachments"));
        assert!(issue_pr_query("nodash").is_none());
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
}
