use anyhow::{Context, Result};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearState {
    pub kind: String, // completed | started | unstarted | backlog | triage | canceled
    pub name: String, // "Done"
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
    if let Some(slug) = std::env::var("LINEAR_WORKSPACE")
        .ok()
        .filter(|s| !s.is_empty())
    {
        return Some(slug);
    }
    let key = std::env::var("LINEAR_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())?;
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
}
