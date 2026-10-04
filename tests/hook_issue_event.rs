//! The SessionStart hook claims the `start` status event in a worktree
//! `issue setup` created and fires it in the background, once per worktree.

#[path = "common/ghfake.rs"]
mod ghfake;

use std::{
    path::Path,
    time::{Duration, Instant},
};

use devkit_common::record::{self, IssueRecord, RecordOrigin};
use devkit_config::IssueEvent;

const START: &str = "[tracker]\nkind = \"github\"\n\
                     [issue.events.start]\nfrom = [\"\", \"Todo\"]\nto = \"In progress\"\n";

const TODO: &str = r#"{"data":{
  "repository":{"issue":{"id":"I_65","projectItems":{"nodes":[
    {"id":"PVTI_3","project":{"id":"PVT_3","number":3},"fieldValueByName":{"name":"Todo","optionId":"opt_todo"}}]}}},
  "repositoryOwner":{"projectV2":{"id":"PVT_3","field":{"id":"F","options":[
    {"id":"opt_todo","name":"Todo"},{"id":"opt_doing","name":"In progress"}]}}}}}"#;

/// A project configured with `config`, `[github] project = 3`, and `rec` as
/// its worktree record.
fn worktree(config: &str, rec: Option<IssueRecord>) -> ghfake::Fake {
    let gh = ghfake::Fake::without_pr(config);
    gh.github_keys("project = 3");
    if let Some(rec) = rec {
        record::write(gh.project(), &rec).unwrap();
    }
    gh.serve_graphql(TODO);
    gh.serve_mutation("{\"data\":{}}");
    gh
}

fn setup_record() -> IssueRecord {
    IssueRecord {
        issue: "65".into(),
        slug: "fix".into(),
        origin: Some(RecordOrigin::Setup),
        events: Some(vec![]),
        ..Default::default()
    }
}

fn session_start(gh: &ghfake::Fake, cwd: &Path) -> std::process::Output {
    let payload = serde_json::json!({
        "session_id": "s-1",
        "transcript_path": "/dev/null",
        "cwd": cwd,
        "hook_event_name": "SessionStart",
        "source": "startup",
    });
    let out = gh.devkit_with_stdin(
        &["hook", "session-start", "--harness", "claude-code"],
        payload.to_string().as_bytes(),
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stdout.is_empty(), "the hook wrote stdout: {out:?}");
    out
}

fn events(gh: &ghfake::Fake) -> Option<Vec<IssueEvent>> {
    record::read(gh.project()).and_then(|r| r.events)
}

fn log(gh: &ghfake::Fake) -> String {
    std::fs::read_to_string(gh.project().join(".devkit").join("issue-event.log"))
        .unwrap_or_default()
}

/// Poll until `done` holds, failing after a generous deadline.
fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn first_session_claims_start_and_runs_the_event() {
    let gh = worktree(START, Some(setup_record()));
    session_start(&gh, gh.project());
    assert_eq!(events(&gh), Some(vec![IssueEvent::Start]));
    wait_for("the background move", || {
        log(&gh).contains("moved 65: Todo -> In progress")
    });
    assert!(
        gh.calls().contains("updateProjectV2ItemFieldValue"),
        "{}",
        gh.calls()
    );
}

#[test]
fn a_later_session_claims_nothing() {
    let rec = IssueRecord {
        events: Some(vec![IssueEvent::Start]),
        ..setup_record()
    };
    let gh = worktree(START, Some(rec));
    let before = std::fs::read_to_string(record::path(gh.project())).unwrap();
    session_start(&gh, gh.project());
    assert_eq!(
        std::fs::read_to_string(record::path(gh.project())).unwrap(),
        before
    );
}

#[test]
fn sessions_starting_together_claim_once() {
    let gh = worktree(START, Some(setup_record()));
    std::thread::scope(|s| {
        let a = s.spawn(|| session_start(&gh, gh.project()));
        let b = s.spawn(|| session_start(&gh, gh.project()));
        a.join().unwrap();
        b.join().unwrap();
    });
    assert_eq!(events(&gh), Some(vec![IssueEvent::Start]));
    wait_for("the background move", || log(&gh).contains("moved 65"));
}

#[test]
fn nothing_is_claimed_without_start_configured() {
    let gh = worktree(
        "[tracker]\nkind = \"github\"\n[issue.events.setup]\nto = \"Todo\"\n",
        Some(setup_record()),
    );
    session_start(&gh, gh.project());
    assert_eq!(events(&gh), Some(vec![]));
}

#[test]
fn nothing_is_claimed_in_a_checkout_worktree() {
    let rec = IssueRecord {
        origin: Some(RecordOrigin::Checkout),
        ..setup_record()
    };
    let gh = worktree(START, Some(rec));
    session_start(&gh, gh.project());
    assert_eq!(events(&gh), Some(vec![]));
}

#[test]
fn nothing_is_claimed_with_tracker_none() {
    let config = START.replace("kind = \"github\"", "kind = \"none\"");
    let gh = worktree(&config, Some(setup_record()));
    session_start(&gh, gh.project());
    assert_eq!(events(&gh), Some(vec![]));
}

#[test]
fn a_legacy_record_carrying_a_pr_claims_nothing() {
    let rec = IssueRecord {
        origin: None,
        events: None,
        pr: Some(devkit_common::forge::PrLocator {
            repo: Some("o/r".into()),
            number: 7,
        }),
        ..setup_record()
    };
    let gh = worktree(START, Some(rec));
    session_start(&gh, gh.project());
    assert_eq!(events(&gh), None);
}

/// `devrun up` gives a hand-made worktree a record named after its branch,
/// with no origin. A branch like `ENG-123` parses as a tracker id, so only
/// the missing origin keeps that worktree from moving an issue.
#[test]
fn a_record_devrun_up_synthesized_claims_nothing() {
    let rec = IssueRecord {
        issue: "ENG-123".into(),
        slug: "ENG-123".into(),
        baseline: Some(record::BaselinePin {
            sha: "abc".into(),
            path: "/b/abc".into(),
        }),
        ..Default::default()
    };
    let gh = worktree(START, Some(rec));
    let before = std::fs::read_to_string(record::path(gh.project())).unwrap();
    session_start(&gh, gh.project());
    assert_eq!(
        std::fs::read_to_string(record::path(gh.project())).unwrap(),
        before
    );
    assert_eq!(events(&gh), None);
}

#[test]
fn a_corrupt_record_is_ignored_silently() {
    let gh = worktree(START, None);
    std::fs::create_dir_all(gh.project().join(".devkit")).unwrap();
    std::fs::write(record::path(gh.project()), "not toml").unwrap();
    let out = session_start(&gh, gh.project());
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(record::path(gh.project())).unwrap(),
        "not toml"
    );
}

#[test]
fn a_failed_background_run_is_logged() {
    let gh = worktree(START, Some(setup_record()));
    gh.serve_graphql(include_str!(
        "../crates/devkit-common/src/tracker/fixtures/github_status_insufficient_scopes.json"
    ));
    session_start(&gh, gh.project());
    wait_for("the logged failure", || {
        log(&gh).contains("lacks the `project` scope")
    });
    assert_eq!(
        events(&gh),
        Some(vec![IssueEvent::Start]),
        "a failed move keeps its claim"
    );
}
