//! `issue info`, `issue status` and `issue end` judge a worktree alike when its
//! PR lookup cannot be made. The project declares no forge and its `origin` is
//! a local path, so every PR lookup is unknown with no network involved.

#[path = "common/baselinetest.rs"]
mod baselinetest;

use std::path::{Path, PathBuf};

use baselinetest::{devkit, git, project};

/// A project with a local `origin`, no `[forge]` table and no tracker, and a
/// pushed worktree `eng-1-fix` whose PR cache names a merged PR.
fn worktree_with_cached_merged_pr(root: &Path) -> (PathBuf, PathBuf) {
    let repo = root.join("proj");
    project(&repo);
    let toml = repo.join("devkit.toml");
    let body = std::fs::read_to_string(&toml).unwrap();
    std::fs::write(&toml, format!("{body}[tracker]\nkind = 'none'\n")).unwrap();
    git(&repo, &["commit", "-qam", "no tracker"]);

    let origin = root.join("origin.git");
    git(root, &["init", "-q", "--bare", origin.to_str().unwrap()]);
    git(&repo, &[
        "remote",
        "add",
        "origin",
        origin.to_str().unwrap(),
    ]);

    let wt = root.join("proj_worktrees").join("eng-1-fix");
    git(&repo, &[
        "worktree",
        "add",
        "-q",
        "-b",
        "eng-1-fix",
        wt.to_str().unwrap(),
    ]);
    git(&wt, &["commit", "-q", "--allow-empty", "-m", "the fix"]);
    git(&wt, &["push", "-q", "-u", "origin", "eng-1-fix"]);

    let devkit_dir = wt.join(".devkit");
    std::fs::create_dir_all(&devkit_dir).unwrap();
    std::fs::write(
        devkit_dir.join("pr.json"),
        r#"{"number":7,"state":"MERGED","url":"https://github.com/o/r/pull/7","is_draft":false}"#,
    )
    .unwrap();
    // Without it the cache dirties the tree, and that alone would hold the
    // worktree.
    devkit_common::gitignore::write_self_ignore(&devkit_dir);
    (repo, wt)
}

/// A PR cache is a remembered answer, not a live one. When the live lookup
/// fails, `info` reports the lookup as unknown, as `status` does, rather than
/// calling the worktree finished on the strength of the cache, and `end`
/// removes nothing.
#[test]
fn an_unknown_pr_lookup_holds_the_worktree_in_info_status_and_end() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let (repo, wt) = worktree_with_cached_merged_pr(tmp.path());

    let info = devkit(&repo, &state, &["issue", "info", "eng-1", "--json"]);
    let info: serde_json::Value = serde_json::from_slice(&info.stdout).unwrap_or_else(|e| {
        panic!("{e}: {}", String::from_utf8_lossy(&info.stderr));
    });
    assert_eq!(info["verdict"], "unknown", "{info}");
    assert_eq!(info["pr_state"], "UNKNOWN", "{info}");

    let status = devkit(&repo, &state, &["issue", "status"]);
    let status = String::from_utf8_lossy(&status.stdout);
    assert!(
        status.contains("PR unknown") && !status.contains("FINISHED"),
        "{status}"
    );

    let end = devkit(&repo, &state, &["issue", "end", "--yes", "--no-preserve"]);
    assert!(
        wt.exists(),
        "the worktree was removed: {}",
        String::from_utf8_lossy(&end.stdout)
    );
}
