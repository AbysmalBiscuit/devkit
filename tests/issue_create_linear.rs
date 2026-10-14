//! `devkit ticket create` and `devkit ticket edit` on a Linear tracker write
//! the ticket and its declared fields through Linear's GraphQL API.

#[path = "common/ghfake.rs"]
mod ghfake;
#[path = "common/linearfake.rs"]
mod linearfake;

use linearfake::LinearFake;

const FIELDS: &str = r#"
[tracker]
kind = "linear"

[ticket.fields.linear.team]
type    = "enum"
where   = "team"
values  = [{ name = "ENG", id = "team-eng" }, { name = "OPS", id = "team-ops" }]
default = "ENG"

[ticket.fields.linear.labels]
type   = "enum"
list   = true
where  = "labels"
values = [{ name = "bug", id = "lbl-bug" }, { name = "infra", id = "lbl-infra", teams = ["ENG"] }]
level  = "warning"

[ticket.fields.linear.priority]
type   = "enum"
where  = "priority"
values = [{ name = "urgent", id = "1" }, { name = "high", id = "2" }]

[ticket.fields.linear.assignee]
type  = "user"
where = "assignee"

[people.ada]
linear = "user-ada"
"#;

const UUID: &str = "0b7c4a52-9d1e-4f3a-8c6b-2e5d7f9a1c30";

/// A read answering ENG-42 with `labels` (ids) and `priority`.
fn read(labels: &[&str], priority: u8) -> String {
    read_in(("team-eng", "ENG"), 42, labels, priority)
}

/// A read answering the ticket `number` of `team` (id, key).
fn read_in(team: (&str, &str), number: u32, labels: &[&str], priority: u8) -> String {
    let (team_id, key) = team;
    let identifier = format!("{key}-{number}");
    let labels: Vec<_> = labels
        .iter()
        .map(|l| serde_json::json!({ "id": l }))
        .collect();
    serde_json::json!({ "data": { "issue": { "nodes": [{
        "id": UUID,
        "identifier": identifier,
        "url": format!("https://linear.app/acme/issue/{identifier}/t"),
        "title": "Old title",
        "team": { "id": team_id, "key": key },
        "labels": { "nodes": labels },
        "assignee": null,
        "priority": priority,
        "estimate": null,
        "cycle": null,
        "project": null,
        "projectMilestone": null,
        "dueDate": null,
        "parent": null,
        "inverseRelations": { "nodes": [] }
    }] } } })
    .to_string()
}

const CREATED: &str = r#"{"data":{"issueCreate":{"success":true}}}"#;
const UPDATED: &str = r#"{"data":{"issueUpdate":{"success":true}}}"#;
/// Linear's answer, sent with HTTP 400, to a create whose id an issue has.
const ID_TAKEN: &str = r#"{"errors":[{"message":"conflict on insert of Issue","extensions":{"code":"INPUT_ERROR","statusCode":400,"userPresentableMessage":"Entity Issue with id 0b7c4a52-9d1e-4f3a-8c6b-2e5d7f9a1c30 already exists."}}]}"#;

fn project(config: &str) -> ghfake::Fake {
    ghfake::Fake::without_pr(config)
}

fn run(fake: &ghfake::Fake, linear: &LinearFake, args: &[&str]) -> std::process::Output {
    let mut env = linear.env().to_vec();
    env.push(("DEVKIT_CALLER", "agent"));
    fake.issue_with_env(args, &env)
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn create_on_linear_writes_fields_and_prints_the_identifier() {
    let fake = project(FIELDS);
    let linear = LinearFake::start();
    linear.answer("issueCreate(", CREATED);
    linear.answer("issue: issues(", &read(&["lbl-bug"], 2));
    let out = run(&fake, &linear, &[
        "create",
        "--title",
        "T",
        "--body",
        "B",
        "--arg",
        "labels=bug",
        "--arg",
        "priority=High",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("ENG-42"), "{}", stdout(&out));

    let creates = linear.sent("issueCreate(");
    assert_eq!(creates.len(), 1, "{creates:?}");
    let input = &creates[0]["variables"]["input"];
    assert_eq!(input["teamId"], "team-eng", "the team default: {input}");
    assert_eq!(input["labelIds"], serde_json::json!(["lbl-bug"]), "{input}");
    assert_eq!(input["priority"], 2, "{input}");
    assert_eq!(input["title"], "T", "{input}");
    assert!(
        input["id"].as_str().is_some_and(|id| id.len() == 36),
        "a client id: {input}"
    );
    assert!(
        fake.calls().is_empty(),
        "no gh call on Linear: {}",
        fake.calls()
    );
}

#[test]
fn create_without_a_team_field_names_the_missing_declaration() {
    let config = FIELDS.replace("where   = \"team\"", "where   = \"cycle\"");
    let fake = project(&config);
    let linear = LinearFake::start();
    let out = run(&fake, &linear, &["create", "--title", "T"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("where = \"team\"") && err.contains("[ticket.fields.linear."),
        "{err}"
    );
    assert!(linear.bodies().is_empty(), "{:?}", linear.bodies());
}

#[test]
fn a_label_from_another_team_is_refused_before_any_request() {
    let fake = project(FIELDS);
    let linear = LinearFake::start();
    let out = run(&fake, &linear, &[
        "create",
        "--title",
        "T",
        "--arg",
        "team=OPS",
        "--arg",
        "labels=infra",
    ]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("`infra`") && err.contains("ENG") && err.contains("OPS"),
        "{err}"
    );
    assert!(linear.bodies().is_empty(), "{:?}", linear.bodies());
}

#[test]
fn a_retried_create_returns_the_first_ticket() {
    let fake = project(FIELDS);
    let linear = LinearFake::start();
    linear.answer("issueCreate(", CREATED);
    linear.answer("issue: issues(", &read(&[], 0));
    let args = ["create", "--title", "T", "--body", "B"];
    let first = run(&fake, &linear, &args);
    assert!(first.status.success(), "{}", stderr(&first));

    linear.answer_status("issueCreate(", 400, ID_TAKEN);
    let retry = run(&fake, &linear, &args);
    assert!(retry.status.success(), "{}", stderr(&retry));
    assert_eq!(stdout(&retry), stdout(&first), "the same ticket");
    assert!(stderr(&retry).contains("already"), "{}", stderr(&retry));

    let creates = linear.sent("issueCreate(");
    assert_eq!(creates.len(), 2);
    assert_eq!(
        creates[0]["variables"]["input"]["id"], creates[1]["variables"]["input"]["id"],
        "the retry passes the first attempt's id"
    );
}

#[test]
fn a_retried_create_returns_the_first_ticket_after_it_was_archived() {
    let fake = project(FIELDS);
    let linear = LinearFake::start();
    let mut archived: serde_json::Value = serde_json::from_str(&read(&[], 0)).unwrap();
    archived["data"]["issue"]["nodes"][0]["archivedAt"] = "2026-10-01T00:00:00.000Z".into();
    linear.answer("includeArchived: true", &archived.to_string());
    linear.answer("issue: issues(", r#"{"data":{"issue":{"nodes":[]}}}"#);
    linear.answer_status("issueCreate(", 400, ID_TAKEN);
    let out = run(&fake, &linear, &["create", "--title", "T", "--body", "B"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("ENG-42"), "{}", stdout(&out));
    assert!(stderr(&out).contains("archived"), "{}", stderr(&out));
}

#[test]
fn a_refused_create_shows_linears_explanation() {
    let fake = project(FIELDS);
    let linear = LinearFake::start();
    linear.answer(
        "issueCreate(",
        r#"{"data":null,"errors":[{"message":"Argument Validation Error","extensions":{"code":"INVALID_INPUT","userPresentableMessage":"Team ENG is archived."}}]}"#,
    );
    linear.answer("issue: issues(", r#"{"data":{"issue":{"nodes":[]}}}"#);
    let out = run(&fake, &linear, &["create", "--title", "T", "--body", "B"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("Team ENG is archived."), "{err}");
    assert!(
        !err.contains("rerun"),
        "a refusal is not a lost answer: {err}"
    );
}

#[test]
fn a_create_refused_with_a_client_error_status_shows_linears_explanation() {
    let fake = project(FIELDS);
    let linear = LinearFake::start();
    linear.answer_status(
        "issueCreate(",
        400,
        r#"{"errors":[{"message":"Argument Validation Error","extensions":{"code":"INVALID_INPUT","userPresentableMessage":"Title is too long."}}]}"#,
    );
    linear.answer("issue: issues(", r#"{"data":{"issue":{"nodes":[]}}}"#);
    let out = run(&fake, &linear, &["create", "--title", "T", "--body", "B"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("Title is too long."), "{err}");
    assert!(
        !err.contains("rerun"),
        "a refusal is not a lost answer: {err}"
    );
}

#[test]
fn an_unanswered_create_with_no_ticket_says_a_rerun_is_safe() {
    let fake = project(FIELDS);
    let linear = LinearFake::start();
    linear.hang_up("issueCreate(");
    linear.answer("issue: issues(", r#"{"data":{"issue":{"nodes":[]}}}"#);
    let out = run(&fake, &linear, &["create", "--title", "T", "--body", "B"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("rerun the same `ticket create`") && err.contains("at most one ticket"),
        "{err}"
    );
}

#[test]
fn edit_on_linear_reads_first_and_writes_only_named_fields() {
    let fake = project(FIELDS);
    let linear = LinearFake::start();
    linear.answer("issue: issues(", &read(&["lbl-bug"], 2));
    linear.answer("issueUpdate(", UPDATED);
    let out = run(&fake, &linear, &[
        "edit",
        "eng-42",
        "--arg",
        "priority=urgent",
    ]);
    // The fake still reads priority 2 back, so the edit reports it.
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("`priority` = `urgent` reads back as high"),
        "{}",
        stderr(&out)
    );

    let bodies = linear.bodies();
    let first = bodies[0]["query"].as_str().unwrap();
    assert!(
        first.starts_with("query") && first.contains("number: { eq: 42 }"),
        "the read comes first: {first}"
    );
    let updates = linear.sent("issueUpdate(");
    assert_eq!(updates.len(), 1, "{bodies:?}");
    assert_eq!(updates[0]["variables"]["id"], UUID);
    assert_eq!(
        updates[0]["variables"]["input"],
        serde_json::json!({ "priority": 1 }),
        "only the named field, no title, body or labels"
    );

    linear.answer("issue: issues(", &read(&["lbl-bug"], 1));
    let out = run(&fake, &linear, &[
        "edit",
        "ENG-42",
        "--arg",
        "priority=urgent",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("ENG-42"), "{}", stdout(&out));
}

#[test]
fn an_edit_moving_the_ticket_to_another_team_reads_it_back_by_its_id() {
    let fake = project(FIELDS);
    let linear = LinearFake::start();
    linear.answer("id: { eq: ", &read_in(("team-ops", "OPS"), 7, &[], 0));
    linear.answer("number: { eq: 42 }", &read(&[], 0));
    linear.answer("issueUpdate(", UPDATED);
    let out = run(&fake, &linear, &["edit", "ENG-42", "--arg", "team=OPS"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out).trim(),
        "https://linear.app/acme/issue/OPS-7/t",
        "the link the ticket has after the move"
    );
    let updates = linear.sent("issueUpdate(");
    assert_eq!(
        updates[0]["variables"]["input"],
        serde_json::json!({ "teamId": "team-ops" })
    );
}

#[test]
fn edit_on_linear_rewrites_the_body_keeping_the_title() {
    let fake = project(FIELDS);
    let linear = LinearFake::start();
    linear.answer("issue: issues(", &read(&[], 0));
    linear.answer("issueUpdate(", UPDATED);
    let out = run(&fake, &linear, &["edit", "ENG-42", "--body", "New body"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let updates = linear.sent("issueUpdate(");
    assert_eq!(
        updates[0]["variables"]["input"],
        serde_json::json!({ "description": "New body" })
    );
}

#[test]
fn a_linear_create_without_a_key_is_refused_before_any_request() {
    let fake = project(FIELDS);
    let linear = LinearFake::start();
    let out = fake.issue_with_env(&["create", "--title", "T"], &[
        ("DEVKIT_TEST_LINEAR_URL", &linear.url),
        ("LINEAR_API_KEY", ""),
    ]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("LINEAR_API_KEY"), "{}", stderr(&out));
    assert!(linear.bodies().is_empty());
}
