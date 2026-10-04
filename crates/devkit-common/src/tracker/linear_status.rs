//! Reading and writing a Linear issue's workflow state.

use std::cell::RefCell;

use anyhow::{Context, Result, ensure};
use serde_json::Value;

use super::{
    linear::{parse_id, post_graphql},
    status::{StatusWriter, find_id, names},
};

/// One query reading the issue's UUID, its state, and its team's states.
pub fn status_query(id: &str) -> Result<String> {
    let (team, num) = parse_id(id).with_context(|| format!("`{id}` is not a Linear issue id"))?;
    Ok(format!(
        "query {{ issues(filter: {{ team: {{ key: {{ eq: \"{team}\" }} }}, number: {{ eq: {num} }} }}) \
         {{ nodes {{ id state {{ name }} team {{ states {{ nodes {{ id name }} }} }} }} }} }}"
    ))
}

/// What [`status_query`] read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearStatus {
    /// The issue's UUID, which `issueUpdate` takes.
    pub issue_id: String,
    pub current: String,
    /// `(state id, name)` for each of the team's workflow states.
    pub states: Vec<(String, String)>,
}

impl LinearStatus {
    /// The id of the team state named `to`, matched case-insensitively.
    pub fn state_id(&self, to: &str) -> Result<&str> {
        find_id(&self.states, to).with_context(|| {
            format!(
                "no state `{to}` in the issue's team; states: {}",
                names(&self.states)
            )
        })
    }
}

/// The [`LinearStatus`] in a [`status_query`] response for `id`.
pub fn parse_status(resp: &Value, id: &str) -> Result<LinearStatus> {
    let node = resp
        .pointer("/data/issues/nodes/0")
        .with_context(|| format!("Linear has no issue {id}"))?;
    Ok(LinearStatus {
        issue_id: node["id"]
            .as_str()
            .context("issue without an id")?
            .to_string(),
        current: node["state"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        states: node["team"]["states"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| {
                Some((
                    s["id"].as_str()?.to_string(),
                    s["name"].as_str()?.to_string(),
                ))
            })
            .collect(),
    })
}

/// Move the issue with UUID `issue_id` to the state `state_id`.
pub fn set_state_mutation(issue_id: &str, state_id: &str) -> String {
    format!(
        "mutation {{ issueUpdate(id: {i}, input: {{stateId: {s}}}) {{ success }} }}",
        i = Value::from(issue_id),
        s = Value::from(state_id),
    )
}

/// Linear workflow states, through the API key `key`.
pub struct LinearWriter {
    key: String,
    /// The read [`StatusWriter::status`] made, which the
    /// [`StatusWriter::set_status`] after it reuses instead of reading again.
    last: RefCell<Option<(String, LinearStatus)>>,
}

impl LinearWriter {
    pub fn new(key: String) -> Self {
        LinearWriter {
            key,
            last: RefCell::new(None),
        }
    }

    fn read(&self, id: &str) -> Result<LinearStatus> {
        let resp = post_graphql(&status_query(id)?, &self.key, "issue status")?;
        parse_status(&resp, id)
    }
}

impl StatusWriter for LinearWriter {
    fn status(&self, id: &str) -> Result<Option<String>> {
        let read = self.read(id)?;
        let current = Some(read.current.clone()).filter(|c| !c.is_empty());
        *self.last.borrow_mut() = Some((id.to_string(), read));
        Ok(current)
    }

    fn set_status(&self, id: &str, to: &str) -> Result<()> {
        let read = match self.last.borrow_mut().take() {
            Some((cached, read)) if cached == id => read,
            _ => self.read(id)?,
        };
        let state = read.state_id(to)?;
        let resp = post_graphql(
            &set_state_mutation(&read.issue_id, state),
            &self.key,
            "issue update",
        )?;
        ensure!(
            resp.pointer("/data/issueUpdate/success") == Some(&Value::Bool(true)),
            "Linear did not move {id} to `{to}`"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/tracker/fixtures")
            .join(name);
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
    }

    #[test]
    fn reads_the_issue_its_state_and_its_teams_states() {
        let s = parse_status(&fixture("linear_status.json"), "ENG-123").unwrap();
        assert_eq!(s.issue_id, "9f1c2b4e-1111-4c1e-9a0a-5d3b2a1c0e01");
        assert_eq!(s.current, "Todo");
        assert_eq!(s.state_id("in progress").unwrap(), "st-doing");
    }

    #[test]
    fn an_unknown_state_lists_the_teams_states() {
        let s = parse_status(&fixture("linear_status.json"), "ENG-123").unwrap();
        let e = s.state_id("Shipping").unwrap_err().to_string();
        assert!(
            e.contains("no state `Shipping`") && e.contains("Todo, In Progress, In Review"),
            "{e}"
        );
    }

    #[test]
    fn a_missing_issue_is_an_error_naming_it() {
        let empty = serde_json::json!({ "data": { "issues": { "nodes": [] } } });
        assert!(
            parse_status(&empty, "ENG-9")
                .unwrap_err()
                .to_string()
                .contains("ENG-9")
        );
    }

    #[test]
    fn a_lowercase_id_queries_the_uppercase_team() {
        let q = status_query("eng-123").unwrap();
        assert!(q.contains("\"ENG\"") && q.contains("eq: 123"), "{q}");
    }

    #[test]
    fn a_malformed_id_is_an_error_naming_it() {
        let e = status_query("not-an-id-x").unwrap_err().to_string();
        assert!(e.contains("not-an-id-x"), "{e}");
    }

    #[test]
    fn the_mutation_sets_the_state_by_uuid() {
        let m = set_state_mutation("uuid-1", "st-doing");
        assert!(
            m.contains("issueUpdate(id: \"uuid-1\"") && m.contains("stateId: \"st-doing\""),
            "{m}"
        );
    }
}
