//! `devkit ticket fields`: the declared ticket fields at this caller's level,
//! read from config alone.

#[path = "common/ghfake.rs"]
mod ghfake;

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
