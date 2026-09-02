//! A `gh` stand-in on `PATH`, for driving the PR commands end to end without a
//! GitHub account.
//!
//! Unix-only: the stand-in is a shell script, so every test including this
//! module is `#![cfg(unix)]`.
//!
//! Compile-time unused helpers are expected: different test binaries include
//! this module via `#[path]` and use different subsets of it.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// A project the PR commands can run in, wired to a `gh` that answers from
/// fixtures and logs every argument vector it is handed.
pub struct Fake {
    project: tempfile::TempDir,
    bin: tempfile::TempDir,
    state: tempfile::TempDir,
    head: String,
}

/// One PR the fake `gh pr list` answers with.
pub struct Pr {
    pub number: u64,
    pub state: &'static str,
    pub is_draft: bool,
}

impl Fake {
    /// A repo on a feature branch with one commit, a `devkit.toml` carrying
    /// `extra` under `[defaults]`, and a `gh` on `PATH` reporting `pr`.
    pub fn new(extra_defaults: &str, pr: &Pr) -> Self {
        let project = tempfile::tempdir().expect("project dir");
        let git = || devkit_common::git::Git::fixture(project.path());
        git()
            .args(["init", "-q", "-b", "lev/eng-1-fix"])
            .output()
            .expect("git init");
        std::fs::write(project.path().join("README"), "x").expect("write README");
        git().args(["add", "-A"]).output().expect("git add");
        git()
            .args(["commit", "-q", "-m", "init"])
            .output()
            .expect("git commit");
        let head = git()
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("git rev-parse")
            .trim()
            .to_string();

        std::fs::write(
            project.path().join("devkit.toml"),
            format!(
                r#"
[defaults]
worktree_root = "wts"
branch_prefix = "lev/"
baseline_ref = "origin/main"
baseline_path = "b"
{extra_defaults}

[github]
pr_repo = "o/r"

[people.lev]
slack = "U_LEV"
github = "LevValle"

[people.bot]
slack = "U_BOT"
github = "sweeper[bot]"
"#
            ),
        )
        .expect("write devkit.toml");

        let bin = tempfile::tempdir().expect("fake bin dir");
        let list = bin.path().join("pr_list.json");
        std::fs::write(
            &list,
            format!(
                r#"[{{"number":{n},"state":"{state}","url":"https://github.com/o/r/pull/{n}",
                     "headRefName":"lev/eng-1-fix","headRefOid":"{head}","isDraft":{draft}}}]"#,
                n = pr.number,
                state = pr.state,
                draft = pr.is_draft,
            ),
        )
        .expect("write pr list payload");
        write_script(bin.path(), &bin.path().join("gh.log"), &list);

        let state = tempfile::tempdir().expect("state dir");
        Fake {
            project,
            bin,
            state,
            head,
        }
    }

    pub fn head(&self) -> &str {
        &self.head
    }

    pub fn project(&self) -> &Path {
        self.project.path()
    }

    /// Every `gh` argument vector the run produced, newline separated.
    pub fn calls(&self) -> String {
        std::fs::read_to_string(self.bin.path().join("gh.log")).unwrap_or_default()
    }

    /// Run `devkit issue <args…>` in the project, against the fake `gh`. Every
    /// GitHub credential is stripped so the run resolves no bearer token and
    /// takes its `gh` fallback for each lookup.
    pub fn issue(&self, args: &[&str]) -> std::process::Output {
        let path = format!(
            "{}:{}",
            self.bin.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new(env!("CARGO_BIN_EXE_devkit"))
            .env("DEVKIT_SKIP_AUTOLINK", "1")
            .env("PATH", path)
            .env("HOME", self.state.path())
            .env("XDG_STATE_HOME", self.state.path())
            .env("XDG_CONFIG_HOME", self.state.path())
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_HOST")
            .env_remove("GH_REPO")
            .env_remove("SLACK_TOKEN")
            .args(["issue", "-C"])
            .arg(self.project.path())
            .args(args)
            .output()
            .expect("spawn devkit issue")
    }
}

/// `auth token` fails so no bearer token resolves; every other verb answers
/// from a fixture or succeeds silently.
fn write_script(bin_dir: &Path, log: &Path, pr_list: &Path) -> PathBuf {
    let script = format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> '{log}'
case "$*" in
  "auth token"*) exit 1 ;;
  "pr list"*) cat '{list}' ;;
  "pr view"*reviewRequests*) printf '{{"reviewRequests":[]}}\n' ;;
  "pr view"*reviews*) printf '{{"reviews":[]}}\n' ;;
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
    gh
}
