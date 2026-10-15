//! `ticket fields discover` on GitHub: one query reading the first page of
//! the issues repository's labels, open milestones, assignable users and
//! issue types, and of the fields of `[github] project`, then the rest of
//! each list page by page, and the parse into the fields they offer.

use std::collections::HashMap;

use anyhow::{Context, Result};
use devkit_config::{EnumValue, FieldType, Person, Place, ProjectRef};
use serde_json::Value as Json;

use super::discover::{
    Discovered, DiscoveredField, field_name, follow, id_name, id_name_taken_by_name, named, nodes,
    partly, spellings, str_of, unfinished,
};
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

/// Each repository list [`parse`] reads: its name, its arguments past the
/// page, the fields of each node, and what a note calls its items.
const LISTS: [(&str, &str, &str, &str); 4] = [
    ("labels", "", "name", "labels"),
    ("milestones", ", states: [OPEN]", "title number", "open milestones"),
    ("assignableUsers", "", "login name", "assignable users"),
    ("issueTypes", "", "id name", "issue types"),
];

/// One page of the list `name`, after `after` when given.
fn list(name: &str, args: &str, fields: &str, after: Option<&str>) -> String {
    let after = after.map_or_else(String::new, |c| format!(", after: {}", Json::from(c)));
    format!(
        "{name}(first: {PAGE}{after}{args}) {{ pageInfo {{ hasNextPage endCursor }} \
         nodes {{ {fields} }} }}"
    )
}

/// `slug`'s repository holding `lists`.
fn repository(slug: &str, lists: &str) -> String {
    let (owner, name) = slug.split_once('/').unwrap_or((slug, ""));
    format!(
        "repository(owner: {}, name: {}) {{ {lists} }}",
        Json::from(owner),
        Json::from(name)
    )
}

/// One page of `project`'s fields, after `after` when given. A
/// single-select field's options are a plain list GitHub answers whole.
fn board(slug: &str, project: &ProjectRef, after: Option<&str>) -> String {
    let owner = slug.split_once('/').map_or(slug, |(o, _)| o);
    format!(
        "repositoryOwner(login: {}) {{ ... on ProjectV2Owner {{ projectV2(number: {}) {{ {} }} }} }}",
        Json::from(project.owner.as_deref().unwrap_or(owner)),
        project.number,
        list(
            "fields",
            "",
            "... on ProjectV2FieldCommon { id name dataType } \
             ... on ProjectV2SingleSelectField { options { id name } }",
            after
        )
    )
}

/// The query reading the first page of every list [`parse`] needs from
/// `slug` and `project`.
pub fn query(slug: &str, project: Option<&ProjectRef>) -> String {
    let lists: Vec<String> = LISTS
        .iter()
        .map(|(name, args, fields, _)| list(name, args, fields, None))
        .collect();
    let board = project.map_or_else(String::new, |p| board(slug, p, None));
    format!("query {{ {} {board} }}", repository(slug, &lists.join(" ")))
}

/// The fields a [`query`] response offers, an enum the read found empty
/// included, so a refresh drops its stale values; issue types only where the
/// owner has them. The project field named
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
    for (list, _, _, what) in LISTS {
        out.truncated.extend(partly(repo.get(list), what));
    }
    let partial = |list: &str| unfinished(repo.get(list));
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
            .with_values(labels)
            .partly(partial("labels")),
        &mut out,
    );
    let milestones: Vec<EnumValue> = nodes(repo, "/milestones")
        .filter_map(|n| {
            let title = str_of(n, "title")?;
            let number = n["number"].as_u64().map(|n| n.to_string());
            Some(EnumValue {
                aliases: spellings(title, number.as_deref()),
                ..named(title)
            })
        })
        .collect();
    field(
        "milestone",
        DiscoveredField::new(FieldType::Enum, Place::Milestone)
            .with_values(milestones)
            .partly(partial("milestones")),
        &mut out,
    );
    field(
        "assignees",
        DiscoveredField::new(FieldType::User, Place::Assignees).list(),
        &mut out,
    );
    let types: Vec<EnumValue> = nodes(repo, "/issueTypes")
        .filter_map(id_name_taken_by_name)
        .collect();
    if repo.get("issueTypes").is_some_and(|t| !t.is_null()) {
        field(
            "type",
            DiscoveredField::new(FieldType::Enum, Place::IssueType)
                .with_values(types)
                .partly(partial("issueTypes")),
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
        out.truncated
            .extend(partly(board.get("fields"), "project fields"));
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
            let key = project_field_key(name, id, &out);
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

/// The key a project field called `name` with node id `id` is added under:
/// its name's, else that prefixed `project_`, else when its name has no
/// letter or digit or both are taken, its id's, numbered past any taken.
fn project_field_key(name: &str, id: &str, out: &Discovered) -> String {
    let free = |k: &String| !k.is_empty() && !out.fields.contains_key(k);
    let named = field_name(name);
    if !named.is_empty()
        && let Some(key) = [named.clone(), format!("project_{named}")]
            .into_iter()
            .find(free)
    {
        return key;
    }
    let base = match field_name(id) {
        b if b.is_empty() => "project_field".to_string(),
        b => b,
    };
    std::iter::once(base.clone())
        .chain((2..).map(|n| format!("{base}_{n}")))
        .find(free)
        .unwrap_or(base)
}

/// Read `repo`'s fields, and `project`'s, from GitHub, each list to its
/// end.
pub fn fetch(
    repo: &Repo,
    project: Option<&ProjectRef>,
    status_field: &str,
    people: &HashMap<String, Person>,
) -> Result<Discovered> {
    let api = Api::new(&repo.host);
    let source = api.token_source();
    let send = |query: &str| -> Result<Json> {
        let resp = api.graphql_or_gh(query, Api::graphql_value)?;
        super::github_status::check(&resp, source)?;
        Ok(resp)
    };
    let slug = &repo.slug;
    let mut resp = send(&query(slug, project))?;
    for (name, args, fields, _) in LISTS {
        follow(&mut resp, &format!("/data/repository/{name}"), |after| {
            send(&format!(
                "query {{ {} }}",
                repository(slug, &list(name, args, fields, Some(after)))
            ))
        })?;
    }
    if let Some(p) = project {
        follow(
            &mut resp,
            "/data/repositoryOwner/projectV2/fields",
            |after| send(&format!("query {{ {} }}", board(slug, p, Some(after)))),
        )?;
    }
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
                "milestones": { "pageInfo": { "hasNextPage": false }, "nodes": [{ "title": "v1", "number": 3 }] },
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
    fn issue_types_are_offered_empty_only_where_the_owner_has_them() {
        let mut resp = response();
        resp["data"]["repository"]["issueTypes"]["nodes"] = json!([]);
        let found = parse(&resp, "Status", &people()).unwrap();
        assert_eq!(found.fields["type"].values, []);
        resp["data"]["repository"]["issueTypes"] = Json::Null;
        let found = parse(&resp, "Status", &people()).unwrap();
        assert!(!found.fields.contains_key("type"));
    }

    #[test]
    fn a_project_field_never_takes_a_key_already_used() {
        let mut resp = response();
        let fields = resp
            .pointer_mut("/data/repositoryOwner/projectV2/fields/nodes")
            .and_then(Json::as_array_mut)
            .unwrap();
        fields.extend([
            json!({ "id": "PVTF_jp", "name": "優先度", "dataType": "TEXT" }),
            json!({ "id": "PVTF_sp2", "name": "Story-points", "dataType": "NUMBER" }),
            json!({ "id": "PVTF_sp3", "name": "story points!", "dataType": "NUMBER" }),
            json!({ "id": "PVTF-sp3", "name": "Story points?", "dataType": "NUMBER" }),
        ]);
        let found = parse(&resp, "Status", &people()).unwrap();
        assert_eq!(found.fields["project"].r#where, Place::Project);
        let at = |key: &str| match &found.fields[key].r#where {
            Place::ProjectField { id, .. } => id.clone(),
            other => panic!("{key}: {other:?}"),
        };
        assert_eq!(at("story_points"), "PVTF_e");
        assert_eq!(at("project_story_points"), "PVTF_sp2");
        assert_eq!(at("pvtf_jp"), "PVTF_jp");
        assert_eq!(at("pvtf_sp3"), "PVTF_sp3");
        assert_eq!(at("pvtf_sp3_2"), "PVTF-sp3");
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
        assert_eq!(
            found.fields["type"].values[0].aliases,
            ["Bug"],
            "issue_write takes a type by its name"
        );
        let v1 = &found.fields["milestone"].values[0];
        assert_eq!(
            (v1.id.as_str(), v1.aliases.as_slice()),
            ("v1", &["3".to_string()][..]),
            "`gh` takes a milestone by its title, issue_write by its number"
        );
        assert_eq!(found.unmatched_users, ["octo (Octo Cat)"]);
        assert_eq!(found.truncated, [
            "only the first 2 assignable users were read"
        ]);
        assert!(!found.fields["labels"].partial);
    }
}
