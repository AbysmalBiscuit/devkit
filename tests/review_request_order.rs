//! `issue review request` resolves who it will notify before it changes the PR.
//!
//! A run with no `--to` against a PR carrying no reviewers cannot name a
//! recipient and refuses. Flipping the draft first would leave the PR promoted
//! to ready by a run that told nobody and exited non-zero.
//!
//! Unix-only: the `gh` stand-in the run resolves off `PATH` is a shell script.
#![cfg(unix)]

use std::path::Path;
use std::process::Command;

/// A `gh` stand-in on `PATH` that logs every argument vector it is handed.
/// `auth token` fails so the run resolves no bearer token and takes its `gh`
/// fallback for every lookup, which is what routes those through this script.
fn write_fake_gh(bin_dir: &Path, log: &Path, pr_list: &Path) {
    let script = format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> '{log}'
case "$*" in
  "auth token"*) exit 1 ;;
  "pr list"*) cat '{list}' ;;
  "pr view"*) printf '{{"reviewRequests":[]}}\n' ;;
  "pr ready"*) exit 0 ;;
  "pr edit"*) exit 0 ;;
  *) exit 1 ;;
esac
"#,
        log = log.display(),
        list = pr_list.display(),
    );
    let gh = bin_dir.join("gh");
    std::fs::write(&gh, script).expect("write fake gh");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).expect("chmod fake gh");
}

/// A repo on a feature branch with one commit, plus a `devkit.toml` naming a PR
/// repository, which is everything `review request` needs before it talks to
/// GitHub. Returns the guard and the branch head oid the PR must carry.
fn project() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("project dir");
    let git = || devkit_common::git::Git::fixture(dir.path());
    git()
        .args(["init", "-q", "-b", "lev/eng-1-fix"])
        .output()
        .expect("git init");
    std::fs::write(dir.path().join("README"), "x").expect("write README");
    git().args(["add", "-A"]).output().expect("git add");
    git()
        .args(["commit", "-q", "-m", "init"])
        .output()
        .expect("git commit");
    std::fs::write(
        dir.path().join("devkit.toml"),
        r#"
[defaults]
worktree_root = "wts"
branch_prefix = "lev/"
baseline_ref = "origin/main"
baseline_path = "b"

[github]
pr_repo = "o/r"
"#,
    )
    .expect("write devkit.toml");
    let head = git()
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse")
        .trim()
        .to_string();
    (dir, head)
}

#[test]
fn a_refused_request_leaves_the_draft_alone() {
    let (dir, head) = project();
    let bin = tempfile::tempdir().expect("fake bin dir");
    let state = tempfile::tempdir().expect("state dir");

    let log = bin.path().join("gh.log");
    let pr_list = bin.path().join("pr_list.json");
    std::fs::write(
        &pr_list,
        format!(
            r#"[{{"number":1,"state":"OPEN","url":"https://github.com/o/r/pull/1",
                 "headRefName":"lev/eng-1-fix","headRefOid":"{head}","isDraft":true}}]"#
        ),
    )
    .expect("write pr list payload");
    write_fake_gh(bin.path(), &log, &pr_list);

    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("PATH", path)
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .env("XDG_CONFIG_HOME", state.path())
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_HOST")
        .env_remove("GH_REPO")
        .env_remove("SLACK_TOKEN")
        .args(["issue", "-C"])
        .arg(dir.path())
        .args(["review", "request", "--no-push"])
        .output()
        .expect("spawn issue review request");

    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !out.status.success(),
        "a request with no recipient must fail: {stderr}"
    );
    assert!(
        stderr.contains("no reviewers on the PR"),
        "the refusal must name the missing recipients: {stderr}"
    );

    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        calls.contains("pr view"),
        "the run must have reached target resolution: {calls}"
    );
    assert!(
        !calls.contains("pr ready"),
        "a refused run must not have promoted the draft: {calls}"
    );
}
