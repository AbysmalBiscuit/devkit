//! `issue pr ready` gates the flip, not the run.
//!
//! The reviewer gate exists to keep a PR from being *made* ready with nobody to
//! look at it. A PR that is already ready is not made ready by this run, and a
//! run whose own `--to` already names a human reviewer cannot learn anything
//! from the PR's current list.

#[path = "common/ghfake.rs"]
mod ghfake;

const REQUIRE_REVIEWER: &str = "require_pr_reviewer = true";

#[test]
fn an_already_ready_pr_is_not_judged_by_the_gate() {
    let fake = ghfake::Fake::new(
        REQUIRE_REVIEWER,
        &ghfake::Pr {
            number: 1,
            state: "OPEN",
            is_draft: false,
            author: "someone-else",
        },
    );
    // A bot reviewer never satisfies the gate, so a gate that fired on this run
    // would refuse it after having added that reviewer.
    let out = fake.issue(&["pr", "ready", "--no-push", "--to", "bot"]);

    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "an already-ready PR must exit zero: {stderr}"
    );
    assert!(
        stderr.contains("already ready for review"),
        "the run must say it changed nothing: {stderr}"
    );

    let calls = fake.calls();
    assert!(
        calls.contains("pr edit"),
        "the requested reviewer is still added: {calls}"
    );
    assert!(
        !calls.contains("pr ready"),
        "an already-ready PR must not be flipped: {calls}"
    );
}

#[test]
fn a_human_in_to_settles_the_gate_without_a_lookup() {
    let fake = ghfake::Fake::new(
        REQUIRE_REVIEWER,
        &ghfake::Pr {
            number: 1,
            state: "OPEN",
            is_draft: true,
            author: "someone-else",
        },
    );
    let out = fake.issue(&["pr", "ready", "--no-push", "--to", "lev"]);

    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "the flip must go through: {stderr}");

    let calls = fake.calls();
    assert!(calls.contains("pr ready"), "the draft is flipped: {calls}");
    assert!(
        !calls.contains("reviewRequests"),
        "a human in --to already satisfies the gate, so the PR's own reviewers \
         are never fetched: {calls}"
    );
}

/// The gate still fires on the run that makes a draft ready with nobody to
/// review it, and it fires before the flip.
#[test]
fn a_draft_with_no_human_reviewer_is_refused_before_the_flip() {
    let fake = ghfake::Fake::new(
        REQUIRE_REVIEWER,
        &ghfake::Pr {
            number: 1,
            state: "OPEN",
            is_draft: true,
            author: "someone-else",
        },
    );
    let out = fake.issue(&["pr", "ready", "--no-push", "--to", "bot"]);

    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "the gate must refuse: {stderr}");
    assert!(
        stderr.contains("no human reviewer"),
        "the refusal must say why: {stderr}"
    );

    let calls = fake.calls();
    assert!(
        !calls.contains("pr ready"),
        "a refused run leaves the PR a draft: {calls}"
    );
}

/// The hole the gate was built to close, and the one it left open: GitHub
/// records a review a PR's own author submits like anyone else's, so counting
/// it let `require_pr_reviewer` pass with nobody but the author having looked.
#[test]
fn a_self_review_does_not_satisfy_the_gate() {
    let fake = ghfake::Fake::new(
        REQUIRE_REVIEWER,
        &ghfake::Pr {
            number: 1,
            state: "OPEN",
            is_draft: true,
            author: "LevValle",
        },
    );
    fake.set_reviews(r#"{"reviews":[{"author":{"login":"LevValle"},"state":"COMMENTED"}]}"#);
    let out = fake.issue(&["pr", "ready", "--no-push"]);

    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !out.status.success(),
        "a PR reviewed only by its author must be refused: {stderr}"
    );
    assert!(
        stderr.contains("no human reviewer"),
        "the refusal must name the gate: {stderr}"
    );
    assert!(
        !fake.calls().contains("pr ready"),
        "a refused run leaves the draft a draft: {}",
        fake.calls()
    );
}

/// Someone else's review is still a review, whatever kind it is: GitHub drops a
/// reviewer from `reviewRequests` the moment they submit, so a PR that has been
/// looked at would otherwise count nobody.
#[test]
fn another_persons_review_satisfies_the_gate() {
    let fake = ghfake::Fake::new(
        REQUIRE_REVIEWER,
        &ghfake::Pr {
            number: 1,
            state: "OPEN",
            is_draft: true,
            author: "LevValle",
        },
    );
    fake.set_reviews(r#"{"reviews":[{"author":{"login":"igoracc"},"state":"COMMENTED"}]}"#);
    let out = fake.issue(&["pr", "ready", "--no-push"]);

    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "must be allowed through: {stderr}");
    assert!(
        fake.calls().contains("pr ready"),
        "the PR is flipped: {}",
        fake.calls()
    );
}
