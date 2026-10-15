//! `ticket fields discover` on Linear: the viewer's teams, or the teams
//! `--team` names, with each team's cycles and projects and each project's
//! milestones, plus the workspace's labels, priority scale and users, and
//! the parse into the fields they offer. Each list is its own paged query,
//! since Linear multiplies a nested list's cost by its parent's page size.

use std::collections::{BTreeSet, HashMap};

use anyhow::{Context, Result, bail};
use devkit_config::{EnumValue, FieldType, Person, Place};
use serde_json::{Value as Json, json};

use super::discover::{
    Discovered, DiscoveredField, id_name, nodes, partly, read_all, str_of, unfinished,
};

/// How many of a list one request reads: Linear's largest page.
const PAGE: u32 = 250;

/// One page of the list `name`, after `$after`, with `args` added to its
/// arguments.
fn list(name: &str, args: &str, fields: &str) -> String {
    format!(
        "{name}(first: {PAGE}, after: $after{args}) {{ pageInfo {{ hasNextPage endCursor }} \
         nodes {{ {fields} }} }}"
    )
}

/// The query reading one page of the teams [`parse`] reads: the viewer's,
/// or with `named` those whose keys `$keys` holds.
fn teams_query(named: bool) -> String {
    let fields = "id key name issueEstimationType";
    if named {
        format!(
            "query($after: String, $keys: [String!]) {{ {} }}",
            list("teams", ", filter: { key: { in: $keys } }", fields)
        )
    } else {
        format!(
            "query($after: String) {{ viewer {{ {} }} }}",
            list("teams", "", fields)
        )
    }
}

/// One page of a list of the team `$team`.
fn team_query(name: &str, args: &str, fields: &str) -> String {
    format!(
        "query($team: String!, $after: String) {{ team(id: $team) {{ {} }} }}",
        list(name, args, fields)
    )
}

/// One page of the milestones of the project `$project`.
fn milestones_query() -> String {
    format!(
        "query($project: String!, $after: String) {{ project(id: $project) {{ {} }} }}",
        list("projectMilestones", "", "id name")
    )
}

/// One page of the workspace list `name`.
fn workspace_query(name: &str, fields: &str) -> String {
    format!("query($after: String) {{ {} }}", list(name, "", fields))
}

const PRIORITIES_QUERY: &str = "query { issuePriorityValues { priority label } }";

/// Send one GraphQL query with its variables, answering its response.
type Post<'a> = dyn FnMut(&str, Json) -> Result<Json> + 'a;

/// The list at `at` in what `query` answers with `vars`, read to its end.
fn read(post: &mut Post<'_>, query: &str, vars: &Json, at: &str) -> Result<Json> {
    read_all(at, |after| {
        let mut vars = vars.clone();
        vars["after"] = after.into();
        post(query, vars)
    })
}

/// Every list [`parse`] reads, each read to its end through `post`, laid
/// out as one response: the teams at `/data/viewer/teams`, or with `asked`
/// at `/data/teams`, each holding its `cycles` and its `projects`, each
/// project its `projectMilestones`; then `issueLabels`,
/// `issuePriorityValues` and `users`.
fn gather(asked: &[String], post: &mut Post<'_>) -> Result<Json> {
    let (query, vars, at) = if asked.is_empty() {
        (teams_query(false), json!({}), "/data/viewer/teams")
    } else {
        (teams_query(true), json!({ "keys": asked }), "/data/teams")
    };
    let mut teams = read(post, &query, &vars, at)?;
    let cycles = team_query(
        "cycles",
        ", filter: { isPast: { eq: false } }",
        "id number name",
    );
    let projects = team_query("projects", "", "id name");
    let milestones = milestones_query();
    let mut read_milestones: HashMap<String, Json> = HashMap::new();
    for team in teams
        .get_mut("nodes")
        .and_then(Json::as_array_mut)
        .into_iter()
        .flatten()
    {
        let Some(id) = str_of(team, "id").map(String::from) else {
            continue;
        };
        let vars = json!({ "team": id });
        team["cycles"] = read(post, &cycles, &vars, "/data/team/cycles")?;
        let mut found = read(post, &projects, &vars, "/data/team/projects")?;
        for project in found
            .get_mut("nodes")
            .and_then(Json::as_array_mut)
            .into_iter()
            .flatten()
        {
            let Some(pid) = str_of(project, "id").map(String::from) else {
                continue;
            };
            let ms = match read_milestones.get(&pid) {
                Some(ms) => ms.clone(),
                None => {
                    let vars = json!({ "project": pid });
                    let ms = read(post, &milestones, &vars, "/data/project/projectMilestones")?;
                    read_milestones.insert(pid, ms.clone());
                    ms
                }
            };
            project["projectMilestones"] = ms;
        }
        team["projects"] = found;
    }
    let labels = workspace_query("issueLabels", "id name isGroup team { key }");
    let users = workspace_query("users", "id name active");
    let mut data = json!({
        "issueLabels": read(post, &labels, &json!({}), "/data/issueLabels")?,
        "issuePriorityValues": post(PRIORITIES_QUERY, Json::Null)?
            .pointer_mut("/data/issuePriorityValues")
            .map(Json::take)
            .unwrap_or(Json::Null),
        "users": read(post, &users, &json!({}), "/data/users")?,
    });
    if asked.is_empty() {
        data["viewer"] = json!({ "teams": teams });
    } else {
        data["teams"] = teams;
    }
    Ok(json!({ "data": data }))
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
            aliases: Vec::new(),
        }),
    }
}

/// The fields a [`gather`] response offers, an enum the read found empty
/// included, so a refresh drops its stale values. A team is named by its
/// key, with its key and name as the aliases Linear also takes for it. A
/// team-bound value (a team's label, cycle, project or project milestone)
/// records the keys of the teams it is valid in; a workspace label records
/// none. The team field is read in part, since the read covers only the
/// viewer's or the named teams. The workflow state belongs to
/// `[ticket.events]` and is never offered. `asked` is the teams `--team`
/// named, each of which must be found.
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
        teams_read: keys.iter().map(|k| k.to_string()).collect(),
        ..Discovered::default()
    };
    for (pointer, what) in [
        (teams_at, "teams"),
        ("/issueLabels", "labels"),
        ("/users", "users"),
    ] {
        out.truncated.extend(partly(data.pointer(pointer), what));
    }

    let mut team_values = Vec::new();
    let (mut cycles, mut projects, mut milestones) = (Vec::new(), Vec::new(), Vec::new());
    let mut estimates = false;
    let (mut partial_cycles, mut partial_projects, mut partial_milestones) = (false, false, false);
    for team in &teams {
        let (Some(id), Some(key)) = (str_of(team, "id"), str_of(team, "key")) else {
            continue;
        };
        let mut aliases = vec![key.to_string()];
        if let Some(name) = str_of(team, "name").filter(|n| !n.eq_ignore_ascii_case(key)) {
            aliases.push(name.to_string());
        }
        team_values.push(EnumValue {
            name: key.to_string(),
            id: id.to_string(),
            teams: Vec::new(),
            aliases,
        });
        estimates |= str_of(team, "issueEstimationType").is_some_and(|t| t != "notUsed");
        partial_cycles |= unfinished(team.get("cycles"));
        partial_projects |= unfinished(team.get("projects"));
        out.truncated.extend(partly(
            team.get("cycles"),
            &format!("upcoming cycles of team {key}"),
        ));
        out.truncated.extend(partly(
            team.get("projects"),
            &format!("projects of team {key}"),
        ));
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
            partial_milestones |= unfinished(p.get("projectMilestones"));
            let what = format!("milestones of project {pname}");
            if let Some(t) = partly(p.get("projectMilestones"), &what)
                && !out.truncated.contains(&t)
            {
                out.truncated.push(t);
            }
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
                aliases: Vec::new(),
            })
        })
        .collect();

    let mut field = |name: &str, f: DiscoveredField| {
        out.fields.insert(name.to_string(), f);
    };
    let enumerated = |place: Place, values: Vec<EnumValue>| {
        DiscoveredField::new(FieldType::Enum, place).with_values(values)
    };
    field("team", enumerated(Place::Team, team_values).partly(true));
    field(
        "labels",
        enumerated(Place::Labels, labels)
            .list()
            .partly(unfinished(data.pointer("/issueLabels"))),
    );
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
    field(
        "cycle",
        enumerated(Place::Cycle, cycles).partly(partial_cycles),
    );
    field(
        "project",
        enumerated(Place::Project, projects).partly(partial_projects),
    );
    field(
        "project_milestone",
        enumerated(Place::ProjectMilestone, milestones)
            .partly(partial_projects || partial_milestones),
    );
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
    let resp = gather(&teams, &mut |query, vars| {
        super::linear::post_graphql_with(query, vars, key, "fields_discover")
    })?;
    parse(&resp, &teams, people)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn team(id: &str, key: &str, name: &str, project: Json) -> Json {
        json!({
            "id": id, "key": key, "name": name, "issueEstimationType": "notUsed",
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
                team("t-eng", "ENG", "Engineering", shared.clone()),
                team("t-ops", "OPS", "OPS", shared)
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
    fn an_enum_the_tracker_answers_empty_is_still_offered() {
        let mut resp = response();
        for team in resp["data"]["viewer"]["teams"]["nodes"]
            .as_array_mut()
            .unwrap()
        {
            team["cycles"]["nodes"] = json!([]);
            team["projects"]["nodes"] = json!([]);
        }
        let found = parse(&resp, &[], &people()).unwrap();
        for field in ["cycle", "project", "project_milestone"] {
            assert_eq!(found.fields[field].values, [], "{field}");
        }
    }

    #[test]
    fn a_team_records_its_key_and_name_as_aliases() {
        let found = parse(&response(), &[], &people()).unwrap();
        let aliases: Vec<(&str, &[String])> = found.fields["team"]
            .values
            .iter()
            .map(|v| (v.id.as_str(), v.aliases.as_slice()))
            .collect();
        assert_eq!(aliases, [
            ("t-eng", &s(&["ENG", "Engineering"])[..]),
            ("t-ops", &s(&["OPS"])[..]),
        ]);
    }

    #[test]
    fn a_list_read_in_part_marks_its_field() {
        let mut resp = response();
        resp["data"]["viewer"]["teams"]["nodes"][0]["cycles"]["pageInfo"] =
            json!({ "hasNextPage": true });
        let found = parse(&resp, &[], &people()).unwrap();
        assert!(found.fields["cycle"].partial);
        assert!(!found.fields["labels"].partial);
        assert_eq!(found.truncated, [
            "only the first 1 upcoming cycles of team ENG were read"
        ]);
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
        let mut sent = Vec::new();
        gather(&[], &mut |q, _| {
            sent.push(q.to_string());
            Ok(json!({ "data": null }))
        })
        .unwrap();
        assert!(sent.iter().all(|q| !q.contains("states")), "{sent:?}");
    }

    /// One page of `nodes`, with a next page at `next` when given.
    fn page(nodes: Json, next: Option<&str>) -> Json {
        json!({ "pageInfo": { "hasNextPage": next.is_some(), "endCursor": next }, "nodes": nodes })
    }

    #[test]
    fn each_list_is_read_on_its_own_to_its_end() {
        let mut sent = Vec::new();
        let resp = gather(&[], &mut |q, vars| {
            sent.push(q.to_string());
            let after = vars["after"].as_str();
            let at = |pointer: &str, v: Json| {
                let mut out = json!({ "data": {} });
                let mut at = &mut out["data"];
                for key in pointer.split('/').filter(|k| !k.is_empty()) {
                    at = &mut at[key];
                }
                *at = v;
                out
            };
            Ok(if q.contains("viewer") {
                at(
                    "viewer/teams",
                    page(
                        json!([
                            { "id": "t-eng", "key": "ENG", "issueEstimationType": "notUsed" },
                            { "id": "t-ops", "key": "OPS", "issueEstimationType": "notUsed" }
                        ]),
                        None,
                    ),
                )
            } else if q.contains("cycles(") {
                let id = vars["team"].as_str().unwrap();
                match after {
                    None => at(
                        "team/cycles",
                        page(
                            json!([{ "id": format!("{id}-c1"), "number": 1 }]),
                            Some("p2"),
                        ),
                    ),
                    Some(_) => at(
                        "team/cycles",
                        page(json!([{ "id": format!("{id}-c2"), "number": 2 }]), None),
                    ),
                }
            } else if q.contains("projects(") {
                at(
                    "team/projects",
                    page(json!([{ "id": "prj-1", "name": "Platform" }]), None),
                )
            } else if q.contains("projectMilestones(") {
                at(
                    "project/projectMilestones",
                    page(json!([{ "id": "ms-1", "name": "Beta" }]), None),
                )
            } else if q.contains("issueLabels(") {
                at("issueLabels", page(json!([]), None))
            } else if q.contains("users(") {
                at("users", page(json!([]), None))
            } else {
                json!({ "data": { "issuePriorityValues": [] } })
            })
        })
        .unwrap();
        assert!(
            sent.iter().all(|q| q.matches("first:").count() <= 1),
            "no list nests another: {sent:#?}"
        );
        assert_eq!(
            sent.iter()
                .filter(|q| q.contains("projectMilestones"))
                .count(),
            1,
            "a project two teams share is read once"
        );
        let found = parse(&resp, &[], &people()).unwrap();
        let cycles: Vec<String> = values(&found, "cycle").into_iter().map(|v| v.1).collect();
        assert_eq!(cycles, ["t-eng-c1", "t-eng-c2", "t-ops-c1", "t-ops-c2"]);
        assert_eq!(values(&found, "project_milestone")[0].2, s(&["ENG", "OPS"]));
        assert!(found.truncated.is_empty(), "{:?}", found.truncated);
    }

    #[test]
    fn team_option_limits_the_teams_read() {
        let mut sent = Vec::new();
        gather(&s(&["ENG"]), &mut |q, vars| {
            sent.push((q.to_string(), vars));
            Ok(json!({ "data": null }))
        })
        .unwrap();
        let (q, vars) = &sent[0];
        assert!(q.contains("filter: { key: { in: $keys } }"), "{q}");
        assert_eq!(vars["keys"], json!(["ENG"]));
        assert!(!q.contains("viewer"), "{q}");
        let mut resp = response();
        let teams = resp["data"]["viewer"]["teams"].take();
        resp["data"]["teams"] = teams;
        let e = parse(&resp, &s(&["ENG", "QA"]), &people()).unwrap_err();
        assert!(format!("{e}").contains("`QA`"), "{e}");
    }
}
