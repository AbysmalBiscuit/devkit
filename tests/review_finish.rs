//! `issue review finish` finds the PR to report on from the worktree's branch
//! when neither `--pr` nor the record names one.

#[path = "common/ghfake.rs"]
mod ghfake;

#[test]
fn two_open_prs_on_the_branch_are_named_rather_than_missed() {
    let open = |number| ghfake::Pr {
        number,
        state: "OPEN",
        is_draft: false,
        author: "LevValle",
    };
    let fake = ghfake::Fake::with_prs("", &[open(7), open(8)]);

    let out = fake.issue(&["review", "finish"]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains("several PRs share this head branch"),
        "{stderr}"
    );
    assert!(stderr.contains("#7") && stderr.contains("#8"), "{stderr}");
}
