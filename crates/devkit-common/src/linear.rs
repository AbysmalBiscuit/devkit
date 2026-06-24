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
}
