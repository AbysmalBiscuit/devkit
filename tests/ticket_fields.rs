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
fn github_discover_reads_every_page_of_a_list() {
    let fake = fields_fake();
    fake.serve_graphql(DISCOVERED);
    fake.serve_graphql_nth(
        1,
        &DISCOVERED.replace(
            r#""labels": { "pageInfo": { "hasNextPage": false }"#,
            r#""labels": { "pageInfo": { "hasNextPage": true, "endCursor": "after-enhancement" }"#,
        ),
    );
    fake.serve_graphql_nth(
        2,
        r#"{ "data": { "repository": { "labels": {
          "pageInfo": { "hasNextPage": false }, "nodes": [{ "name": "wontfix" }]
        } } } }"#,
    );
    stdout(&fake.issue_as("agent", &["fields", "discover"]));
    let cfg = devkit_config::Config::parse(&devkit_toml(&fake)).unwrap();
    let labels: Vec<&str> = cfg.ticket.fields.github["labels"]
        .values
        .iter()
        .map(|v| v.name.as_str())
        .collect();
    assert_eq!(labels, ["bug", "enhancement", "wontfix"]);
    assert!(
        fake.calls().contains("after-enhancement"),
        "the second page is read after the first's cursor: {}",
        fake.calls()
    );
}

#[test]
fn discover_writes_the_config_devkit_config_names() {
    let fake = fields_fake();
    fake.serve_graphql(DISCOVERED);
    let before = devkit_toml(&fake);
    let named = fake.state().join("named.toml");
    std::fs::write(&named, &before).unwrap();
    stdout(&fake.issue_with_env(&["fields", "discover"], &[
        ("DEVKIT_CALLER", "agent"),
        ("DEVKIT_CONFIG", named.to_str().unwrap()),
    ]));
    let written = std::fs::read_to_string(&named).unwrap();
    assert!(
        written.contains("[ticket.fields.github.labels]"),
        "{written}"
    );
    assert_eq!(devkit_toml(&fake), before, "the project file is untouched");
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

/// One page of `nodes`, as Linear answers a list, with a next page at
/// `next` when given.
fn page(nodes: &str, next: Option<&str>) -> String {
    format!(
        r#"{{ "pageInfo": {{ "hasNextPage": {}, "endCursor": {} }}, "nodes": {nodes} }}"#,
        next.is_some(),
        next.map_or("null".to_string(), |c| format!("\"{c}\""))
    )
}

/// Answer each list `ticket fields discover --team ENG` reads: the team
/// ENG, its upcoming cycles over two pages, `projects`, two labels, a
/// priority, and no milestones or users.
fn serve_eng(linear: &linearfake::LinearFake, projects: &str) {
    linear.answer(
        "teams(first",
        &format!(
            r#"{{ "data": {{ "teams": {} }} }}"#,
            page(
                r#"[{ "id": "t-eng", "key": "ENG", "name": "Engineering", "issueEstimationType": "fibonacci" }]"#,
                None
            )
        ),
    );
    linear.answer(
        r#""after":"cyc-next""#,
        &format!(
            r#"{{ "data": {{ "team": {{ "cycles": {} }} }} }}"#,
            page(r#"[{ "id": "cyc-2", "number": 5, "name": "" }]"#, None)
        ),
    );
    linear.answer(
        "cycles(first",
        &format!(
            r#"{{ "data": {{ "team": {{ "cycles": {} }} }} }}"#,
            page(
                r#"[{ "id": "cyc-1", "number": 4, "name": "" }]"#,
                Some("cyc-next")
            )
        ),
    );
    linear.answer(
        "projects(first",
        &format!(
            r#"{{ "data": {{ "team": {{ "projects": {} }} }} }}"#,
            page(projects, None)
        ),
    );
    linear.answer(
        "issueLabels(first",
        &format!(
            r#"{{ "data": {{ "issueLabels": {} }} }}"#,
            page(
                r#"[
            { "id": "L1", "name": "infra", "isGroup": false, "team": { "key": "ENG" } },
            { "id": "L2", "name": "bug", "isGroup": false, "team": null }
          ]"#,
                None
            )
        ),
    );
    linear.answer(
        "issuePriorityValues",
        r#"{ "data": { "issuePriorityValues": [{ "priority": 1, "label": "Urgent" }] } }"#,
    );
    linear.answer(
        "users(first",
        &format!(r#"{{ "data": {{ "users": {} }} }}"#, page("[]", None)),
    );
    linear.answer(
        "projectMilestones(first",
        &format!(
            r#"{{ "data": {{ "project": {{ "projectMilestones": {} }} }} }}"#,
            page("[]", None)
        ),
    );
}

#[test]
fn linear_discover_records_team_scoped_values_for_the_named_team() {
    let fake = ghfake::Fake::without_pr("[tracker]\nkind = \"linear\"\n");
    let linear = linearfake::LinearFake::start();
    serve_eng(&linear, "[]");
    let mut env = linear.env().to_vec();
    env.push(("DEVKIT_CALLER", "agent"));
    let out = stdout(&fake.issue_with_env(&["fields", "discover", "--team", "ENG"], &env));
    assert!(out.contains("added [ticket.fields.linear.team]"), "{out}");
    let sent = linear.sent("teams(first");
    assert_eq!(sent[0]["variables"]["keys"], serde_json::json!(["ENG"]));
    assert_eq!(
        linear.sent("cycles(first").len(),
        2,
        "the cycles are read page by page"
    );
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
    let cycles: Vec<(&str, &[String])> = fields["cycle"]
        .values
        .iter()
        .map(|v| (v.id.as_str(), v.teams.as_slice()))
        .collect();
    assert_eq!(
        cycles,
        [
            ("cyc-1", &["ENG".to_string()][..]),
            ("cyc-2", &["ENG".to_string()][..])
        ],
        "{after}"
    );
    assert_eq!(fields["team"].values[0].id, "t-eng");
    assert!(fields.contains_key("estimate"), "{after}");
    assert!(!after.contains("state"), "{after}");
}

#[test]
fn linearfake_refuses_a_list_nested_in_a_list_of_full_pages() {
    let nested = "query { teams(first: 250) { nodes { projects(first: 250) { nodes { id } } } } }";
    assert!(linearfake::complexity(nested) > linearfake::MAX_COMPLEXITY);
    let flat = "query($after: String) { teams(first: 250, after: $after) { nodes { id } } }";
    assert!(linearfake::complexity(flat) <= linearfake::MAX_COMPLEXITY);
}

#[test]
fn linear_discover_of_one_team_keeps_what_other_teams_hold() {
    let fake = ghfake::Fake::without_pr(
        "[tracker]\nkind = \"linear\"\n\n[ticket.fields.linear.team]\ntype = \"enum\"\n\
         where = \"team\"\nlevel = \"info\"\nvalues = [{ name = \"ENG\", id = \"t-eng\" }, \
         { name = \"OPS\", id = \"t-ops\" }]\n\n[ticket.fields.linear.project]\n\
         type = \"enum\"\nwhere = \"project\"\nlevel = \"info\"\nvalues = [\
         { name = \"Platform\", id = \"prj-1\", teams = [\"ENG\", \"OPS\"] }, \
         { name = \"Pager\", id = \"prj-2\", teams = [\"OPS\"] }]\n",
    );
    let linear = linearfake::LinearFake::start();
    serve_eng(&linear, r#"[{ "id": "prj-1", "name": "Platform" }]"#);
    let mut env = linear.env().to_vec();
    env.push(("DEVKIT_CALLER", "agent"));
    stdout(&fake.issue_with_env(&["fields", "discover", "--team", "ENG"], &env));
    let after = std::fs::read_to_string(fake.project().join("devkit.toml")).unwrap();
    let cfg = devkit_config::Config::parse(&after).unwrap();
    let values = |field: &str| -> Vec<(String, Vec<String>)> {
        cfg.ticket.fields.linear[field]
            .values
            .iter()
            .map(|v| (v.id.clone(), v.teams.clone()))
            .collect()
    };
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(
        values("team"),
        [("t-eng".into(), vec![]), ("t-ops".into(), vec![])],
        "{after}"
    );
    assert_eq!(
        values("project"),
        [
            ("prj-1".into(), s(&["ENG", "OPS"])),
            ("prj-2".into(), s(&["OPS"]))
        ],
        "{after}"
    );
}

#[test]
fn discover_refuses_a_refresh_the_config_loader_would_refuse() {
    let fake = ghfake::Fake::without_pr(
        "[tracker]\nkind = \"github\"\n\n[ticket.fields.github.milestone]\ntype = \"enum\"\n\
         where = \"milestone\"\nvalues = [{ name = \"v1\", id = \"v1\" }]\nlevel = \"error\"\n",
    );
    fake.github_keys("project = 1");
    fake.serve_graphql(&DISCOVERED.replace(r#"[{ "title": "v1" }, { "title": "v2" }]"#, "[]"));
    let before = devkit_toml(&fake);
    let out = fake.issue_as("agent", &["fields", "discover"]);
    assert!(
        !out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[ticket.fields.github.milestone]") && stderr.contains("no `values`"),
        "the loader's error: {stderr}"
    );
    assert_eq!(devkit_toml(&fake), before, "nothing written");
}

#[test]
fn a_name_several_values_share_is_listed_with_each_id() {
    let fake = ghfake::Fake::without_pr(
        "[tracker]\nkind = \"linear\"\n\n[ticket.fields.linear.cycle]\ntype = \"enum\"\n\
         where = \"cycle\"\nlevel = \"warning\"\nvalues = [\
         { name = \"Cycle 3\", id = \"c-eng\", teams = [\"ENG\"] }, \
         { name = \"Cycle 3\", id = \"c-ops\", teams = [\"OPS\"] }, \
         { name = \"Cycle 4\", id = \"c-4\", teams = [\"ENG\"] }]\n",
    );
    let out = stdout(&fake.issue_as("agent", &["fields"]));
    assert!(
        out.contains(
            "values: Cycle 3 (id c-eng, teams ENG), Cycle 3 (id c-ops, teams OPS), Cycle 4\n"
        ),
        "{out}"
    );
}

/// Run `devkit hook pre-tool-use` in `fake`'s project on a Linear MCP
/// `save_issue` call of `input` in session `S1`, returning the denial's
/// reason, or `None` for an allow.
fn linear_mcp_denial(fake: &ghfake::Fake, input: serde_json::Value) -> Option<String> {
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "S1",
        "cwd": fake.project(),
        "tool_name": "mcp__claude_ai_Linear__save_issue",
        "tool_input": input,
    });
    let out = fake.devkit_with_stdin(
        &["hook", "pre-tool-use", "--harness", "claude-code"],
        payload.to_string().as_bytes(),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    (v["hookSpecificOutput"]["permissionDecision"] == "deny").then(|| {
        v["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    })
}

#[test]
fn discovered_linear_labels_pass_the_mcp_gate_by_their_names() {
    let fake = ghfake::Fake::without_pr(
        "[tracker]\nkind = \"linear\"\n\n[harness.issue_tools.linear]\n\
         servers = [\"*linear*\"]\ntools = [\"save_issue\"]\nabsent = [\"id\"]\n\
         title = \"title\"\nbody = \"description\"\ntracker = \"linear\"\n\
         fields = { team = \"team\", labels = [\"labels\", \"addLabels\"] }\n",
    );
    let linear = linearfake::LinearFake::start();
    serve_eng(&linear, "[]");
    let mut env = linear.env().to_vec();
    env.push(("DEVKIT_CALLER", "agent"));
    stdout(&fake.issue_with_env(&["fields", "discover", "--team", "ENG"], &env));
    let rendered = stdout(&fake.issue_in_session("agent", Some("S1"), &[
        "render", "--title", "T", "--body", "B",
    ]));
    let rendered: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    let call = |labels_key: &str, label: &str| {
        serde_json::json!({
            "title": rendered["title"],
            "description": rendered["body"],
            "team": "Engineering",
            labels_key: [label],
        })
    };
    for (key, label) in [
        ("labels", "bug"),
        ("labels", "Infra"),
        ("addLabels", "bug"),
        ("labels", "L2"),
    ] {
        assert_eq!(
            linear_mcp_denial(&fake, call(key, label)),
            None,
            "{key} = [{label}]"
        );
    }
    let reason = linear_mcp_denial(&fake, call("labels", "nope")).expect("denied");
    assert!(reason.contains("`nope`"), "{reason}");
}

#[test]
fn discover_fills_an_error_enum_declared_with_no_values() {
    let fake = ghfake::Fake::without_pr(
        "[tracker]\nkind = \"linear\"\n\n[ticket.fields.linear.cycle]\ntype = \"enum\"\n\
         where = \"cycle\"\nlevel = \"error\"\n",
    );
    let refused = fake.issue_as("agent", &["fields"]);
    assert!(!refused.status.success());
    let err = String::from_utf8_lossy(&refused.stderr);
    assert!(
        err.contains("[ticket.fields.linear.cycle]") && err.contains("ticket fields discover"),
        "the loader names the command that fills it: {err}"
    );

    let linear = linearfake::LinearFake::start();
    serve_eng(&linear, "[]");
    let mut env = linear.env().to_vec();
    env.push(("DEVKIT_CALLER", "agent"));
    let out = stdout(&fake.issue_with_env(&["fields", "discover", "--team", "ENG"], &env));
    assert!(out.contains("refreshed [ticket.fields.linear.cycle]"), "{out}");
    let after = std::fs::read_to_string(fake.project().join("devkit.toml")).unwrap();
    let cfg = devkit_config::Config::parse(&after).unwrap();
    let ids: Vec<&str> = cfg.ticket.fields.linear["cycle"]
        .values
        .iter()
        .map(|v| v.id.as_str())
        .collect();
    assert_eq!(ids, ["cyc-1", "cyc-2"], "{after}");
    stdout(&fake.issue_as("agent", &["fields"]));
}
