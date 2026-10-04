//! `devkit issue event` moves an issue's tracker status as `[issue.events]`
//! configures, driven end to end against the fake `gh`.

#[path = "common/ghfake.rs"]
mod ghfake;

const GITHUB: &str = "[tracker]\nkind = \"github\"\n";

/// A `[tracker] kind = "github"` project with `events` configured and
/// `[github] project = 3`, recorded as an `issue setup` worktree for 65.
fn board(events: &str) -> ghfake::Fake {
    let gh = ghfake::Fake::without_pr(&format!("{GITHUB}{events}"));
    gh.github_keys("project = 3");
    gh.record_issue("65");
    gh
}

/// The status query's answer for issue 65 in project 3: its item there holds
/// `current`, or it has no item there when `current` is `None`.
fn status_answer(current: Option<&str>) -> String {
    let item = current.map(|name| {
        serde_json::json!({
            "id": "PVTI_3",
            "project": { "id": "PVT_3", "number": 3 },
            "fieldValueByName": { "name": name, "optionId": "opt" },
        })
    });
    serde_json::json!({ "data": {
        "repository": { "issue": { "id": "I_65", "projectItems": { "nodes": item.into_iter().collect::<Vec<_>>() } } },
        "repositoryOwner": { "projectV2": { "id": "PVT_3", "field": {
            "id": "PVTSSF_status",
            "options": [
                { "id": "opt_todo", "name": "Todo" },
                { "id": "opt_doing", "name": "In progress" },
                { "id": "opt_review", "name": "In review" },
            ],
        } } },
    } })
    .to_string()
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn graphql_calls(gh: &ghfake::Fake) -> Vec<String> {
    gh.calls()
        .lines()
        .filter(|l| l.starts_with("api graphql"))
        .map(String::from)
        .collect()
}

#[test]
fn an_unconfigured_event_does_nothing_and_says_so() {
    let gh = board("");
    let out = gh.issue(&["event", "start"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("[issue.events.start] is not configured"),
        "{}",
        stderr(&out)
    );
    assert!(gh.calls().is_empty(), "no gh call expected: {}", gh.calls());
}

#[test]
fn an_issue_already_at_the_target_is_not_written() {
    let gh = board("[issue.events.start]\nto = \"In progress\"\n");
    gh.serve_graphql(&status_answer(Some("in PROGRESS ")));
    let out = gh.issue(&["event", "start"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("65 is already in PROGRESS"),
        "{}",
        stderr(&out)
    );
    assert_eq!(graphql_calls(&gh).len(), 1, "{}", gh.calls());
}

#[test]
fn an_allowed_status_moves_to_the_target_in_one_read_and_one_write() {
    let gh = board("[issue.events.start]\nfrom = [\"\", \"Todo\"]\nto = \"In progress\"\n");
    gh.serve_graphql(&status_answer(Some("Todo")));
    gh.serve_mutation("{\"data\":{}}");
    let out = gh.issue(&["event", "start"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("moved 65: Todo -> In progress"),
        "{}",
        stderr(&out)
    );
    let calls = graphql_calls(&gh);
    assert_eq!(calls.len(), 2, "{}", gh.calls());
    assert!(
        calls[1].contains("updateProjectV2ItemFieldValue") && calls[1].contains("opt_doing"),
        "{}",
        calls[1]
    );
    assert!(calls[1].contains("PVTI_3"), "{}", calls[1]);
}

#[test]
fn an_issue_outside_the_project_is_added_then_moved() {
    let gh = board("[issue.events.start]\nfrom = [\"\"]\nto = \"In progress\"\n");
    gh.serve_graphql(&status_answer(None));
    gh.serve_mutation("{\"data\":{\"addProjectV2ItemById\":{\"item\":{\"id\":\"PVTI_new\"}}}}");
    let out = gh.issue(&["event", "start", "65"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("moved 65: (none) -> In progress"),
        "{}",
        stderr(&out)
    );
    let calls = graphql_calls(&gh);
    assert_eq!(calls.len(), 3, "{}", gh.calls());
    assert!(
        calls[1].contains("addProjectV2ItemById") && calls[1].contains("I_65"),
        "{}",
        calls[1]
    );
    assert!(calls[2].contains("PVTI_new"), "{}", calls[2]);
}

#[test]
fn a_status_outside_from_is_left_alone() {
    let gh = board("[issue.events.start]\nfrom = [\"Todo\"]\nto = \"In progress\"\n");
    gh.serve_graphql(&status_answer(Some("In review")));
    let out = gh.issue(&["event", "start"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("65 is In review, not in [issue.events.start] from"),
        "{}",
        stderr(&out)
    );
    assert_eq!(graphql_calls(&gh).len(), 1, "{}", gh.calls());
}

#[test]
fn an_unknown_target_fails_listing_the_options() {
    let gh = board("[issue.events.pr_open]\nto = \"Shipping\"\n");
    gh.serve_graphql(&status_answer(Some("Todo")));
    let out = gh.issue(&["event", "pr_open"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("no option `Shipping` in `Status`")
            && stderr(&out).contains("Todo, In progress, In review"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn github_without_a_project_is_an_error_naming_the_key() {
    let gh = ghfake::Fake::without_pr(&format!(
        "{GITHUB}[issue.events.start]\nto = \"In progress\"\n"
    ));
    gh.record_issue("65");
    let out = gh.issue(&["event", "start"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("[github] project"),
        "{}",
        stderr(&out)
    );
    assert!(gh.calls().is_empty(), "{}", gh.calls());
}

#[test]
fn no_tracker_is_an_error_naming_the_key() {
    let gh = ghfake::Fake::without_pr(
        "[tracker]\nkind = \"none\"\n[issue.events.start]\nto = \"In progress\"\n",
    );
    gh.record_issue("65");
    let out = gh.issue(&["event", "start"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("[tracker] kind"), "{}", stderr(&out));
}

#[test]
fn a_worktree_without_an_issue_id_is_an_error() {
    let gh = ghfake::Fake::without_pr(&format!(
        "{GITHUB}[issue.events.start]\nto = \"In progress\"\n"
    ));
    gh.github_keys("project = 3");
    gh.record_issue("UNKNOWN");
    let out = gh.issue(&["event", "start"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("no tracker issue"),
        "{}",
        stderr(&out)
    );
    assert!(gh.calls().is_empty(), "{}", gh.calls());
}
