//! `ticket fields discover` on GitHub: one query reading the issues
//! repository's labels, open milestones, assignable users and issue types,
//! and the fields of `[github] project`, and the parse into the fields they
//! offer.

use std::collections::HashMap;

use anyhow::{Context, Result};
use devkit_config::{EnumValue, FieldType, Person, Place, ProjectRef};
use serde_json::Value as Json;

use super::discover::{Discovered, DiscoveredField, field_name, id_name, named, nodes, str_of};
use crate::{forge::Repo, github::Api};

/// How many of each list one query reads.
const PAGE: u32 = 100;

/// The project field types GitHub fills itself rather than someone creating.
const BUILT_IN: &[&str] = &[
    "ASSIGNEES",
    "ISSUE_TYPE",
    "LABELS",
    "LINKED_PULL_REQUESTS",
    "MILESTONE",
    "PARENT_ISSUE",
    "REPOSITORY",
    "REVIEWERS",
    "SUB_ISSUES_PROGRESS",
    "TITLE",
    "TRACKED_BY",
    "TRACKS",
];

/// The query reading what [`parse`] needs from `slug` and `project`.
pub fn query(slug: &str, project: Option<&ProjectRef>) -> String {
    let (owner, name) = slug.split_once('/').unwrap_or((slug, ""));
    let more = "pageInfo { hasNextPage }";
    let board = project.map_or_else(String::new, |p| {
        format!(
            "\n  repositoryOwner(login: {po}) {{ ... on ProjectV2Owner {{ projectV2(number: {n}) {{ \
             fields(first: {PAGE}) {{ {more} nodes {{ \
             ... on ProjectV2FieldCommon {{ id name dataType }} \
             ... on ProjectV2SingleSelectField {{ options {{ id name }} }} }} }} }} }} }}",
            po = Json::from(p.owner.as_deref().unwrap_or(owner)),
            n = p.number,
        )
    });
    format!(
        "query {{\n  repository(owner: {o}, name: {n}) {{\n    \
         labels(first: {PAGE}) {{ {more} nodes {{ name }} }}\n    \
         milestones(first: {PAGE}, states: [OPEN]) {{ {more} nodes {{ title }} }}\n    \
         assignableUsers(first: {PAGE}) {{ {more} nodes {{ login name }} }}\n    \
         issueTypes(first: {PAGE}) {{ {more} nodes {{ id name }} }}\n  }}{board}\n}}",
        o = Json::from(owner),
        n = Json::from(name),
    )
}

/// The fields a [`query`] response offers. The project field named
/// `status_field` belongs to `[ticket.events]` and is never offered, nor is a
/// project field GitHub fills itself (its title, assignees, labels and the
/// like). Assignable users no `[people]` entry names, and created project
/// fields of a type devkit cannot write, are reported.
pub fn parse(
    resp: &Json,
    status_field: &str,
    people: &HashMap<String, Person>,
) -> Result<Discovered> {
    let repo = resp
        .pointer("/data/repository")
        .filter(|r| !r.is_null())
        .context("GitHub answered no repository: check `[github] issues_repo` and the token")?;
    let mut out = Discovered {
        people_key: "github",
        ..Discovered::default()
    };
    for (list, what) in [
        ("labels", "labels"),
        ("milestones", "open milestones"),
        ("assignableUsers", "assignable users"),
        ("issueTypes", "issue types"),
    ] {
        if repo.pointer(&format!("/{list}/pageInfo/hasNextPage")) == Some(&Json::Bool(true)) {
            out.truncated
                .push(format!("only the first {PAGE} {what} were read"));
        }
    }
    let field = |name: &str, f: DiscoveredField, out: &mut Discovered| {
        out.fields.insert(name.to_string(), f);
    };
    let labels: Vec<EnumValue> = nodes(repo, "/labels")
        .filter_map(|n| str_of(n, "name"))
        .map(named)
        .collect();
    field(
        "labels",
        DiscoveredField::new(FieldType::Enum, Place::Labels)
            .list()
            .with_values(labels),
        &mut out,
    );
    let milestones: Vec<EnumValue> = nodes(repo, "/milestones")
        .filter_map(|n| str_of(n, "title"))
        .map(named)
        .collect();
    field(
        "milestone",
        DiscoveredField::new(FieldType::Enum, Place::Milestone).with_values(milestones),
        &mut out,
    );
    field(
        "assignees",
        DiscoveredField::new(FieldType::User, Place::Assignees).list(),
        &mut out,
    );
    let types: Vec<EnumValue> = nodes(repo, "/issueTypes").filter_map(id_name).collect();
    if !types.is_empty() {
        field(
            "type",
            DiscoveredField::new(FieldType::Enum, Place::IssueType).with_values(types),
            &mut out,
        );
    }
    field(
        "parent",
        DiscoveredField::new(FieldType::Issue, Place::Parent),
        &mut out,
    );
    field(
        "blocked_by",
        DiscoveredField::new(FieldType::Issue, Place::BlockedBy).list(),
        &mut out,
    );
    if let Some(board) = resp
        .pointer("/data/repositoryOwner/projectV2")
        .filter(|b| !b.is_null())
    {
        field(
            "project",
            DiscoveredField::new(FieldType::String, Place::Project),
            &mut out,
        );
        if board.pointer("/fields/pageInfo/hasNextPage") == Some(&Json::Bool(true)) {
            out.truncated
                .push(format!("only the first {PAGE} project fields were read"));
        }
        for f in nodes(board, "/fields") {
            let (Some(id), Some(name), Some(data)) =
                (str_of(f, "id"), str_of(f, "name"), str_of(f, "dataType"))
            else {
                continue;
            };
            if name.trim().eq_ignore_ascii_case(status_field) {
                continue;
            }
            let ty = match data {
                "SINGLE_SELECT" => FieldType::Enum,
                "TEXT" => FieldType::String,
                "NUMBER" => FieldType::Number,
                "DATE" => FieldType::Date,
                _ if BUILT_IN.contains(&data) => continue,
                _ => {
                    out.unwritable.push(format!("`{name}` ({data})"));
                    continue;
                }
            };
            let values = f
                .get("options")
                .and_then(Json::as_array)
                .into_iter()
                .flatten()
                .filter_map(id_name)
                .collect();
            let mut key = field_name(name);
            if key.is_empty() || out.fields.contains_key(&key) {
                key = format!("project_{key}").trim_end_matches('_').to_string();
            }
            let place = Place::ProjectField {
                id: id.to_string(),
                name: name.to_string(),
            };
            field(
                &key,
                DiscoveredField::new(ty, place).with_values(values),
                &mut out,
            );
        }
    }
    out.unmatched_users = nodes(repo, "/assignableUsers")
        .filter_map(|u| {
            let login = str_of(u, "login")?;
            let known = people.values().any(|p| {
                p.github
                    .as_deref()
                    .is_some_and(|g| g.eq_ignore_ascii_case(login))
            });
            (!known).then(|| match str_of(u, "name").filter(|n| !n.is_empty()) {
                Some(name) => format!("{login} ({name})"),
                None => login.to_string(),
            })
        })
        .collect();
    Ok(out)
}

/// Read `repo`'s fields, and `project`'s, from GitHub.
pub fn fetch(
    repo: &Repo,
    project: Option<&ProjectRef>,
    status_field: &str,
    people: &HashMap<String, Person>,
) -> Result<Discovered> {
    let api = Api::new(&repo.host);
    let source = api.token_source();
    let resp = api.graphql_or_gh(&query(&repo.slug, project), Api::graphql_value)?;
    super::github_status::check(&resp, source)?;
    parse(&resp, status_field, people)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn response() -> Json {
        json!({ "data": {
            "repository": {
                "labels": { "pageInfo": { "hasNextPage": false }, "nodes": [{ "name": "bug" }, { "name": "area:todo" }] },
                "milestones": { "pageInfo": { "hasNextPage": false }, "nodes": [{ "title": "v1" }] },
                "assignableUsers": { "pageInfo": { "hasNextPage": true }, "nodes": [
                    { "login": "LevValle", "name": "Lev" },
                    { "login": "octo", "name": "Octo Cat" }
                ] },
                "issueTypes": { "pageInfo": { "hasNextPage": false }, "nodes": [{ "id": "IT_bug", "name": "Bug" }] }
            },
            "repositoryOwner": { "projectV2": { "fields": { "pageInfo": { "hasNextPage": false }, "nodes": [
                { "id": "PVTF_title", "name": "Title", "dataType": "TITLE" },
                { "id": "PVTSSF_s", "name": "Status", "dataType": "SINGLE_SELECT", "options": [{ "id": "o1", "name": "Todo" }] },
                { "id": "PVTSSF_p", "name": "Priority", "dataType": "SINGLE_SELECT", "options": [{ "id": "f75", "name": "P0" }, { "id": "47f", "name": "P1" }] },
                { "id": "PVTF_e", "name": "Story points", "dataType": "NUMBER" },
                { "id": "PVTF_l", "name": "Labels", "dataType": "TEXT" },
                { "id": "PVTIF_s", "name": "Sprint", "dataType": "ITERATION" },
                { "id": "PVTF_r", "name": "Reviewers", "dataType": "REVIEWERS" }
            ] } } }
        } })
    }

    fn people() -> HashMap<String, Person> {
        devkit_config::Config::parse("[people.lev]\ngithub = \"levvalle\"\n")
            .unwrap()
            .people
    }

    #[test]
    fn the_status_field_is_never_added() {
        let found = parse(&response(), "status", &people()).unwrap();
        assert!(
            !found.fields.values().any(|f| matches!(
                &f.r#where,
                Place::ProjectField { name, .. } if name == "Status"
            )),
            "{:?}",
            found.fields.keys()
        );
        assert!(!found.fields.contains_key("status"));
    }

    #[test]
    fn a_created_field_of_an_unsupported_type_is_reported_not_written() {
        let found = parse(&response(), "Status", &people()).unwrap();
        assert_eq!(found.unwritable, ["`Sprint` (ITERATION)"]);
        assert!(!found.fields.contains_key("sprint"));
    }

    #[test]
    fn every_place_is_offered_with_its_values() {
        let found = parse(&response(), "Status", &people()).unwrap();
        let names: Vec<&str> = found.fields.keys().map(String::as_str).collect();
        assert_eq!(names, [
            "assignees",
            "blocked_by",
            "labels",
            "milestone",
            "parent",
            "priority",
            "project",
            "project_labels",
            "story_points",
            "type"
        ]);
        let p = &found.fields["priority"];
        assert_eq!(p.r#type, FieldType::Enum);
        assert_eq!(p.values[1].name, "P1");
        assert_eq!(p.values[1].id, "47f");
        assert!(found.fields["labels"].list);
        assert_eq!(found.fields["type"].values[0].id, "IT_bug");
        assert_eq!(found.unmatched_users, ["octo (Octo Cat)"]);
        assert_eq!(found.truncated, [
            "only the first 100 assignable users were read"
        ]);
    }
}
