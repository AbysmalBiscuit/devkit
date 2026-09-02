//! `issue review request` resolves who it will notify before it changes the PR.
//!
//! A run with no `--to` against a PR carrying no reviewers cannot name a
//! recipient and refuses. Flipping the draft first would leave the PR promoted
//! to ready by a run that told nobody and exited non-zero.
#![cfg(unix)]

#[path = "common/ghfake.rs"]
mod ghfake;

#[test]
fn a_refused_request_leaves_the_draft_alone() {
    let fake = ghfake::Fake::new(
        "",
        &ghfake::Pr {
            number: 1,
            state: "OPEN",
            is_draft: true,
        },
    );
    let out = fake.issue(&["review", "request", "--no-push"]);

    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !out.status.success(),
        "a request with no recipient must fail: {stderr}"
    );
    assert!(
        stderr.contains("no reviewers on the PR"),
        "the refusal must name the missing recipients: {stderr}"
    );

    let calls = fake.calls();
    assert!(
        calls.contains("pr view"),
        "the run must have reached target resolution: {calls}"
    );
    assert!(
        !calls.contains("pr ready"),
        "a refused run must not have promoted the draft: {calls}"
    );
}
