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

/// Writes `file` under the `.devkit/` of the `eng-1-fix` worktree.
fn write_devkit_file(fake: &ghfake::Fake, file: &str, body: &str) {
    let dir = fake.project().join("wts/eng-1-fix/.devkit");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(file), body).unwrap();
}

const CACHED_PR_7: &str =
    r#"{"number":7,"state":"OPEN","url":"https://github.com/o/r/pull/7","is_draft":false}"#;

/// `--cache-only` reads no network, so a PR selector resolves through the PR
/// that a worktree cached on an earlier live run.
#[test]
fn cache_only_info_selects_a_worktree_by_its_cached_pr_number() {
    let fake = two_worktrees();
    write_devkit_file(&fake, "pr.json", CACHED_PR_7);

    let info = stdout(&fake.issue(&["info", "#7", "--cache-only", "--json"]));

    let info: serde_json::Value = serde_json::from_str(&info).unwrap();
    assert_eq!(info["branch"], "lev/eng-1-fix", "info: {info}");
}

/// A record that binds another PR discards the cache, so the cached number no
/// longer selects the worktree, and the recorded one does.
#[test]
fn cache_only_info_selects_by_the_recorded_pr_over_a_stale_cache() {
    let fake = two_worktrees();
    write_devkit_file(&fake, "pr.json", CACHED_PR_7);
    write_devkit_file(
        &fake,
        "ticket.toml",
        "issue = \"ENG-1\"\nslug = \"fix\"\napps = []\n\n[pr]\nnumber = 9\n",
    );

    let stale = fake.issue(&["info", "#7", "--cache-only", "--json"]);
    let recorded = stdout(&fake.issue(&["info", "#9", "--cache-only", "--json"]));

    assert!(
        !stale.status.success(),
        "the stale cached PR selects nothing"
    );
    let recorded: serde_json::Value = serde_json::from_str(&recorded).unwrap();
    assert_eq!(recorded["branch"], "lev/eng-1-fix", "info: {recorded}");
}
