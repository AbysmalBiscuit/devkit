//! `issue end` without `--clean-worktree`: the finished gate decides what is
//! removed. The project declares no forge and no tracker, so a worktree whose
//! commits are on a remote is finished with no network involved.

#[path = "common/baselinetest.rs"]
mod baselinetest;

use std::path::{Path, PathBuf};

use baselinetest::{devkit, git, project};

/// A project with a local `origin` and worktree `eng-1-fix`, whose one commit
/// past `main` is pushed.
fn pushed_worktree(root: &Path) -> (PathBuf, PathBuf) {
    let repo = root.join("proj");
    project(&repo);
    let toml = repo.join("devkit.toml");
    let body = std::fs::read_to_string(&toml).unwrap();
    std::fs::write(
        &toml,
        format!("{body}[forge]\nkind = 'none'\n[tracker]\nkind = 'none'\n"),
    )
    .unwrap();
    git(&repo, &["commit", "-qam", "no forge"]);

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
    (repo, wt)
}

fn end(repo: &Path, state: &Path) -> String {
    let out = devkit(repo, state, &["issue", "end", "--yes", "--no-preserve"]);
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

#[test]
fn a_finished_worktree_is_removed_by_the_gate() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let (repo, wt) = pushed_worktree(tmp.path());

    let output = end(&repo, &state);

    assert!(!wt.exists(), "a finished worktree survived: {output}");
}

/// A corrupt record could be hiding the issue the worktree belongs to, or the
/// baseline whose servers its removal must stop. Status and `end` both read it
/// as unknown, never as finished.
#[test]
fn an_unreadable_record_holds_the_worktree_in_status_and_end() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let (repo, wt) = pushed_worktree(tmp.path());
    let devkit_dir = wt.join(".devkit");
    std::fs::create_dir_all(&devkit_dir).unwrap();
    std::fs::write(devkit_dir.join("issue.toml"), "issue = \n").unwrap();
    // Every real record write drops this beside the record. Without it the
    // tree is dirty, and that alone would hold the worktree.
    devkit_common::gitignore::write_self_ignore(&devkit_dir);

    let status = devkit(&repo, &state, &["issue", "status"]);
    let status = String::from_utf8_lossy(&status.stdout);
    assert!(
        status.contains("issue record unreadable") && !status.contains("FINISHED"),
        "{status}"
    );

    let output = end(&repo, &state);

    assert!(wt.exists(), "the worktree was removed: {output}");
    assert!(output.contains("Nothing finished"), "{output}");
}
