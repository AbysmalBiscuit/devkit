//! GitHub issues as `devkit todo sync` reads them.
//!
//! Split like `github.rs`: query builders and parsers tested over canned
//! responses, and networked wrappers. Where GitHub refuses GraphQL, REST
//! reads the list, then each issue's blockers and parent.

use anyhow::{Context, Result, bail};
use devkit_config::{Assignee, GithubConfig, MatchRule, ProjectRef};
use serde_json::Value;

use super::{
    github_status::{Board, board_fields, check, parse_board},
    tickets::{CloseReason, SyncedTicket, TicketStatus, in_closed_name},
};
use crate::{
    forge::{Repo, rest::encode},
    github::{Api, Method, each, graphql_refused},
};

/// The board a sync reads statuses from: `[github] project` and its status
/// field, `None` without a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardRef {
    pub project: ProjectRef,
    pub field: String,
}

impl BoardRef {
    pub fn of(github: &GithubConfig) -> Option<BoardRef> {
        Some(BoardRef {
            project: github.project.clone()?,
            field: github.status_field().to_string(),
        })
    }
}

/// The issue fields a sync reads, with the item on `board`'s project when
/// there is one.
fn issue_fields(board: Option<&BoardRef>) -> String {
    let items = match board {
        Some(b) => format!(
            "projectItems(first: 100) {{ nodes {{ project {{ id }} \
             fieldValueByName(name: {f}) {{ ... on ProjectV2ItemFieldSingleSelectValue {{ name }} }} }} }}",
            f = Value::from(b.field.as_str()),
        ),
        None => String::new(),
    };
    format!(
        "number title state stateReason \
         parent {{ number repository {{ nameWithOwner }} }} \
         blockedBy(first: 100) {{ nodes {{ number repository {{ nameWithOwner }} }} }} {items}"
    )
}

fn board_part(slug: &str, board: Option<&BoardRef>) -> String {
    board
        .map(|b| board_fields(slug, &b.project, &b.field))
        .unwrap_or_default()
}

/// The search qualifiers `rule` stands for, scoped to open issues of `slug`.
/// A project role is Linear's alone, so a rule naming one is refused.
pub fn search_terms(slug: &str, rule: &MatchRule) -> Result<String> {
    if rule.project_role.is_some() {
        bail!("[ticket.sync.rules] project_role: GitHub has no project roles");
    }
    let mut terms = vec![format!("repo:{slug}"), "is:issue".into(), "is:open".into()];
    match &rule.assignee {
        Some(Assignee::Me) => terms.push("assignee:@me".into()),
        Some(Assignee::Nobody) => terms.push("no:assignee".into()),
        Some(Assignee::User(login)) => terms.push(format!("assignee:{login}")),
        None => {}
    }
    for label in &rule.labels {
        if label.trim().is_empty() {
            bail!("[ticket.sync.rules] labels: a label is blank");
        }
        terms.push(format!("label:{}", Value::from(label.trim())));
    }
    Ok(terms.join(" "))
}

/// One page of the issues `terms` select.
pub fn search_query(
    slug: &str,
    terms: &str,
    board: Option<&BoardRef>,
    after: Option<&str>,
) -> String {
    let after = after
        .map(|c| format!(", after: {}", Value::from(c)))
        .unwrap_or_default();
    format!(
        "query {{\n  search(query: {q}, type: ISSUE, first: 100{after}) {{\n    \
         pageInfo {{ hasNextPage endCursor }}\n    \
         nodes {{ ... on Issue {{ {fields} }} }}\n  }}\n  {board}\n}}",
        q = Value::from(terms),
        fields = issue_fields(board),
        board = board_part(slug, board),
    )
}

/// The issues numbered `numbers`, one alias apiece.
pub fn issues_query(slug: &str, numbers: &[u64], board: Option<&BoardRef>) -> String {
    let (owner, name) = slug.split_once('/').unwrap_or((slug, ""));
    let fields = issue_fields(board);
    let issues: Vec<String> = numbers
        .iter()
        .map(|n| format!("i{n}: issue(number: {n}) {{ {fields} }}"))
        .collect();
    format!(
        "query {{\n  repository(owner: {o}, name: {r}) {{ {issues} }}\n  {board}\n}}",
        o = Value::from(owner),
        r = Value::from(name),
        issues = issues.join(" "),
        board = board_part(slug, board),
    )
}

/// The key of issue `number` of `slug`: `owner/repo#65`, so two
/// repositories' issues with one number are two tickets. GitHub names a
/// repository in any case, so the slug is lowercased: a slug configured
/// in another case still names the same tickets.
fn ticket_key(slug: &str, number: u64) -> String {
    format!("{}#{number}", slug.to_ascii_lowercase())
}

/// The issue number `key` names when it is a [`ticket_key`] of `slug`.
fn key_number(key: &str, slug: &str) -> Option<u64> {
    let (repo, number) = key.rsplit_once('#')?;
    repo.eq_ignore_ascii_case(slug)
        .then(|| number.parse().ok())
        .flatten()
}

/// The key of the issue `v` names when it is an issue of `slug`.
fn key_in(v: &Value, slug: &str) -> Option<String> {
    let repo = v.pointer("/repository/nameWithOwner")?.as_str()?;
    repo.eq_ignore_ascii_case(slug)
        .then(|| v["number"].as_u64())
        .flatten()
        .map(|n| ticket_key(slug, n))
}

/// A closed issue's reason, from GraphQL's `stateReason` or REST's
/// `state_reason`, either case.
fn close_reason(reason: Option<&str>) -> CloseReason {
    match reason.map(str::to_ascii_uppercase).as_deref() {
        Some("NOT_PLANNED") => CloseReason::NotPlanned,
        Some("DUPLICATE") => CloseReason::Duplicate,
        _ => CloseReason::Completed,
    }
}

/// One issue node of a [`search_query`] or [`issues_query`] response.
/// `board` is the configured project, whose item holds an open issue's
/// status; without one every open issue is `Named("")`.
pub fn parse_issue(v: &Value, slug: &str, board: Option<&Board>) -> Option<SyncedTicket> {
    let number = v["number"].as_u64()?;
    let status = if v["state"].as_str() == Some("CLOSED") {
        TicketStatus::Closed(close_reason(v["stateReason"].as_str()))
    } else {
        let option = board.and_then(|board| {
            v.pointer("/projectItems/nodes")?
                .as_array()?
                .iter()
                .find(|item| {
                    item.pointer("/project/id").and_then(Value::as_str)
                        == Some(board.project_id.as_str())
                })?
                .pointer("/fieldValueByName/name")?
                .as_str()
        });
        TicketStatus::Named(option.unwrap_or_default().to_string())
    };
    let mut blocked_by: Vec<String> = v
        .pointer("/blockedBy/nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|b| key_in(b, slug))
        .collect();
    blocked_by.sort();
    blocked_by.dedup();
    Some(SyncedTicket {
        key: ticket_key(slug, number),
        title: v["title"].as_str().unwrap_or_default().to_string(),
        status,
        blocked_by,
        parent: v
            .get("parent")
            .filter(|p| !p.is_null())
            .and_then(|p| key_in(p, slug)),
    })
}

/// The board in a response when a project is configured.
fn board_in(resp: &Value, board: Option<&BoardRef>) -> Result<Option<Board>> {
    board.map(|b| parse_board(resp, &b.field)).transpose()
}

/// The issues on one [`search_query`] page and the cursor of the next.
pub fn parse_search(
    resp: &Value,
    slug: &str,
    board: Option<&BoardRef>,
) -> Result<(Vec<SyncedTicket>, Option<String>)> {
    let board = board_in(resp, board)?;
    let search = resp
        .pointer("/data/search")
        .context("GitHub's issue search returned no results field")?;
    let tickets = search["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| parse_issue(v, slug, board.as_ref()))
        .collect();
    let page = &search["pageInfo"];
    let next = match page["hasNextPage"].as_bool() {
        Some(true) => page["endCursor"].as_str().map(String::from),
        _ => None,
    };
    Ok((tickets, next))
}

/// The issues an [`issues_query`] response found. A number with no issue
/// is left out.
pub fn parse_issues(
    resp: &Value,
    slug: &str,
    board: Option<&BoardRef>,
) -> Result<Vec<SyncedTicket>> {
    let board = board_in(resp, board)?;
    let repo = resp
        .pointer("/data/repository")
        .filter(|r| !r.is_null())
        .context("[github] issues_repo was not found")?;
    Ok(repo
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, v)| parse_issue(v, slug, board.as_ref()))
        .collect())
}

/// The status names a board offers: its options, and `""` for an open
/// issue with none. Without a project, `""` alone.
pub fn status_names(board: Option<&Board>) -> Vec<String> {
    let mut names = vec![String::new()];
    if let Some(board) = board {
        names.extend(board.options.each_name().map(String::from));
    }
    names
}

/// The GitHub issues of one repository, read for a sync.
pub struct GithubTickets<'a> {
    pub repo: &'a Repo,
    pub api: &'a Api,
    pub board: Option<BoardRef>,
}

impl GithubTickets<'_> {
    fn send(&self, query: &str) -> Result<Value> {
        let resp = self.api.graphql_or_gh(query, Api::graphql_value)?;
        check(&resp, self.api.token_source())?;
        Ok(resp)
    }

    /// Where GitHub refuses GraphQL, REST can read every field a sync needs
    /// but a project's status.
    fn rest_allowed(&self, refused: anyhow::Error) -> Result<()> {
        match &self.board {
            Some(_) => Err(refused.context(
                "reading [github] project statuses needs GitHub's GraphQL API, which refused",
            )),
            None => Ok(()),
        }
    }

    /// Every open issue a rule selects, but those in one of `closed_names`,
    /// each once.
    pub fn find(&self, rules: &[MatchRule], closed_names: &[String]) -> Result<Vec<SyncedTicket>> {
        let slug = &self.repo.slug;
        let mut found: Vec<SyncedTicket> = Vec::new();
        for rule in rules {
            let terms = search_terms(slug, rule)?;
            let mut after: Option<String> = None;
            loop {
                let query = search_query(slug, &terms, self.board.as_ref(), after.as_deref());
                let (page, next) = match self.send(&query) {
                    Ok(resp) => parse_search(&resp, slug, self.board.as_ref())?,
                    Err(e) if graphql_refused(&e) => {
                        self.rest_allowed(e)?;
                        (self.rest_find(rule)?, None)
                    }
                    Err(e) => return Err(e),
                };
                for ticket in page {
                    if !found.iter().any(|t| t.key == ticket.key) {
                        found.push(ticket);
                    }
                }
                match next {
                    Some(cursor) => after = Some(cursor),
                    None => break,
                }
            }
        }
        found.retain(|t| !in_closed_name(t, closed_names));
        Ok(found)
    }

    /// The issues `keys` name, whatever their state. A key that names no
    /// issue of this repository is left out.
    pub fn read(&self, keys: &[String]) -> Result<Vec<SyncedTicket>> {
        let slug = &self.repo.slug;
        let numbers: Vec<u64> = keys.iter().filter_map(|k| key_number(k, slug)).collect();
        if numbers.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for chunk in numbers.chunks(50) {
            let query = issues_query(slug, chunk, self.board.as_ref());
            match self.send(&query) {
                Ok(resp) => out.extend(parse_issues(&resp, slug, self.board.as_ref())?),
                Err(e) if graphql_refused(&e) => {
                    self.rest_allowed(e)?;
                    out.extend(self.rest_read(chunk)?);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// The status names the configured board offers, and `""`.
    pub fn status_names(&self) -> Result<Vec<String>> {
        let Some(b) = &self.board else {
            return Ok(status_names(None));
        };
        let query = super::github_status::board_query(&self.repo.slug, &b.project, &b.field);
        let board = parse_board(&self.send(&query)?, &b.field)?;
        Ok(status_names(Some(&board)))
    }

    fn rest_find(&self, rule: &MatchRule) -> Result<Vec<SyncedTicket>> {
        let slug = &self.repo.slug;
        search_terms(slug, rule)?;
        let mut path = format!("/repos/{slug}/issues?state=open");
        match &rule.assignee {
            Some(Assignee::Me) => {
                let login = super::github::viewer_login(self.api)?;
                path.push_str(&format!("&assignee={}", encode(&login)));
            }
            Some(Assignee::Nobody) => path.push_str("&assignee=none"),
            Some(Assignee::User(login)) => path.push_str(&format!("&assignee={}", encode(login))),
            None => {}
        }
        if !rule.labels.is_empty() {
            let labels: Vec<&str> = rule.labels.iter().map(|l| l.trim()).collect();
            path.push_str(&format!("&labels={}", encode(&labels.join(","))));
        }
        let listed = super::github::rest_pages(self.api, &path)?;
        let issues: Vec<&Value> = listed
            .iter()
            .filter(|v| v.get("pull_request").is_none())
            .collect();
        self.rest_tickets(&issues)
    }

    fn rest_read(&self, numbers: &[u64]) -> Result<Vec<SyncedTicket>> {
        let slug = &self.repo.slug;
        let read = each(numbers, |n| {
            self.api
                .rest(Method::GET, &format!("/repos/{slug}/issues/{n}"), None)
        });
        let issues: Vec<Value> = read
            .into_iter()
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .filter(|v| v.get("pull_request").is_none())
            .collect();
        self.rest_tickets(&issues.iter().collect::<Vec<_>>())
    }

    /// Each REST issue as a ticket, with its blockers and parent read one
    /// issue at a time.
    fn rest_tickets(&self, issues: &[&Value]) -> Result<Vec<SyncedTicket>> {
        let slug = &self.repo.slug;
        let links = each(issues, |v| -> Result<(Vec<String>, Option<String>)> {
            let n = v["number"].as_u64().context("an issue without a number")?;
            let blocked = v
                .pointer("/issue_dependencies_summary/total_blocked_by")
                .and_then(Value::as_u64)
                .is_none_or(|count| count > 0);
            let blockers = match blocked {
                true => super::github::rest_pages(
                    self.api,
                    &format!("/repos/{slug}/issues/{n}/dependencies/blocked_by"),
                )?,
                false => Vec::new(),
            };
            let parent = self.api.rest(
                Method::GET,
                &format!("/repos/{slug}/issues/{n}/parent"),
                None,
            )?;
            Ok((
                blockers
                    .iter()
                    .filter_map(|b| rest_key_in(b, slug))
                    .collect(),
                parent.and_then(|p| rest_key_in(&p, slug)),
            ))
        });
        issues
            .iter()
            .zip(links)
            .map(|(v, links)| {
                let (mut blocked_by, parent) = links?;
                blocked_by.sort();
                blocked_by.dedup();
                let status = match v["state"].as_str() {
                    Some("closed") => {
                        TicketStatus::Closed(close_reason(v["state_reason"].as_str()))
                    }
                    _ => TicketStatus::Named(String::new()),
                };
                Ok(SyncedTicket {
                    key: ticket_key(slug, v["number"].as_u64().unwrap_or_default()),
                    title: v["title"].as_str().unwrap_or_default().to_string(),
                    status,
                    blocked_by,
                    parent,
                })
            })
            .collect()
    }
}

/// The key of a REST issue when it is an issue of `slug`.
fn rest_key_in(v: &Value, slug: &str) -> Option<String> {
    let repo = v["repository_url"].as_str()?;
    repo.to_ascii_lowercase()
        .ends_with(&format!("/repos/{}", slug.to_ascii_lowercase()))
        .then(|| v["number"].as_u64())
        .flatten()
        .map(|n| ticket_key(slug, n))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const SLUG: &str = "o/r";

    fn board_ref() -> BoardRef {
        BoardRef {
            project: ProjectRef {
                owner: None,
                number: 3,
            },
            field: "Status".into(),
        }
    }

    /// A board answer for project 3 with the options `names`.
    fn board(names: &[&str]) -> Value {
        let options: Vec<Value> = names
            .iter()
            .enumerate()
            .map(|(i, n)| json!({ "id": format!("opt{i}"), "name": n }))
            .collect();
        json!({ "projectV2": {
            "id": "PVT_3",
            "field": { "id": "PVTSSF_status", "options": options }
        }})
    }

    fn issue(number: u64, extra: Value) -> Value {
        let mut v = json!({
            "number": number,
            "title": format!("Issue {number}"),
            "state": "OPEN",
            "stateReason": null,
            "parent": null,
            "blockedBy": { "nodes": [] },
            "projectItems": { "nodes": [] },
        });
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        v
    }

    fn search(nodes: Vec<Value>, next: Option<&str>) -> Value {
        json!({ "data": {
            "search": {
                "pageInfo": { "hasNextPage": next.is_some(), "endCursor": next },
                "nodes": nodes,
            },
            "repositoryOwner": board(&["Todo", "In progress", "Done"]),
        }})
    }

    fn only(resp: &Value) -> SyncedTicket {
        let (tickets, _) = parse_search(resp, SLUG, Some(&board_ref())).unwrap();
        assert_eq!(tickets.len(), 1, "{tickets:?}");
        tickets.into_iter().next().unwrap()
    }

    #[test]
    fn open_issue_in_project_maps_its_field_option() {
        let item = json!({ "projectItems": { "nodes": [
            { "project": { "id": "PVT_other" }, "fieldValueByName": { "name": "Done" } },
            { "project": { "id": "PVT_3" }, "fieldValueByName": { "name": "In progress" } },
        ]}});
        let ticket = only(&search(vec![issue(65, item)], None));
        assert_eq!(ticket.status, TicketStatus::Named("In progress".into()));
        assert_eq!(ticket.title, "Issue 65");
    }

    #[test]
    fn open_issue_outside_project_is_the_empty_name() {
        let ticket = only(&search(vec![issue(65, json!({}))], None));
        assert_eq!(ticket.status, TicketStatus::Named(String::new()));
        let unset = json!({ "projectItems": { "nodes": [
            { "project": { "id": "PVT_3" }, "fieldValueByName": null },
        ]}});
        let ticket = only(&search(vec![issue(66, unset)], None));
        assert_eq!(ticket.status, TicketStatus::Named(String::new()));
        let (tickets, _) =
            parse_search(&search(vec![issue(67, json!({}))], None), SLUG, None).unwrap();
        assert_eq!(tickets[0].status, TicketStatus::Named(String::new()));
    }

    #[test]
    fn closed_issue_maps_by_reason() {
        let in_progress =
            json!({ "project": { "id": "PVT_3" }, "fieldValueByName": { "name": "In progress" } });
        for (reason, want) in [
            ("COMPLETED", CloseReason::Completed),
            ("NOT_PLANNED", CloseReason::NotPlanned),
            ("DUPLICATE", CloseReason::Duplicate),
        ] {
            let closed = json!({
                "state": "CLOSED",
                "stateReason": reason,
                "projectItems": { "nodes": [in_progress.clone()] },
            });
            let ticket = only(&search(vec![issue(65, closed)], None));
            assert_eq!(ticket.status, TicketStatus::Closed(want), "{reason}");
        }
    }

    #[test]
    fn keys_name_the_repository_and_issue_number() {
        let linked = json!({
            "parent": { "number": 60, "repository": { "nameWithOwner": "O/R" } },
            "blockedBy": { "nodes": [
                { "number": 66, "repository": { "nameWithOwner": "o/r" } },
                { "number": 9, "repository": { "nameWithOwner": "other/repo" } },
                { "number": 64, "repository": { "nameWithOwner": "o/r" } },
            ]},
        });
        let ticket = only(&search(vec![issue(65, linked)], None));
        assert_eq!(ticket.key, "o/r#65");
        assert_eq!(ticket.blocked_by, ["o/r#64", "o/r#66"]);
        assert_eq!(ticket.parent.as_deref(), Some("o/r#60"));
        let foreign =
            json!({ "parent": { "number": 1, "repository": { "nameWithOwner": "x/y" } } });
        assert_eq!(only(&search(vec![issue(65, foreign)], None)).parent, None);
    }

    #[test]
    fn assignee_me_queries_the_viewer() {
        let rule = |assignee| MatchRule {
            assignee,
            ..Default::default()
        };
        assert_eq!(
            search_terms(SLUG, &rule(Some(Assignee::Me))).unwrap(),
            "repo:o/r is:issue is:open assignee:@me"
        );
        assert_eq!(
            search_terms(SLUG, &rule(Some(Assignee::Nobody))).unwrap(),
            "repo:o/r is:issue is:open no:assignee"
        );
        assert_eq!(
            search_terms(SLUG, &rule(Some(Assignee::User("lev".into())))).unwrap(),
            "repo:o/r is:issue is:open assignee:lev"
        );
        assert_eq!(
            search_terms(SLUG, &rule(None)).unwrap(),
            "repo:o/r is:issue is:open"
        );
    }

    #[test]
    fn labels_must_all_match() {
        let rule = MatchRule {
            labels: vec!["area:todo".into(), "needs review".into()],
            ..Default::default()
        };
        assert_eq!(
            search_terms(SLUG, &rule).unwrap(),
            r#"repo:o/r is:issue is:open label:"area:todo" label:"needs review""#
        );
        let query = search_query(SLUG, &search_terms(SLUG, &rule).unwrap(), None, Some("c1"));
        assert!(
            query.contains(r#"search(query: "repo:o/r is:issue is:open label:\"area:todo\" label:\"needs review\"", type: ISSUE, first: 100, after: "c1")"#),
            "{query}"
        );
    }

    #[test]
    fn a_blank_label_is_refused() {
        let rule = MatchRule {
            labels: vec!["  ".into()],
            ..Default::default()
        };
        let err = search_terms(SLUG, &rule).unwrap_err();
        assert!(err.to_string().contains("blank"), "{err:#}");
    }

    #[test]
    fn project_role_is_refused_on_github() {
        let rule = MatchRule {
            project_role: Some(devkit_config::ProjectRole::Lead),
            ..Default::default()
        };
        let err = search_terms(SLUG, &rule).unwrap_err();
        assert!(err.to_string().contains("project_role"), "{err:#}");
    }

    #[test]
    fn status_names_are_field_options_plus_empty() {
        let resp = json!({ "data": { "repositoryOwner": board(&["Todo", "Done"]) } });
        let board = parse_board(&resp, "Status").unwrap();
        assert_eq!(status_names(Some(&board)), ["", "Todo", "Done"]);
        assert_eq!(status_names(None), [""]);
    }

    #[test]
    fn a_search_page_names_the_next_cursor() {
        let (_, next) = parse_search(
            &search(vec![issue(1, json!({}))], Some("c2")),
            SLUG,
            Some(&board_ref()),
        )
        .unwrap();
        assert_eq!(next.as_deref(), Some("c2"));
    }

    #[test]
    fn issues_by_number_skip_a_missing_one() {
        let resp = json!({ "data": {
            "repository": { "i65": issue(65, json!({})), "i99": null },
            "repositoryOwner": board(&["Todo"]),
        }});
        let tickets = parse_issues(&resp, SLUG, Some(&board_ref())).unwrap();
        assert_eq!(tickets.len(), 1);
        assert_eq!(tickets[0].key, "o/r#65");
        assert_eq!(key_number("O/R#65", SLUG), Some(65));
        assert_eq!(key_number("x/y#65", SLUG), None);
        assert_eq!(key_number("65", SLUG), None);
        let query = issues_query(SLUG, &[65, 99], Some(&board_ref()));
        assert!(query.contains("i65: issue(number: 65)"), "{query}");
        assert!(query.contains("projectV2(number: 3)"), "{query}");
    }
}
