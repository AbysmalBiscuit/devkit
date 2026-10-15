//! Linear issues as `devkit todo sync` reads them.
//!
//! Filters travel as GraphQL variables, so no config value is spliced into
//! a query.

use anyhow::{Context, Result};
use devkit_config::{Assignee, MatchRule, ProjectRole};
use serde_json::{Value, json};

use super::{
    discover::next_cursor,
    linear::{parse_id, post_graphql_with},
    status::same_status,
    tickets::{SyncedTicket, TicketStatus, in_closed_name},
};

// Nested connections read up to 250 nodes, Linear's largest page. Linear
// multiplies a nested connection's cost by its parent's page size, so the
// outer pages are 10 to stay within its query-complexity budget.

/// The issue fields a sync reads, for one page of `issues(filter:)`.
const ISSUES_QUERY: &str = "query($filter: IssueFilter, $after: String) { \
     issues(first: 10, after: $after, filter: $filter) { \
     pageInfo { hasNextPage endCursor } \
     nodes { identifier title state { name } parent { identifier } \
     inverseRelations(first: 250) { nodes { type issue { identifier } } } } } }";

/// Each team's workflow state names, for one page of `teams`.
const TEAMS_QUERY: &str = "query($after: String) { \
     teams(first: 10, after: $after) { \
     pageInfo { hasNextPage endCursor } \
     nodes { key states(first: 250) { nodes { name } } } } }";

/// Keys read by [`LinearTickets::read`] per request: one page of
/// [`ISSUES_QUERY`].
const KEYS_PER_REQUEST: usize = 10;

fn is_me() -> Value {
    json!({ "isMe": { "eq": true } })
}

/// Linear's workflow state types an issue is closed in.
const CLOSED_STATE_TYPES: [&str; 3] = ["completed", "canceled", "duplicate"];

/// The `IssueFilter` `rule` stands for, over open issues only and leaving
/// out issues in one of `closed_names`. Every condition is one clause of an
/// `and`, so a rule's labels each require their own label.
pub fn rule_filter(rule: &MatchRule, closed_names: &[String]) -> Value {
    let mut clauses = Vec::new();
    match &rule.assignee {
        Some(Assignee::Me) => clauses.push(json!({ "assignee": is_me() })),
        Some(Assignee::Nobody) => clauses.push(json!({ "assignee": { "null": true } })),
        Some(Assignee::User(name)) => clauses.push(json!({ "assignee": { "or": [
            { "name": { "eqIgnoreCase": name } },
            { "displayName": { "eqIgnoreCase": name } },
        ]}})),
        None => {}
    }
    match rule.project_role {
        Some(ProjectRole::Lead) => clauses.push(json!({ "project": { "lead": is_me() } })),
        Some(ProjectRole::Member) => clauses.push(json!({ "project": { "or": [
            { "lead": is_me() },
            { "members": { "some": is_me() } },
        ]}})),
        None => {}
    }
    for label in &rule.labels {
        clauses.push(json!({ "labels": { "some": { "name": { "eqIgnoreCase": label } } } }));
    }
    clauses.push(json!({ "state": { "type": { "nin": CLOSED_STATE_TYPES } } }));
    for name in closed_names {
        clauses.push(json!({ "state": { "name": { "neqIgnoreCase": name.trim() } } }));
    }
    json!({ "and": clauses })
}

/// The `IssueFilter` selecting the issues `keys` name. Keys that are not
/// Linear ids are left out; `None` when none is.
pub fn keys_filter(keys: &[String]) -> Option<Value> {
    let each: Vec<Value> = keys
        .iter()
        .filter_map(|k| parse_id(k))
        .map(|(team, number)| {
            json!({ "team": { "key": { "eq": team } }, "number": { "eq": number } })
        })
        .collect();
    (!each.is_empty()).then(|| json!({ "or": each }))
}

/// One issue node of an [`ISSUES_QUERY`] page.
fn parse_issue(v: &Value) -> Option<SyncedTicket> {
    let key = v["identifier"].as_str()?.to_string();
    let mut blocked_by: Vec<String> = v
        .pointer("/inverseRelations/nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|r| r["type"].as_str() == Some("blocks"))
        .filter_map(|r| r.pointer("/issue/identifier")?.as_str())
        .map(String::from)
        .collect();
    blocked_by.sort();
    blocked_by.dedup();
    Some(SyncedTicket {
        key,
        title: v["title"].as_str().unwrap_or_default().to_string(),
        status: TicketStatus::Named(
            v.pointer("/state/name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ),
        blocked_by,
        parent: v
            .pointer("/parent/identifier")
            .and_then(Value::as_str)
            .map(String::from),
    })
}

/// The tickets on one issues page and the cursor of the next.
pub fn parse_page(resp: &Value) -> Result<(Vec<SyncedTicket>, Option<String>)> {
    let issues = resp
        .pointer("/data/issues")
        .filter(|v| !v.is_null())
        .context("Linear's issue list returned no issues field")?;
    let tickets = issues["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_issue)
        .collect();
    Ok((tickets, next_cursor(issues)))
}

/// Every issue `filter` selects, page by page, added to `found` unless a
/// ticket with its key is already there.
fn read_all(
    filter: &Value,
    found: &mut Vec<SyncedTicket>,
    fetch: &mut impl FnMut(&Value, Option<&str>) -> Result<Value>,
) -> Result<()> {
    let mut after: Option<String> = None;
    loop {
        let (page, next) = parse_page(&fetch(filter, after.as_deref())?)?;
        for ticket in page {
            if !found.iter().any(|t| t.key == ticket.key) {
                found.push(ticket);
            }
        }
        match next {
            Some(cursor) => after = Some(cursor),
            None => return Ok(()),
        }
    }
}

/// Every issue a rule selects, but those in one of `closed_names`, each
/// once. `fetch` answers one page for a filter and a cursor.
///
/// The filter already leaves closed names out; matching them again here
/// keeps out a state whose name differs from the map's only in a way
/// Linear's comparison does not ignore, such as surrounding whitespace.
pub fn find_with(
    rules: &[MatchRule],
    closed_names: &[String],
    mut fetch: impl FnMut(&Value, Option<&str>) -> Result<Value>,
) -> Result<Vec<SyncedTicket>> {
    let mut found = Vec::new();
    for rule in rules {
        read_all(&rule_filter(rule, closed_names), &mut found, &mut fetch)?;
    }
    found.retain(|t| !in_closed_name(t, closed_names));
    Ok(found)
}

/// The issues `keys` name, whatever their state. A key that names no issue
/// is left out.
pub fn read_with(
    keys: &[String],
    mut fetch: impl FnMut(&Value, Option<&str>) -> Result<Value>,
) -> Result<Vec<SyncedTicket>> {
    let mut found = Vec::new();
    for chunk in keys.chunks(KEYS_PER_REQUEST) {
        if let Some(filter) = keys_filter(chunk) {
            read_all(&filter, &mut found, &mut fetch)?;
        }
    }
    Ok(found)
}

/// The union of every team's workflow state names: a status map is shared
/// by all of a workspace's teams, so a name is known when any team has it.
/// `fetch` answers one page of teams for a cursor.
pub fn status_names_with(
    mut fetch: impl FnMut(Option<&str>) -> Result<Value>,
) -> Result<Vec<String>> {
    let mut names: Vec<String> = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let resp = fetch(after.as_deref())?;
        let teams = resp
            .pointer("/data/teams")
            .filter(|v| !v.is_null())
            .context("Linear's team list returned no teams field")?;
        let states = teams["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|team| team.pointer("/states/nodes").and_then(Value::as_array))
            .flatten()
            .filter_map(|state| state["name"].as_str());
        for name in states {
            if !names.iter().any(|have| same_status(have, name)) {
                names.push(name.to_string());
            }
        }
        match next_cursor(teams) {
            Some(cursor) => after = Some(cursor),
            None => return Ok(names),
        }
    }
}

/// Linear's issues, read for a sync through the API key `key`.
pub struct LinearTickets<'a> {
    pub key: &'a str,
}

impl LinearTickets<'_> {
    fn page(
        &self,
        query: &str,
        filter: &Value,
        after: Option<&str>,
        detail: &str,
    ) -> Result<Value> {
        post_graphql_with(
            query,
            json!({ "filter": filter, "after": after }),
            self.key,
            detail,
        )
    }

    pub fn find(&self, rules: &[MatchRule], closed_names: &[String]) -> Result<Vec<SyncedTicket>> {
        find_with(rules, closed_names, |filter, after| {
            self.page(ISSUES_QUERY, filter, after, "sync issues")
        })
    }

    pub fn read(&self, keys: &[String]) -> Result<Vec<SyncedTicket>> {
        read_with(keys, |filter, after| {
            self.page(ISSUES_QUERY, filter, after, "sync issues")
        })
    }

    pub fn status_names(&self) -> Result<Vec<String>> {
        status_names_with(|after| {
            post_graphql_with(
                TEAMS_QUERY,
                json!({ "after": after }),
                self.key,
                "sync team states",
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use devkit_config::{Assignee, ProjectRole};
    use serde_json::json;

    use super::*;
    use crate::tracker::tickets::TicketStatus;

    fn rule(assignee: Option<Assignee>, role: Option<ProjectRole>, labels: &[&str]) -> MatchRule {
        MatchRule {
            assignee,
            project_role: role,
            labels: labels.iter().map(|l| l.to_string()).collect(),
        }
    }

    fn issue(identifier: &str, state: &str, extra: Value) -> Value {
        let mut v = json!({
            "identifier": identifier,
            "title": format!("Title of {identifier}"),
            "state": { "name": state },
            "parent": null,
            "inverseRelations": { "nodes": [] },
        });
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        v
    }

    fn page(nodes: Vec<Value>, next: Option<&str>) -> Value {
        json!({ "data": { "issues": {
            "pageInfo": { "hasNextPage": next.is_some(), "endCursor": next },
            "nodes": nodes,
        }}})
    }

    fn only(resp: &Value) -> SyncedTicket {
        let (mut tickets, _) = parse_page(resp).unwrap();
        assert_eq!(tickets.len(), 1, "{tickets:?}");
        tickets.remove(0)
    }

    fn open() -> Value {
        json!({ "state": { "type": { "nin": ["completed", "canceled", "duplicate"] } } })
    }

    #[test]
    fn rule_filter_is_built_from_conditions() {
        let me = rule(Some(Assignee::Me), None, &[]);
        assert_eq!(
            rule_filter(&me, &[]),
            json!({ "and": [{ "assignee": { "isMe": { "eq": true } } }, open()] })
        );

        let unassigned_lead = rule(Some(Assignee::Nobody), Some(ProjectRole::Lead), &[]);
        assert_eq!(
            rule_filter(&unassigned_lead, &[]),
            json!({ "and": [
                { "assignee": { "null": true } },
                { "project": { "lead": { "isMe": { "eq": true } } } },
                open(),
            ]})
        );

        let member = rule(None, Some(ProjectRole::Member), &[]);
        assert_eq!(
            rule_filter(&member, &[]),
            json!({ "and": [{ "project": { "or": [
                { "lead": { "isMe": { "eq": true } } },
                { "members": { "some": { "isMe": { "eq": true } } } },
            ]}}, open()]})
        );

        let labelled = rule(None, None, &["bug", "Backend"]);
        assert_eq!(
            rule_filter(&labelled, &[]),
            json!({ "and": [
                { "labels": { "some": { "name": { "eqIgnoreCase": "bug" } } } },
                { "labels": { "some": { "name": { "eqIgnoreCase": "Backend" } } } },
                open(),
            ]})
        );

        let named = rule(Some(Assignee::User("lev".into())), None, &[]);
        assert_eq!(
            rule_filter(&named, &[]),
            json!({ "and": [{ "assignee": { "or": [
                { "name": { "eqIgnoreCase": "lev" } },
                { "displayName": { "eqIgnoreCase": "lev" } },
            ]}}, open()]})
        );
    }

    #[test]
    fn closed_names_are_excluded_by_state_name() {
        let closed = vec!["Done".to_string(), " canceled ".to_string()];
        let filter = rule_filter(&rule(Some(Assignee::Me), None, &[]), &closed);
        assert_eq!(
            filter,
            json!({ "and": [
                { "assignee": { "isMe": { "eq": true } } },
                open(),
                { "state": { "name": { "neqIgnoreCase": "Done" } } },
                { "state": { "name": { "neqIgnoreCase": "canceled" } } },
            ]})
        );

        // A state Linear spells differently from the map still stays out.
        let found = find_with(&[rule(Some(Assignee::Me), None, &[])], &closed, |_, _| {
            Ok(page(
                vec![
                    issue("SWE-1", "Todo", json!({})),
                    issue("SWE-2", "Canceled", json!({})),
                ],
                None,
            ))
        })
        .unwrap();
        let keys: Vec<&str> = found.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(keys, ["SWE-1"]);
    }

    #[test]
    fn every_rule_is_read_to_its_last_page_and_each_issue_kept_once() {
        let rules = [
            rule(Some(Assignee::Me), None, &[]),
            rule(Some(Assignee::Nobody), Some(ProjectRole::Lead), &[]),
        ];
        let mut asked: Vec<(Value, Option<String>)> = Vec::new();
        let found = find_with(&rules, &[], |filter, after| {
            asked.push((filter.clone(), after.map(String::from)));
            Ok(match (asked.len(), after) {
                (1, None) => page(vec![issue("SWE-1", "Todo", json!({}))], Some("c1")),
                (2, Some("c1")) => page(vec![issue("SWE-2", "Todo", json!({}))], None),
                (3, None) => page(
                    vec![
                        issue("SWE-2", "Todo", json!({})),
                        issue("OPS-7", "Backlog", json!({})),
                    ],
                    None,
                ),
                other => panic!("unexpected page request {other:?}"),
            })
        })
        .unwrap();
        let keys: Vec<&str> = found.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(keys, ["SWE-1", "SWE-2", "OPS-7"]);
        assert_eq!(asked[0].0, asked[1].0);
        assert_ne!(asked[0].0, asked[2].0);
    }

    #[test]
    fn an_issue_reads_its_key_title_and_state() {
        let t = only(&page(vec![issue("SWE-12", "In Progress", json!({}))], None));
        assert_eq!(t.key, "SWE-12");
        assert_eq!(t.title, "Title of SWE-12");
        assert_eq!(t.status, TicketStatus::Named("In Progress".into()));
        assert_eq!(t.parent, None);
        assert!(t.blocked_by.is_empty());
    }

    #[test]
    fn blocks_relations_become_blocked_by_on_the_blocked_issue() {
        // SWE-5 is the related side of SWE-9's and OPS-2's `blocks`
        // relations, and of a `related` one that blocks nothing.
        let t = only(&page(
            vec![issue(
                "SWE-5",
                "Todo",
                json!({ "inverseRelations": { "nodes": [
                    { "type": "blocks", "issue": { "identifier": "SWE-9" } },
                    { "type": "related", "issue": { "identifier": "SWE-3" } },
                    { "type": "blocks", "issue": { "identifier": "OPS-2" } },
                    { "type": "blocks", "issue": { "identifier": "SWE-9" } },
                ]}}),
            )],
            None,
        ));
        assert_eq!(t.blocked_by, ["OPS-2", "SWE-9"]);
    }

    #[test]
    fn parent_is_the_parent_identifier() {
        let t = only(&page(
            vec![issue(
                "SWE-6",
                "Todo",
                json!({ "parent": { "identifier": "SWE-1" } }),
            )],
            None,
        ));
        assert_eq!(t.parent.as_deref(), Some("SWE-1"));
    }

    #[test]
    fn a_rule_without_conditions_selects_every_open_issue() {
        assert_eq!(
            rule_filter(&MatchRule::default(), &[]),
            json!({ "and": [
                { "state": { "type": { "nin": ["completed", "canceled", "duplicate"] } } },
            ]})
        );
    }

    #[test]
    fn tickets_are_read_by_team_key_and_number() {
        let keys = [
            "swe-12".to_string(),
            "not a key".to_string(),
            "OPS-3".to_string(),
        ];
        let mut asked = Vec::new();
        let found = read_with(&keys, |filter, _| {
            asked.push(filter.clone());
            Ok(page(vec![issue("SWE-12", "Done", json!({}))], None))
        })
        .unwrap();
        assert_eq!(found[0].status, TicketStatus::Named("Done".into()));
        assert_eq!(asked, [json!({ "or": [
            { "team": { "key": { "eq": "SWE" } }, "number": { "eq": 12 } },
            { "team": { "key": { "eq": "OPS" } }, "number": { "eq": 3 } },
        ]})]);
        assert!(
            read_with(&["".to_string()], |_, _| panic!("no key, no request"))
                .unwrap()
                .is_empty()
        );
    }

    fn teams(nodes: Value) -> Value {
        json!({ "data": { "teams": {
            "pageInfo": { "hasNextPage": false, "endCursor": null },
            "nodes": nodes,
        }}})
    }

    #[test]
    fn status_names_are_every_teams_states() {
        let mut asked = Vec::new();
        let names = status_names_with(|after| {
            asked.push(after.map(String::from));
            Ok(match after {
                None => json!({ "data": { "teams": {
                    "pageInfo": { "hasNextPage": true, "endCursor": "c1" },
                    "nodes": [{ "key": "SWE", "states": { "nodes": [
                        { "name": "Triage" }, { "name": "Todo" },
                        { "name": "In Review" }, { "name": "Done" },
                    ]}}],
                }}}),
                Some(_) => teams(json!([{ "key": "OPS", "states": { "nodes": [
                    { "name": "todo" }, { "name": "Blocked" }, { "name": "Done" },
                ]}}])),
            })
        })
        .unwrap();
        assert_eq!(names, ["Triage", "Todo", "In Review", "Done", "Blocked"]);
        assert_eq!(asked, [None, Some("c1".to_string())]);
    }
}
