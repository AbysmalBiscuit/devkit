//! `issue end` and `[hooks] before_worktree_remove`.

#[path = "common/baselinetest.rs"]
mod baselinetest;

use std::path::{Path, PathBuf};

use baselinetest::{devkit, git, project};

/// The hook creates this branch from its working directory's `HEAD`, so where
/// the branch points shows which checkout the hook ran in, and that the
/// checkout still existed.
const MARKER: &str = "before-remove-marker";

fn rev(cwd: &Path, rev: &str) -> Option<String> {
    devkit_common::git::Git::fixture(cwd)
        .args(["rev-parse", "--verify", "--quiet", rev])
        .output()
        .ok()
        .map(|s| s.trim().to_string())
}

/// A project whose `devkit.toml` carries `extra`, with worktree `a` holding a
/// commit the primary checkout does not.
fn setup(root: &Path, extra: &str) -> (PathBuf, PathBuf) {
    let repo = root.join("proj");
    project(&repo);
    let toml = repo.join("devkit.toml");
    let body = std::fs::read_to_string(&toml).unwrap();
    std::fs::write(&toml, format!("{body}{extra}")).unwrap();
    git(&repo, &["commit", "-qam", "hooks"]);
    // The triage `issue end` runs first reads the `origin` remote; the URL
    // itself is never fetched here.
    git(&repo, &[
        "remote",
        "add",
        "origin",
        "https://github.com/acme/proj.git",
    ]);

    let wt = root.join("proj_worktrees").join("a");
    git(&repo, &["worktree", "add", "-b", "a", wt.to_str().unwrap()]);
    git(&wt, &["commit", "-q", "--allow-empty", "-m", "wt"]);
    (repo, wt)
}

const HOOK: &str =
    "[hooks]\nbefore_worktree_remove = [['git', 'branch', 'before-remove-marker']]\n";

#[test]
fn before_worktree_remove_runs_inside_the_worktree_before_it_goes() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let (repo, wt) = setup(tmp.path(), HOOK);
    let wt_head = rev(&wt, "HEAD").unwrap();

    let out = devkit(&repo, &state, &[
        "issue",
        "end",
        wt.to_str().unwrap(),
        "--yes",
        "--clean-worktree",
        "--no-preserve",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!wt.exists(), "the worktree survived `issue end`: {stderr}");
    assert_eq!(
        rev(&repo, MARKER).as_deref(),
        Some(wt_head.as_str()),
        "the hook did not run in the worktree: {stderr}"
    );
}

#[test]
fn a_worktree_kept_back_by_preserve_fires_no_before_worktree_remove() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    // A relative destination is a required entry's failure.
    let preserve = "[preserve.notes]\nfrom = ['a.md']\nto = 'relative/path'\nrequired = true\n";
    let (repo, wt) = setup(tmp.path(), &format!("{HOOK}{preserve}"));

    let out = devkit(&repo, &state, &[
        "issue",
        "end",
        wt.to_str().unwrap(),
        "--yes",
        "--clean-worktree",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        wt.exists(),
        "the required preserve entry did not keep the worktree: {stderr}"
    );
    assert_eq!(
        rev(&repo, MARKER),
        None,
        "the hook fired for a kept worktree: {stderr}"
    );
}
