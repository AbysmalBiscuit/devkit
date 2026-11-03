//! `workspace status` and `workspace end` take one set of selectors: an issue
//! id, a branch, a worktree path, or a PR number.

#[path = "common/ghfake.rs"]
mod ghfake;

/// A project whose PR #7 is open from `lev/eng-1-fix`, that branch checked
/// out in a worktree, and a second worktree on `lev/eng-2-x` with no PR.
fn two_worktrees() -> ghfake::Fake {
    let fake = ghfake::Fake::new("", &ghfake::Pr {
        number: 7,
        state: "OPEN",
        is_draft: false,
        author: "LevValle",
    });
    let git = |args: &[&str]| {
        devkit_git::Git::fixture(fake.project())
            .args(args.iter().copied())
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
    };
    let wts = fake.project().join("wts");
    git(&["checkout", "-q", "main"]);
    git(&[
        "worktree",
        "add",
        "-q",
        wts.join("eng-1-fix").to_str().unwrap(),
        "lev/eng-1-fix",
    ]);
    git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "lev/eng-2-x",
        wts.join("eng-2-x").to_str().unwrap(),
    ]);
    let pr7 = serde_json::json!({
        "number": 7, "state": "OPEN", "url": "https://github.com/o/r/pull/7",
        "title": "t", "headRefName": "lev/eng-1-fix", "headRefOid": fake.head(),
        "isDraft": false, "author": { "login": "LevValle" },
        "headRepositoryOwner": { "login": "o" }
    });
    fake.serve_graphql(
        &serde_json::json!({ "data": { "repository": {
            "b0": { "totalCount": 1, "nodes": [pr7] },
            "b1": { "totalCount": 0, "nodes": [] }
        } } })
        .to_string(),
    );
    fake
}

fn stdout(out: &std::process::Output) -> String {
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn status_and_info_select_one_worktree_by_its_pr_number() {
    let fake = two_worktrees();

    let status = stdout(&fake.issue(&["status", "#7"]));
    let info = stdout(&fake.issue(&["info", "#7", "--json"]));

    assert!(status.contains("lev/eng-1-fix"), "status: {status}");
    assert!(!status.contains("lev/eng-2-x"), "status: {status}");
    let info: serde_json::Value = serde_json::from_str(&info).unwrap();
    assert_eq!(info["branch"], "lev/eng-1-fix", "info: {info}");
}
