//! `devkit ticket fields`: the declared ticket fields at this caller's level,
//! read from config alone.

#[path = "common/ghfake.rs"]
mod ghfake;
#[path = "common/linearfake.rs"]
mod linearfake;

const FIELDS: &str = r#"
[tracker]
kind = "github"

[ticket.fields.github.estimate]
type        = "number"
where       = { project_field = "PVTF_e", name = "Estimate" }
level       = "info"
when        = [{ if = { caller = "agent" }, level = "error" }]
description = "story points"

[ticket.fields.github.project]
type  = "string"
where = "project"

[ticket.fields.github.milestone]
type   = "enum"
where  = "milestone"
values = [{ name = "v1", id = "v1" }]
level  = "none"
when   = [{ if = { project = "*" }, level = "warning" }]
"#;

/// A project declaring [`FIELDS`] on `[github] project` 1.
fn fields_fake() -> ghfake::Fake {
    let fake = ghfake::Fake::without_pr(FIELDS);
    fake.github_keys("project = 1");
    fake
}

fn stdout(out: &std::process::Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The line naming `field`, which leads its block.
fn line_of(text: &str, field: &str) -> String {
    text.lines()
        .find(|l| l.split_whitespace().next() == Some(field))
        .unwrap_or_else(|| panic!("no `{field}` in\n{text}"))
        .to_string()
}

#[test]
fn lists_fields_with_their_level_for_this_caller() {
    let fake = fields_fake();
    let agent = stdout(&fake.issue_as("agent", &["fields"]));
    assert!(line_of(&agent, "estimate").contains("error"), "{agent}");
    assert!(agent.contains("story points"), "{agent}");
    let human = stdout(&fake.issue_as("human", &["fields"]));
    assert!(line_of(&human, "estimate").contains("info"), "{human}");
    assert!(
        !human.lines().any(|l| l.starts_with("project")),
        "a field that can never rise above none is left out: {human}"
    );
}

#[test]
fn arg_values_evaluate_conditions() {
    let fake = fields_fake();
    let bare = stdout(&fake.issue_as("agent", &["fields"]));
    assert!(line_of(&bare, "milestone").contains("none"), "{bare}");
    let with = stdout(&fake.issue_as("agent", &["fields", "--arg", "project=devkit"]));
    assert!(line_of(&with, "milestone").contains("warning"), "{with}");
}

#[test]
fn json_is_the_field_views() {
    let fake = fields_fake();
    let out = stdout(&fake.issue_as("agent", &["fields", "--json"]));
    let v: serde_json::Value = serde_json::from_str(&out).expect("JSON");
    let items = v.as_array().expect("an array");
    assert_eq!(items.len(), 2, "{out}");
    let m = items
        .iter()
        .find(|i| i["name"] == "milestone")
        .expect("milestone");
    assert_eq!(m["level"], "none");
    assert_eq!(m["values"], serde_json::json!(["v1"]));
    assert_eq!(m["allows_none"], true);
}

#[test]
fn makes_no_tracker_request() {
    let fake = fields_fake();
    stdout(&fake.issue_as("agent", &["fields"]));
    assert_eq!(fake.calls(), "", "no gh call");
}

#[test]
fn both_tables_without_a_kind_use_the_detected_trackers() {
    let fake = ghfake::Fake::with_origin_and(
        "[github]\nproject = \"o/1\"\n[ticket.fields.github.estimate]\ntype = \"number\"\n\
         where = { project_field = \"PVTF_e\", name = \"Estimate\" }\nlevel = \"warning\"\n\
         [ticket.fields.linear.team]\nlevel = \"error\"\n",
        "https://github.com/o/r.git",
    );
    let out = stdout(&fake.issue_as("agent", &["fields"]));
    assert!(out.contains("estimate") && !out.contains("team"), "{out}");
    assert_eq!(fake.calls(), "", "no gh call");
    let rendered =
        stdout(&fake.issue_as("agent", &["render", "--title", "T", "--arg", "estimate=3"]));
    assert!(rendered.contains("\"title\""), "{rendered}");
}

/// What GitHub answers `ticket fields discover`'s read: two labels, one open
/// milestone, two assignable users, and on the project its status, an
/// estimate and a priority.
const DISCOVERED: &str = r#"{ "data": {
  "repository": {
    "labels": { "pageInfo": { "hasNextPage": false }, "nodes": [{ "name": "bug" }, { "name": "enhancement" }] },
    "milestones": { "pageInfo": { "hasNextPage": false }, "nodes": [{ "title": "v1" }, { "title": "v2" }] },
    "assignableUsers": { "pageInfo": { "hasNextPage": false }, "nodes": [
      { "login": "LevValle", "name": "Lev" }, { "login": "octo", "name": "Octo Cat" }
    ] },
    "issueTypes": { "pageInfo": { "hasNextPage": false }, "nodes": [] }
  },
  "repositoryOwner": { "projectV2": { "fields": { "pageInfo": { "hasNextPage": false }, "nodes": [
    { "id": "PVTSSF_s", "name": "Status", "dataType": "SINGLE_SELECT", "options": [{ "id": "o1", "name": "Todo" }] },
    { "id": "PVTF_e", "name": "Estimate", "dataType": "NUMBER" },
    { "id": "PVTSSF_p", "name": "Priority", "dataType": "SINGLE_SELECT", "options": [{ "id": "p0", "name": "P0" }] }
  ] } } }
} }"#;

fn devkit_toml(fake: &ghfake::Fake) -> String {
    std::fs::read_to_string(fake.project().join("devkit.toml")).unwrap()
}

#[test]
fn an_agent_writes_without_asking() {
    let fake = fields_fake();
    fake.serve_graphql(DISCOVERED);
    let before = devkit_toml(&fake);
    let out = stdout(&fake.issue_as("agent", &["fields", "discover"]));
    let after = devkit_toml(&fake);
    assert!(after.starts_with(&before[..before.find("[ticket.fields").unwrap()]));
    let cfg = devkit_config::Config::parse(&after).unwrap();
    let gh = &cfg.ticket.fields.github;
    assert_eq!(
        gh["labels"].level,
        devkit_config::FieldLevel::Info,
        "{after}"
    );
    assert_eq!(gh["milestone"].values.len(), 2, "refreshed: {after}");
    assert_eq!(
        gh["milestone"].level,
        devkit_config::FieldLevel::None,
        "kept: {after}"
    );
    assert_eq!(
        gh["estimate"].description.as_deref(),
        Some("story points"),
        "kept: {after}"
    );
    assert!(
        gh["priority"].values.iter().any(|v| v.id == "p0"),
        "{after}"
    );
    assert!(!after.contains("PVTSSF_s"), "the status field: {after}");
    assert!(out.contains("added [ticket.fields.github.labels]"), "{out}");
    assert!(
        out.contains("octo (Octo Cat)") && !out.contains("LevValle"),
        "unmatched users are reported: {out}"
    );
    assert!(!after.contains("octo"), "and not written: {after}");
    assert!(
        !out.contains("---"),
        "an agent's run shows no diff to confirm: {out}"
    );

    let again = stdout(&fake.issue_as("agent", &["fields", "discover"]));
    assert!(again.contains("already declares"), "{again}");
    assert_eq!(devkit_toml(&fake), after);
}

#[test]
fn a_human_without_a_terminal_must_pass_yes() {
    let fake = fields_fake();
    fake.serve_graphql(DISCOVERED);
    let before = devkit_toml(&fake);
    let out = fake.issue_as("human", &["fields", "discover"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--yes"), "{stderr}");
    let shown = String::from_utf8_lossy(&out.stdout);
    assert!(
        shown.contains("+[ticket.fields.github.labels]"),
        "the diff: {shown}"
    );
    assert_eq!(devkit_toml(&fake), before, "nothing written");

    stdout(&fake.issue_as("human", &["fields", "discover", "--yes"]));
    assert!(devkit_toml(&fake).contains("[ticket.fields.github.labels]"));
}

#[test]
fn linear_discover_records_team_scoped_values_for_the_named_team() {
    let fake = ghfake::Fake::without_pr("[tracker]\nkind = \"linear\"\n");
    let linear = linearfake::LinearFake::start();
    linear.answer(
        "issuePriorityValues",
        r#"{ "data": {
          "teams": { "pageInfo": { "hasNextPage": false }, "nodes": [{
            "id": "t-eng", "key": "ENG", "name": "Engineering", "issueEstimationType": "fibonacci",
            "cycles": { "nodes": [{ "id": "cyc-1", "number": 4, "name": "" }] },
            "projects": { "nodes": [] }
          }] },
          "issueLabels": { "pageInfo": { "hasNextPage": false }, "nodes": [
            { "id": "L1", "name": "infra", "isGroup": false, "team": { "key": "ENG" } },
            { "id": "L2", "name": "bug", "isGroup": false, "team": null }
          ] },
          "issuePriorityValues": [{ "priority": 1, "label": "Urgent" }],
          "users": { "pageInfo": { "hasNextPage": false }, "nodes": [] }
        } }"#,
    );
    let mut env = linear.env().to_vec();
    env.push(("DEVKIT_CALLER", "agent"));
    let out = stdout(&fake.issue_with_env(&["fields", "discover", "--team", "ENG"], &env));
    assert!(out.contains("added [ticket.fields.linear.team]"), "{out}");
    let sent = linear.sent("issuePriorityValues");
    let query = sent[0]["query"].as_str().unwrap();
    assert!(query.contains(r#"in: ["ENG"]"#), "{query}");
    let after = std::fs::read_to_string(fake.project().join("devkit.toml")).unwrap();
    let cfg = devkit_config::Config::parse(&after).unwrap();
    let fields = &cfg.ticket.fields.linear;
    let infra = fields["labels"]
        .values
        .iter()
        .find(|v| v.id == "L1")
        .unwrap();
    assert_eq!(infra.teams, ["ENG"], "{after}");
    let bug = fields["labels"]
        .values
        .iter()
        .find(|v| v.id == "L2")
        .unwrap();
    assert!(bug.teams.is_empty(), "{after}");
    assert_eq!(fields["cycle"].values[0].teams, ["ENG"], "{after}");
    assert_eq!(fields["team"].values[0].id, "t-eng");
    assert!(fields.contains_key("estimate"), "{after}");
    assert!(!after.contains("state"), "{after}");
}
