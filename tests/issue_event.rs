//! `devkit ticket event` moves an issue's tracker status as `[issue.events]`
//! configures, driven end to end against the fake `gh`.

#[path = "common/ghfake.rs"]
mod ghfake;

const GITHUB: &str = "[tracker]\nkind = \"github\"\n";

/// A `[tracker] kind = "github"` project with `events` configured and
/// `[github] project = 3`, recorded as a `workspace setup` worktree for 65.
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
            && stderr(&out).contains("Todo, In progress, In review")
            && stderr(&out).contains("[issue.events.pr_open] to")
            && !stderr(&out).contains("[github] status_field"),
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

const PR_7: ghfake::Pr = ghfake::Pr {
    number: 7,
    state: "OPEN",
    is_draft: true,
    author: "LevValle",
};

/// A bare `origin` holding the project's commit as `main`, which `workspace
/// setup` and `pr checkout` fetch and branch from.
fn push_origin(gh: &ghfake::Fake) -> tempfile::TempDir {
    let origin = tempfile::tempdir().unwrap();
    devkit_git::Git::fixture(origin.path())
        .args(["init", "-q", "--bare"])
        .output()
        .unwrap();
    let git = || devkit_git::Git::fixture(gh.project());
    git()
        .args(["remote", "add", "origin", origin.path().to_str().unwrap()])
        .output()
        .unwrap();
    git()
        .args(["push", "-q", "origin", "HEAD:main"])
        .output()
        .unwrap();
    origin
}

const ALL_EVENTS: &str = "[issue.events.setup]\nto = \"Todo\"\n\
                          [issue.events.start]\nto = \"In progress\"\n\
                          [issue.events.pr_open]\nto = \"In review\"\n";

#[test]
fn setup_records_and_fires_its_event_and_warns_without_failing() {
    let gh = ghfake::Fake::without_pr(&format!("{GITHUB}[issue.events.setup]\nto = \"Todo\"\n"));
    gh.github_keys("project = 3");
    let _origin = push_origin(&gh);
    gh.serve_graphql(include_str!(
        "../crates/devkit-common/src/tracker/fixtures/github_status_insufficient_scopes.json"
    ));

    let dir = gh.project().to_str().unwrap();
    let out = gh.devkit_here(&[
        "workspace",
        "setup",
        "-C",
        dir,
        "65",
        "--slug",
        "fix",
        "--no-gitignore",
    ]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("warning: ticket event setup")
            && !stderr(&out).contains("issue event")
            && stderr(&out).contains("gh auth refresh -s project"),
        "{}",
        stderr(&out)
    );
    assert_eq!(graphql_calls(&gh).len(), 1, "{}", gh.calls());
    let rec = devkit_common::record::read(&setup_worktree(&out)).expect("setup record");
    assert_eq!(
        (rec.origin, rec.events),
        (
            Some(devkit_common::record::RecordOrigin::Setup),
            Some(vec![devkit_config::IssueEvent::Setup])
        )
    );
}

#[test]
fn setup_without_the_event_claims_nothing_and_reads_no_tracker() {
    let gh = ghfake::Fake::without_pr(&format!(
        "{GITHUB}[issue.events.start]\nto = \"In progress\"\n"
    ));
    gh.github_keys("project = 3");
    let _origin = push_origin(&gh);

    let out = gh.issue(&["setup", "65", "--slug", "fix", "--no-gitignore"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert!(graphql_calls(&gh).is_empty(), "{}", gh.calls());
    let rec = devkit_common::record::read(&setup_worktree(&out)).expect("setup record");
    assert_eq!(rec.events, Some(vec![]));
}

#[test]
fn pr_create_fires_pr_open_when_it_reuses_a_pr() {
    let gh = ghfake::Fake::new(
        &format!("{GITHUB}[issue.events.pr_open]\nto = \"In review\"\n"),
        &PR_7,
    );
    gh.github_keys("project = 3");
    gh.record_issue("65");
    gh.serve_pr(&PR_7);
    gh.serve_graphql(&status_answer(Some("In progress")));
    gh.serve_mutation("{\"data\":{}}");

    let out = gh.issue(&["pr", "create", "--no-push"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("moved 65: In progress -> In review"),
        "{}",
        stderr(&out)
    );
    let rec = devkit_common::record::read(gh.project()).unwrap();
    assert_eq!(rec.events, Some(vec![devkit_config::IssueEvent::PrOpen]));
    assert_eq!(rec.pr.map(|p| p.number), Some(7));

    let again = gh.issue(&["pr", "create", "--no-push"]);
    assert!(again.status.success(), "{}", stderr(&again));
    assert_eq!(
        graphql_calls(&gh).len(),
        2,
        "a second run fires nothing: {}",
        gh.calls()
    );
}

#[test]
fn a_failed_pr_open_warns_and_the_pr_stands() {
    let gh = ghfake::Fake::new(
        &format!("{GITHUB}[issue.events.pr_open]\nto = \"In review\"\n"),
        &PR_7,
    );
    gh.github_keys("project = 3");
    gh.record_issue("65");

    let out = gh.issue(&["pr", "create", "--no-push"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("warning: ticket event pr_open"),
        "{}",
        stderr(&out)
    );
    assert_eq!(
        devkit_common::record::read(gh.project())
            .unwrap()
            .pr
            .map(|p| p.number),
        Some(7)
    );
}

#[test]
fn checkout_records_its_origin_and_fires_nothing() {
    let gh = ghfake::Fake::without_pr(&format!("{GITHUB}{ALL_EVENTS}"));
    gh.github_keys("project = 3");
    gh.serve_pr(&PR_7);
    let _origin = push_origin(&gh);
    let scratch = tempfile::tempdir().unwrap();
    let worktree = scratch.path().join("pr-7");

    let out = gh.issue(&["pr", "checkout", "#7", worktree.to_str().unwrap()]);

    assert!(out.status.success(), "{}", stderr(&out));
    let rec = devkit_common::record::read(&worktree).expect("checkout record");
    assert_eq!(
        (rec.origin, rec.events),
        (
            Some(devkit_common::record::RecordOrigin::Checkout),
            Some(vec![])
        )
    );
    assert!(graphql_calls(&gh).is_empty(), "{}", gh.calls());
}

/// The worktree `workspace setup` reported creating, from its JSON on stdout.
fn setup_worktree(out: &std::process::Output) -> std::path::PathBuf {
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("setup JSON: {e}: {}", String::from_utf8_lossy(&out.stdout)));
    json["worktree"].as_str().expect("worktree key").into()
}

#[test]
fn pr_create_fires_pr_open_when_it_opens_the_pr() {
    let gh = ghfake::Fake::without_pr(&format!(
        "{GITHUB}[issue.events.pr_open]\nto = \"In review\"\n"
    ));
    gh.github_keys("project = 3");
    gh.record_issue("65");
    gh.create_opens(&PR_7);
    gh.serve_graphql(&status_answer(Some("In progress")));
    gh.serve_mutation("{\"data\":{}}");

    let out = gh.issue(&["pr", "create", "--no-push", "--pr-title", "t"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert!(gh.calls().contains("pr create"), "{}", gh.calls());
    assert!(
        stderr(&out).contains("moved 65: In progress -> In review"),
        "{}",
        stderr(&out)
    );
    let rec = devkit_common::record::read(gh.project()).unwrap();
    assert_eq!(rec.events, Some(vec![devkit_config::IssueEvent::PrOpen]));
}
