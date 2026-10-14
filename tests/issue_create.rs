//! `devkit ticket create` and `devkit ticket edit` render the issue templates
//! and write the result through `gh issue create` and `gh issue edit`, on
//! GitHub only.

#[path = "common/ghfake.rs"]
mod ghfake;

const TEMPLATES: &str = r#"
[templates]
issue_body = """
{{ input }}
Title: {{ issue_title }}

## Acceptance criteria
{{ acceptance }}
"""

[templates.variables]
acceptance = { required = "agents", description = "observable outcomes that mean the issue is done" }
"#;

fn fake(kind: &str) -> ghfake::Fake {
    ghfake::Fake::without_pr(&format!("[tracker]\nkind = \"{kind}\"\n{TEMPLATES}"))
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_missing_required_arg_is_refused_before_gh_runs() {
    let fake = fake("github");
    let out = fake.issue(&["create", "--title", "T"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("acceptance"), "{}", stderr(&out));
    assert!(
        !fake.calls().contains("issue create"),
        "gh must not run: {}",
        fake.calls()
    );
}

#[test]
fn create_passes_the_rendered_text_to_gh() {
    let fake = fake("github");
    let out = fake.issue(&["create", "--title", "T", "--arg", "acceptance=A"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("https://github.com/o/r/issues/42"),
        "{stdout}"
    );
    let calls = fake.calls();
    assert!(calls.contains("issue create --title T --body"), "{calls}");
    assert!(calls.contains("## Acceptance criteria"), "{calls}");
}

#[test]
fn a_linear_tracker_is_pointed_at_render() {
    let fake = fake("linear");
    let out = fake.issue(&["create", "--title", "T", "--arg", "acceptance=A"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("devkit ticket render") && !stderr(&out).contains("issue render"),
        "{}",
        stderr(&out)
    );
    assert!(!fake.calls().contains("issue create"), "{}", fake.calls());
}

#[test]
fn edit_replaces_the_body_with_the_rendered_template() {
    let fake = fake("github");
    fake.serve_issue("the old body");
    let out = fake.issue(&["edit", "7", "--body", "B", "--arg", "acceptance=A"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let calls = fake.calls();
    assert!(
        calls.contains("issue edit 7 --body B\nTitle: An issue\n\n## Acceptance criteria\nA"),
        "{calls}"
    );
    assert!(!calls.contains("--title"), "the title is kept: {calls}");
}

#[test]
fn edit_with_a_title_replaces_it_with_the_rendered_title() {
    let fake = fake("github");
    let out = fake.issue(&[
        "edit",
        "7",
        "--title",
        "T",
        "--body",
        "B",
        "--arg",
        "acceptance=A",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let calls = fake.calls();
    assert!(
        calls.contains("issue edit 7 --title T --body B\nTitle: T\n"),
        "{calls}"
    );
}

#[test]
fn edit_refuses_a_missing_required_arg_before_gh_runs() {
    let fake = fake("github");
    fake.serve_issue("the old body");
    let out = fake.issue(&["edit", "7", "--body", "B"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("acceptance"), "{}", stderr(&out));
    assert!(!fake.calls().contains("issue edit"), "{}", fake.calls());
}

#[test]
fn edit_under_a_linear_tracker_is_pointed_at_render() {
    let fake = fake("linear");
    let out = fake.issue(&["edit", "7", "--body", "B", "--arg", "acceptance=A"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("devkit ticket render") && !stderr(&out).contains("issue render"),
        "{}",
        stderr(&out)
    );
    assert!(!fake.calls().contains("issue edit"), "{}", fake.calls());
}

const FIELDS: &str = r#"
[tracker]
kind = "github"

[templates]
ticket_body = """
{{ input }}
Priority: {{ priority }}
"""

[ticket.fields.github.priority]
type   = "enum"
where  = { project_field = "PVTSSF_p", name = "Priority" }
values = [{ name = "P0", id = "opt_p0" }, { name = "P1", id = "opt_p1" }]
level  = "warning"
"#;

fn json_body(out: &std::process::Output) -> String {
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("render prints JSON");
    v["body"].as_str().expect("a body").to_string()
}

#[test]
fn a_field_the_body_reads_is_not_required_by_the_template() {
    let fake = ghfake::Fake::without_pr(FIELDS);
    let out = fake.issue_as("agent", &["render", "--title", "T"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        json_body(&out).contains("Priority: \n"),
        "{}",
        json_body(&out)
    );
}

#[test]
fn a_given_field_reaches_the_body() {
    let fake = ghfake::Fake::without_pr(FIELDS);
    let out = fake.issue_as("agent", &["render", "--title", "T", "--arg", "priority=p1"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        json_body(&out).contains("Priority: P1\n"),
        "{}",
        json_body(&out)
    );
}

#[test]
fn an_undeclared_arg_is_still_refused() {
    let fake = ghfake::Fake::without_pr(FIELDS);
    let out = fake.issue_as("agent", &["render", "--title", "T", "--arg", "nope=1"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("nope"), "{}", stderr(&out));
}

#[test]
fn none_on_an_undeclared_field_is_refused() {
    let fake = ghfake::Fake::without_pr(FIELDS);
    let out = fake.issue_as("agent", &["create", "--title", "T", "--none", "nope"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--none nope"), "{}", stderr(&out));
    assert!(!fake.calls().contains("issue create"), "{}", fake.calls());
}
