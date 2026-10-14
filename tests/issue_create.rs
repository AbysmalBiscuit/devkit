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
fn a_title_only_edit_asks_only_for_what_the_title_reads() {
    let fake = fake("github");
    let out = fake.issue(&["edit", "7", "--title", "T"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let calls = fake.calls();
    assert!(calls.contains("issue edit 7 --title T"), "{calls}");
    assert!(!calls.contains("--body"), "{calls}");
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

/// A project declaring [`FIELDS`] on `[github] project` 1.
fn fields_fake() -> ghfake::Fake {
    let fake = ghfake::Fake::without_pr(FIELDS);
    fake.github_keys("project = 1");
    fake
}

fn json_body(out: &std::process::Output) -> String {
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("render prints JSON");
    v["body"].as_str().expect("a body").to_string()
}

#[test]
fn a_field_the_body_reads_is_not_required_by_the_template() {
    let fake = fields_fake();
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
    let fake = fields_fake();
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
    let fake = fields_fake();
    let out = fake.issue_as("agent", &["render", "--title", "T", "--arg", "nope=1"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("nope"), "{}", stderr(&out));
}

#[test]
fn none_on_an_undeclared_field_is_refused() {
    let fake = fields_fake();
    let out = fake.issue_as("agent", &["create", "--title", "T", "--none", "nope"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--none nope"), "{}", stderr(&out));
    assert!(!fake.calls().contains("issue create"), "{}", fake.calls());
}

const WRITES: &str = r#"
[tracker]
kind = "github"

[ticket.fields.github.labels]
type   = "enum"
list   = true
where  = "labels"
values = [{ name = "bug", id = "bug" }, { name = "Enhancement", id = "enhancement" }, { name = "a", id = "a" }, { name = "b", id = "b" }]

[ticket.fields.github.assignee]
type  = "user"
where = "assignees"

[ticket.fields.github.milestone]
type   = "enum"
where  = "milestone"
values = [{ name = "v1", id = "v1.0" }]

[ticket.fields.github.priority]
type       = "enum"
where      = { project_field = "PVTSSF_p", name = "Priority" }
values     = [{ name = "P0", id = "opt_p0" }, { name = "P1", id = "opt_p1" }]
level      = "error"
allow_none = false

[ticket.fields.github.estimate]
type  = "number"
where = { project_field = "PVTF_e", name = "Estimate" }

[ticket.fields.github.type]
type   = "enum"
where  = "issue_type"
values = [{ name = "Bug", id = "IT_bug" }]

[ticket.fields.github.parent]
type  = "issue"
where = "parent"

[ticket.fields.github.blocked_by]
type  = "issue"
list  = true
where = "blocked_by"
"#;

fn writes_fake() -> ghfake::Fake {
    let fake = ghfake::Fake::without_pr(WRITES);
    fake.github_keys("project = 3");
    fake
}

/// A read of issue `n` as GitHub answers it: `issue` holds the issue's own
/// fields, and `items` its project items.
fn issue_read(n: u64, issue: serde_json::Value, items: serde_json::Value) -> String {
    let mut i = serde_json::json!({
        "id": format!("I_{n}"),
        "labels": { "nodes": [] },
        "assignees": { "nodes": [] },
        "milestone": null,
        "issueType": null,
        "parent": null,
        "blockedBy": { "nodes": [] },
        "projectItems": { "nodes": items },
    });
    for (k, v) in issue.as_object().unwrap() {
        i[k] = v.clone();
    }
    serde_json::json!({ "data": {
        "repository": {
            "issue": i,
            "ref12": { "id": "I_12" },
            "ref13": { "id": "I_13" },
            "ref14": { "id": "I_14" },
        },
        "repositoryOwner": { "projectV2": { "id": "PVT_3" } },
    }})
    .to_string()
}

fn item(values: serde_json::Value) -> serde_json::Value {
    serde_json::json!([{
        "id": "PVTI_1",
        "project": { "id": "PVT_3", "number": 3 },
        "fieldValues": { "nodes": values },
    }])
}

const MUTATED: &str = r#"{"data":{
  "addProjectV2ItemById":{"item":{"id":"PVTI_1"}},
  "updateProjectV2ItemFieldValue":{"projectV2Item":{"id":"PVTI_1"}},
  "clearProjectV2ItemFieldValue":{"projectV2Item":{"id":"PVTI_1"}},
  "updateIssueIssueType":{"issue":{"id":"I_42"}},
  "addSubIssue":{"issue":{"id":"I_12"}},
  "addBlockedBy":{"issue":{"id":"I_42"}}
}}"#;

fn after_create() -> String {
    issue_read(
        42,
        serde_json::json!({
            "labels": { "nodes": [{ "name": "bug" }, { "name": "enhancement" }] },
            "assignees": { "nodes": [{ "login": "LevValle" }] },
            "milestone": { "title": "v1.0" },
            "issueType": { "id": "IT_bug", "name": "Bug" },
            "parent": { "id": "I_12", "number": 12 },
            "blockedBy": { "nodes": [{ "id": "I_13", "number": 13 }, { "id": "I_14", "number": 14 }] },
        }),
        item(serde_json::json!([
            { "optionId": "opt_p1", "field": { "id": "PVTSSF_p" } },
            { "number": 3.0, "field": { "id": "PVTF_e" } },
        ])),
    )
}

const CREATE_ALL: &[&str] = &[
    "create",
    "--title",
    "T",
    "--arg",
    "labels=bug, enhancement",
    "--arg",
    "assignee=lev",
    "--arg",
    "milestone=v1",
    "--arg",
    "priority=p1",
    "--arg",
    "estimate=3",
    "--arg",
    "type=bug",
    "--arg",
    "parent=12",
    "--arg",
    "blocked_by=13,#14",
];

#[test]
fn create_writes_every_field_and_reads_it_back() {
    let fake = writes_fake();
    fake.serve_graphql_nth(
        1,
        &issue_read(42, serde_json::json!({}), serde_json::json!([])),
    );
    fake.serve_graphql_nth(2, &after_create());
    fake.serve_mutation(MUTATED);
    let out = fake.issue_as("agent", CREATE_ALL);
    assert!(out.status.success(), "{}", stderr(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("https://github.com/o/r/issues/42"),
        "{stdout}"
    );
    let calls = fake.calls();
    for flag in [
        "--label bug --label enhancement",
        "--assignee LevValle",
        "--milestone v1.0",
    ] {
        assert!(calls.contains(flag), "{flag}: {calls}");
    }
    for written in [
        "addProjectV2ItemById(input: {projectId: \"PVT_3\", contentId: \"I_42\"})",
        "singleSelectOptionId: \"opt_p1\"",
        "fieldId: \"PVTF_e\", value: {number: 3}",
        "updateIssueIssueType(input: {issueId: \"I_42\", issueTypeId: \"IT_bug\"})",
        "addSubIssue(input: {issueId: \"I_12\", subIssueId: \"I_42\", replaceParent: true})",
        "addBlockedBy(input: {issueId: \"I_42\", blockingIssueId: \"I_13\"})",
        "addBlockedBy(input: {issueId: \"I_42\", blockingIssueId: \"I_14\"})",
    ] {
        assert!(calls.contains(written), "missing {written}: {calls}");
    }
    assert_eq!(
        calls.matches("api graphql").count() - calls.matches("query=mutation").count(),
        2,
        "one read before the writes, one after: {calls}"
    );
}

#[test]
fn a_missing_error_field_refuses_before_gh_runs() {
    let fake = writes_fake();
    let out = fake.issue_as("agent", &["create", "--title", "T"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("priority") && err.contains("P0, P1"), "{err}");
    assert!(!fake.calls().contains("issue create"), "{}", fake.calls());
}

#[test]
fn a_failed_project_add_prints_the_url_and_the_field() {
    let fake = writes_fake();
    fake.fail_graphql(
        "gh: Your token has not been granted the required scopes to execute this query \
         (INSUFFICIENT_SCOPES)",
    );
    let out = fake.issue_as("agent", &[
        "create",
        "--title",
        "T",
        "--arg",
        "labels=bug",
        "--arg",
        "priority=P1",
    ]);
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("https://github.com/o/r/issues/42"),
        "{stdout}"
    );
    let err = stderr(&out);
    assert!(
        err.contains("priority")
            && err.contains("gh auth refresh -s project")
            && err.contains("ticket edit 42"),
        "{err}"
    );
    assert_eq!(fake.calls().matches("issue create").count(), 1);
}

#[test]
fn an_ambiguous_create_failure_says_to_search_before_retrying() {
    let fake = writes_fake();
    fake.create_fails("Post \"https://api.github.com/graphql\": net/http: timeout");
    let out = fake.issue_as("agent", &[
        "create",
        "--title",
        "Fix it",
        "--arg",
        "priority=P1",
    ]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("may have been created") && err.contains("Fix it"),
        "{err}"
    );
}

#[test]
fn a_read_back_mismatch_names_the_field() {
    let fake = writes_fake();
    fake.serve_graphql_nth(
        1,
        &issue_read(42, serde_json::json!({}), serde_json::json!([])),
    );
    fake.serve_graphql_nth(
        2,
        &issue_read(
            42,
            serde_json::json!({}),
            item(serde_json::json!([{ "optionId": "opt_p0", "field": { "id": "PVTSSF_p" } }])),
        ),
    );
    fake.serve_mutation(MUTATED);
    let out = fake.issue_as("agent", &["create", "--title", "T", "--arg", "priority=P1"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("priority") && err.contains("P0"), "{err}");
}

#[test]
fn a_rejected_id_names_the_field_and_its_declaration() {
    let fake = writes_fake();
    fake.serve_graphql(&issue_read(
        42,
        serde_json::json!({}),
        serde_json::json!([]),
    ));
    fake.serve_mutation(MUTATED);
    fake.serve_mutation_nth(
        2,
        r#"{"data":{"updateProjectV2ItemFieldValue":null},
            "errors":[{"type":"NOT_FOUND","message":"Could not resolve to a node with the global id of 'opt_p1'"}]}"#,
    );
    let out = fake.issue_as("agent", &["create", "--title", "T", "--arg", "priority=P1"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("priority")
            && err.contains("P1")
            && err.contains("[ticket.fields.github.priority]"),
        "{err}"
    );
    assert_eq!(
        fake.calls()
            .matches("updateProjectV2ItemFieldValue")
            .count(),
        1,
        "not retried"
    );
}

#[test]
fn field_only_edit_leaves_title_and_body() {
    let fake = writes_fake();
    fake.serve_graphql_nth(
        1,
        &issue_read(
            7,
            serde_json::json!({}),
            item(serde_json::json!([{ "optionId": "opt_p0", "field": { "id": "PVTSSF_p" } }])),
        ),
    );
    fake.serve_graphql_nth(
        2,
        &issue_read(
            7,
            serde_json::json!({}),
            item(serde_json::json!([{ "optionId": "opt_p1", "field": { "id": "PVTSSF_p" } }])),
        ),
    );
    fake.serve_mutation(MUTATED);
    let out = fake.issue_as("agent", &["edit", "7", "--arg", "priority=P1"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let calls = fake.calls();
    assert!(!calls.contains("issue edit"), "{calls}");
    assert!(
        calls.contains(
            "itemId: \"PVTI_1\", fieldId: \"PVTSSF_p\", value: {singleSelectOptionId: \"opt_p1\"}"
        ),
        "{calls}"
    );
    assert!(
        !calls.contains("addProjectV2ItemById"),
        "already an item: {calls}"
    );
}

#[test]
fn edit_lists_replace() {
    let fake = writes_fake();
    let labels = |names: &[&str]| {
        let nodes: Vec<serde_json::Value> = names
            .iter()
            .map(|n| serde_json::json!({ "name": n }))
            .collect();
        issue_read(
            7,
            serde_json::json!({ "labels": { "nodes": nodes } }),
            serde_json::json!([]),
        )
    };
    fake.serve_graphql_nth(1, &labels(&["a", "b"]));
    fake.serve_graphql_nth(2, &labels(&["a"]));
    let out = fake.issue_as("agent", &["edit", "7", "--arg", "labels=a"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let calls = fake.calls();
    assert!(calls.contains("issue edit 7 --remove-label b"), "{calls}");
    assert!(!calls.contains("--add-label"), "{calls}");
}

#[test]
fn edit_lists_compare_labels_case_insensitively() {
    let fake = writes_fake();
    let labels = issue_read(
        7,
        serde_json::json!({ "labels": { "nodes": [{ "name": "Bug" }, { "name": "old" }] } }),
        serde_json::json!([]),
    );
    fake.serve_graphql_nth(1, &labels);
    fake.serve_graphql_nth(
        2,
        &issue_read(
            7,
            serde_json::json!({ "labels": { "nodes": [{ "name": "Bug" }] } }),
            serde_json::json!([]),
        ),
    );
    let out = fake.issue_as("agent", &["edit", "7", "--arg", "labels=bug"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let calls = fake.calls();
    assert!(calls.contains("issue edit 7 --remove-label old"), "{calls}");
    assert!(
        !calls.contains("--add-label") && !calls.contains("--remove-label Bug"),
        "{calls}"
    );
}

#[test]
fn edit_none_clears_and_refuses_when_not_allowed() {
    let fake = writes_fake();
    let with_milestone = issue_read(
        7,
        serde_json::json!({ "milestone": { "title": "v1.0" } }),
        item(serde_json::json!([{ "optionId": "opt_p0", "field": { "id": "PVTSSF_p" } }])),
    );
    fake.serve_graphql_nth(1, &with_milestone);
    fake.serve_graphql_nth(
        2,
        &issue_read(
            7,
            serde_json::json!({}),
            item(serde_json::json!([{ "optionId": "opt_p0", "field": { "id": "PVTSSF_p" } }])),
        ),
    );
    let out = fake.issue_as("agent", &["edit", "7", "--none", "milestone"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        fake.calls().contains("issue edit 7 --remove-milestone"),
        "{}",
        fake.calls()
    );

    let fake = writes_fake();
    fake.serve_graphql(&with_milestone);
    let out = fake.issue_as("agent", &["edit", "7", "--none", "priority"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("priority") && err.contains("allow_none"),
        "{err}"
    );
    let calls = fake.calls();
    assert!(
        !calls.contains("issue edit") && !calls.contains("query=mutation"),
        "nothing written: {calls}"
    );
}

#[test]
fn edit_sees_a_current_assignee_as_its_people_alias() {
    let fake = ghfake::Fake::without_pr(
        r#"
[tracker]
kind = "github"

[templates]
ticket_body = "{{ input }}\nOwner: {{ assignee }}\n"

[ticket.fields.github.assignee]
type  = "user"
where = "assignees"

[ticket.fields.github.milestone]
type  = "string"
where = "milestone"
when  = [{ if = { assignee = "lev" }, level = "warning" }]
"#,
    );
    fake.serve_issue("the old body");
    fake.serve_graphql(&issue_read(
        7,
        serde_json::json!({ "assignees": { "nodes": [{ "login": "LevValle" }] } }),
        serde_json::json!([]),
    ));
    let out = fake.issue_as("agent", &["edit", "7", "--body", "B", "--none", "milestone"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        fake.calls().contains("issue edit 7 --body B\nOwner: lev\n"),
        "{}",
        fake.calls()
    );
    let err = stderr(&out);
    assert!(err.contains("warning") && err.contains("milestone"), "{err}");
}

#[test]
fn a_text_only_edit_needs_no_field_read() {
    let fake = writes_fake();
    fake.serve_issue("the old body");
    fake.refuse_graphql();
    let out = fake.issue_as("agent", &["edit", "7", "--body", "B"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let calls = fake.calls();
    assert!(calls.contains("issue edit 7 --body B"), "{calls}");
    assert!(!calls.contains("api graphql"), "{calls}");
}

#[test]
fn edit_with_nothing_to_write_is_refused() {
    let fake = writes_fake();
    let out = fake.issue_as("agent", &["edit", "7"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--body"), "{}", stderr(&out));
}

#[test]
fn without_a_kind_only_the_detected_trackers_table_is_checked() {
    let fake = ghfake::Fake::with_origin_and(
        "[ticket.fields.linear.team]\nlevel = \"error\"\n",
        "https://github.com/o/r.git",
    );
    let out = fake.issue(&["create", "--title", "T"]);
    assert!(out.status.success(), "{}", stderr(&out));

    let fake = ghfake::Fake::with_origin_and(
        "[ticket.fields.github.priority]\nlevel = \"error\"\n",
        "https://github.com/o/r.git",
    );
    let out = fake.issue(&["create", "--title", "T"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("[ticket.fields.github.priority]") && err.contains("`type` and `where`"),
        "{err}"
    );
    assert!(!fake.calls().contains("issue create"), "{}", fake.calls());
}
