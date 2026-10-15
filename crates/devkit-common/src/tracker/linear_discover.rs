//! `ticket fields discover` on Linear: one query reading the viewer's teams,
//! or the teams `--team` names, with their cycles, projects and project
//! milestones, plus the workspace's labels, priority scale and users, and
//! the parse into the fields they offer.

use std::collections::{BTreeSet, HashMap};

use anyhow::{Context, Result, bail};
use devkit_config::{EnumValue, FieldType, Person, Place};
use serde_json::Value as Json;

use super::discover::{Discovered, DiscoveredField, id_name, nodes, str_of};

/// How many of each list one query reads.
const PAGE: u32 = 250;

/// The query reading what [`parse`] needs: the viewer's teams, or with
/// `teams` the teams of those keys.
pub fn query(teams: &[String]) -> String {
    let fields = format!(
        "pageInfo {{ hasNextPage }} nodes {{ id key name issueEstimationType \
         cycles(first: {PAGE}, filter: {{ isPast: {{ eq: false }} }}) {{ nodes {{ id number name }} }} \
         projects(first: {PAGE}) {{ nodes {{ id name projectMilestones(first: {PAGE}) {{ nodes {{ id name }} }} }} }} }}"
    );
    let teams = if teams.is_empty() {
        format!("viewer {{ teams(first: {PAGE}) {{ {fields} }} }}")
    } else {
        format!(
            "teams(first: {PAGE}, filter: {{ key: {{ in: {} }} }}) {{ {fields} }}",
            Json::from(teams.to_vec())
        )
    };
    format!(
        "query {{\n  {teams}\n  \
         issueLabels(first: {PAGE}) {{ pageInfo {{ hasNextPage }} nodes {{ id name isGroup team {{ key }} }} }}\n  \
         issuePriorityValues {{ priority label }}\n  \
         users(first: {PAGE}) {{ pageInfo {{ hasNextPage }} nodes {{ id name active }} }}\n}}"
    )
}

/// Add a value valid in `team` to `values`, or `team` to the value of the
/// same id already there.
fn add(values: &mut Vec<EnumValue>, id: &str, name: &str, team: &str) {
    match values.iter_mut().find(|v| v.id == id) {
        Some(v) if !v.teams.iter().any(|t| t == team) => v.teams.push(team.to_string()),
        Some(_) => {}
        None => values.push(EnumValue {
            name: name.to_string(),
            id: id.to_string(),
            teams: vec![team.to_string()],
        }),
    }
}

/// The fields a [`query`] response offers. A team-bound value (a team's
/// label, cycle, project or project milestone) records the keys of the
/// teams it is valid in; a workspace label records none. The workflow state
/// belongs to `[ticket.events]` and is never offered. `asked` is the teams
/// `--team` named, each of which must be found.
pub fn parse(
    resp: &Json,
    asked: &[String],
    people: &HashMap<String, Person>,
) -> Result<Discovered> {
    let data = resp.get("data").context("Linear answered no data")?;
    let teams_at = if asked.is_empty() {
        "/viewer/teams"
    } else {
        "/teams"
    };
    let teams: Vec<&Json> = nodes(data, teams_at).collect();
    let keys: BTreeSet<&str> = teams.iter().filter_map(|t| str_of(t, "key")).collect();
    if let Some(missing) = asked.iter().find(|k| !keys.contains(k.as_str())) {
        bail!("Linear has no team with the key `{missing}` that this key can read");
    }
    if teams.is_empty() {
        bail!("the Linear key reads no team: join a team, or name one with --team");
    }
    let mut out = Discovered {
        people_key: "linear",
        ..Discovered::default()
    };
    for (pointer, what) in [
        (teams_at, "teams"),
        ("/issueLabels", "labels"),
        ("/users", "users"),
    ] {
        if data.pointer(&format!("{pointer}/pageInfo/hasNextPage")) == Some(&Json::Bool(true)) {
            out.truncated
                .push(format!("only the first {PAGE} {what} were read"));
        }
    }

    let mut team_values = Vec::new();
    let (mut cycles, mut projects, mut milestones) = (Vec::new(), Vec::new(), Vec::new());
    let mut estimates = false;
    for team in &teams {
        let (Some(id), Some(key)) = (str_of(team, "id"), str_of(team, "key")) else {
            continue;
        };
        team_values.push(EnumValue {
            name: key.to_string(),
            id: id.to_string(),
            teams: Vec::new(),
        });
        estimates |= str_of(team, "issueEstimationType").is_some_and(|t| t != "notUsed");
        for c in nodes(team, "/cycles") {
            let Some(cid) = str_of(c, "id") else { continue };
            let name = match (
                str_of(c, "name").filter(|n| !n.is_empty()),
                c["number"].as_u64(),
            ) {
                (Some(n), _) => n.to_string(),
                (None, Some(n)) => format!("Cycle {n}"),
                (None, None) => cid.to_string(),
            };
            add(&mut cycles, cid, &name, key);
        }
        for p in nodes(team, "/projects") {
            let (Some(pid), Some(pname)) = (str_of(p, "id"), str_of(p, "name")) else {
                continue;
            };
            add(&mut projects, pid, pname, key);
            for m in nodes(p, "/projectMilestones") {
                if let (Some(mid), Some(mname)) = (str_of(m, "id"), str_of(m, "name")) {
                    add(&mut milestones, mid, mname, key);
                }
            }
        }
    }
    let labels: Vec<EnumValue> = nodes(data, "/issueLabels")
        .filter(|l| l["isGroup"].as_bool() != Some(true))
        .filter_map(|l| {
            let team = l.pointer("/team/key").and_then(Json::as_str);
            if team.is_some_and(|t| !keys.contains(t)) {
                return None;
            }
            Some(EnumValue {
                teams: team.map(String::from).into_iter().collect(),
                ..id_name(l)?
            })
        })
        .collect();
    let priorities: Vec<EnumValue> = data
        .get("issuePriorityValues")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let n = p["priority"].as_u64().filter(|n| *n != 0)?;
            Some(EnumValue {
                name: str_of(p, "label")?.to_string(),
                id: n.to_string(),
                teams: Vec::new(),
            })
        })
        .collect();

    let mut field = |name: &str, f: DiscoveredField| {
        out.fields.insert(name.to_string(), f);
    };
    let enumerated = |place: Place, values: Vec<EnumValue>| {
        DiscoveredField::new(FieldType::Enum, place).with_values(values)
    };
    field("team", enumerated(Place::Team, team_values));
    field("labels", enumerated(Place::Labels, labels).list());
    field(
        "assignee",
        DiscoveredField::new(FieldType::User, Place::Assignee),
    );
    if !priorities.is_empty() {
        field("priority", enumerated(Place::Priority, priorities));
    }
    if estimates {
        field(
            "estimate",
            DiscoveredField::new(FieldType::Number, Place::Estimate),
        );
    }
    if !cycles.is_empty() {
        field("cycle", enumerated(Place::Cycle, cycles));
    }
    if !projects.is_empty() {
        field("project", enumerated(Place::Project, projects));
    }
    if !milestones.is_empty() {
        field(
            "project_milestone",
            enumerated(Place::ProjectMilestone, milestones),
        );
    }
    field(
        "due_date",
        DiscoveredField::new(FieldType::Date, Place::DueDate),
    );
    field(
        "parent",
        DiscoveredField::new(FieldType::Issue, Place::Parent),
    );
    field(
        "blocked_by",
        DiscoveredField::new(FieldType::Issue, Place::BlockedBy).list(),
    );

    out.unmatched_users = nodes(data, "/users")
        .filter(|u| u["active"].as_bool() != Some(false))
        .filter_map(|u| {
            let id = str_of(u, "id")?;
            let known = people.values().any(|p| p.linear.as_deref() == Some(id));
            (!known).then(|| match str_of(u, "name").filter(|n| !n.is_empty()) {
                Some(name) => format!("{id} ({name})"),
                None => id.to_string(),
            })
        })
        .collect();
    Ok(out)
}

/// Read the fields of `teams`, or of the viewer's teams, from Linear.
pub fn fetch(key: &str, teams: &[String], people: &HashMap<String, Person>) -> Result<Discovered> {
    let teams: Vec<String> = teams
        .iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    let resp = super::linear::post_graphql(&query(&teams), key, "fields_discover")?;
    parse(&resp, &teams, people)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn team(id: &str, key: &str, project: Json) -> Json {
        json!({
            "id": id, "key": key, "name": key, "issueEstimationType": "notUsed",
            "cycles": { "nodes": [{ "id": format!("cyc-{key}"), "number": 3, "name": null }] },
            "projects": { "nodes": [project] },
            "states": { "nodes": [{ "id": "st-1", "name": "Todo", "type": "unstarted" }] }
        })
    }

    fn response() -> Json {
        let shared = json!({ "id": "prj-1", "name": "Platform",
            "projectMilestones": { "nodes": [{ "id": "ms-1", "name": "Beta" }] } });
        json!({ "data": {
            "viewer": { "teams": { "pageInfo": { "hasNextPage": false }, "nodes": [
                team("t-eng", "ENG", shared.clone()),
                team("t-ops", "OPS", shared)
            ] } },
            "issueLabels": { "pageInfo": { "hasNextPage": false }, "nodes": [
                { "id": "L1", "name": "infra", "isGroup": false, "team": { "key": "ENG" } },
                { "id": "L2", "name": "bug", "isGroup": false, "team": null },
                { "id": "L3", "name": "Area", "isGroup": true, "team": null },
                { "id": "L4", "name": "design", "isGroup": false, "team": { "key": "DES" } }
            ] },
            "issuePriorityValues": [
                { "priority": 0, "label": "No priority" },
                { "priority": 1, "label": "Urgent" },
                { "priority": 4, "label": "Low" }
            ],
            "users": { "pageInfo": { "hasNextPage": false }, "nodes": [
                { "id": "u-ada", "name": "Ada", "active": true },
                { "id": "u-bob", "name": "Bob", "active": true },
                { "id": "u-old", "name": "Gone", "active": false }
            ] }
        } })
    }

    fn people() -> HashMap<String, Person> {
        devkit_config::Config::parse("[people.ada]\nlinear = \"u-ada\"\n")
            .unwrap()
            .people
    }

    fn values(found: &Discovered, field: &str) -> Vec<(String, String, Vec<String>)> {
        found.fields[field]
            .values
            .iter()
            .map(|v| (v.name.clone(), v.id.clone(), v.teams.clone()))
            .collect()
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn team_scoped_labels_record_their_teams() {
        let found = parse(&response(), &[], &people()).unwrap();
        assert_eq!(values(&found, "labels"), [
            ("infra".to_string(), "L1".to_string(), s(&["ENG"])),
            ("bug".to_string(), "L2".to_string(), vec![]),
        ]);
        assert_eq!(values(&found, "cycle"), [
            ("Cycle 3".to_string(), "cyc-ENG".to_string(), s(&["ENG"])),
            ("Cycle 3".to_string(), "cyc-OPS".to_string(), s(&["OPS"])),
        ]);
        assert_eq!(values(&found, "project"), [(
            "Platform".to_string(),
            "prj-1".to_string(),
            s(&["ENG", "OPS"])
        )]);
        assert_eq!(values(&found, "project_milestone")[0].2, s(&["ENG", "OPS"]));
        assert_eq!(values(&found, "team"), [
            ("ENG".to_string(), "t-eng".to_string(), vec![]),
            ("OPS".to_string(), "t-ops".to_string(), vec![]),
        ]);
        assert_eq!(values(&found, "priority"), [
            ("Urgent".to_string(), "1".to_string(), vec![]),
            ("Low".to_string(), "4".to_string(), vec![]),
        ]);
        assert!(!found.fields.contains_key("estimate"), "no team estimates");
        assert_eq!(found.unmatched_users, ["u-bob (Bob)"]);
    }

    #[test]
    fn the_workflow_state_is_never_added() {
        let found = parse(&response(), &[], &people()).unwrap();
        assert!(
            found.fields.values().all(|f| f.r#where != Place::State),
            "{:?}",
            found.fields.keys()
        );
        assert!(!found.fields.contains_key("state") && !found.fields.contains_key("status"));
        assert!(!query(&[]).contains("states"));
    }

    #[test]
    fn team_option_limits_the_teams_read() {
        let q = query(&s(&["ENG"]));
        assert!(
            q.contains(r#"teams(first: 250, filter: { key: { in: ["ENG"] } })"#),
            "{q}"
        );
        assert!(!q.contains("viewer"), "{q}");
        let mut resp = response();
        let teams = resp["data"]["viewer"]["teams"].take();
        resp["data"]["teams"] = teams;
        let e = parse(&resp, &s(&["ENG", "QA"]), &people()).unwrap_err();
        assert!(format!("{e}").contains("`QA`"), "{e}");
    }
}
