//! A `gh` stand-in on `PATH`, for driving the PR commands end to end without a
//! GitHub account.
//!
//! The stand-in is the `ghfake` example binary, copied in under the name `gh`.
//! It answers each verb from a file in the same directory, so a test that wants
//! a different answer writes one before the run.
//!
//! Compile-time unused helpers are expected: different test binaries include
//! this module via `#[path]` and use different subsets of it.
#![allow(dead_code)]

use std::path::Path;
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
    /// The login that opened it. The reviewer gate reads this, so a test about
    /// self-review sets it to the same person it puts in the reviews payload.
    pub author: &'static str,
}

impl Fake {
    /// A repo on a feature branch with one commit, a `devkit.toml` carrying
    /// `extra` under `[defaults]`, and a `gh` on `PATH` reporting `pr`.
    ///
    /// `extra` lands at the end of `[defaults]`, so a caller that needs another
    /// table (`[templates]`, say) opens one there.
    pub fn new(extra: &str, pr: &Pr) -> Self {
        Self::build(extra, Some(pr))
    }

    /// The same project, with `gh pr list` reporting no PR at all.
    pub fn without_pr(extra: &str) -> Self {
        Self::build(extra, None)
    }

    fn build(extra_defaults: &str, pr: Option<&Pr>) -> Self {
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
baseline_dir = "b"
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
        let payload = match pr {
            Some(pr) => format!(
                r#"[{{"number":{n},"state":"{state}","url":"https://github.com/o/r/pull/{n}",
                     "headRefName":"lev/eng-1-fix","headRefOid":"{head}","isDraft":{draft},
                     "author":{{"login":"{author}"}}}}]"#,
                n = pr.number,
                state = pr.state,
                draft = pr.is_draft,
                author = pr.author,
            ),
            None => "[]".to_string(),
        };
        std::fs::write(bin.path().join("pr_list.json"), payload).expect("write pr list payload");
        install_fake_gh(bin.path());

        let state = tempfile::tempdir().expect("state dir");
        Fake {
            project,
            bin,
            state,
            head,
        }
    }

    /// Answer `gh pr view --json reviews` with `payload` for the rest of this
    /// fake's life. Without one the fake reports no reviews at all.
    pub fn set_reviews(&self, payload: &str) {
        std::fs::write(self.bin.path().join("reviews.json"), payload).expect("write reviews");
    }

    /// Answer `gh pr view --json reviewRequests` with `payload`.
    pub fn set_review_requests(&self, payload: &str) {
        std::fs::write(self.bin.path().join("review_requests.json"), payload)
            .expect("write review requests");
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
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(self.bin.path().to_path_buf()).chain(std::env::split_paths(&inherited)),
        )
        .expect("join PATH");
        Command::new(env!("CARGO_BIN_EXE_devkit"))
            .env("DEVKIT_SKIP_AUTOLINK", "1")
            .env("PATH", path)
            .env("GHFAKE_DIR", self.bin.path())
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

/// Copy the `ghfake` example binary into `bin_dir` under the name `gh`, so a
/// `PATH` carrying that directory resolves it the way `Command::new("gh")`
/// looks: exact name plus the platform's executable suffix, never a script.
fn install_fake_gh(bin_dir: &Path) {
    let name = format!("ghfake{}", std::env::consts::EXE_SUFFIX);
    let built = Path::new(env!("CARGO_BIN_EXE_devkit"))
        .parent()
        .expect("target dir")
        .join("examples")
        .join(&name);
    let gh = bin_dir.join(format!("gh{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(&built, &gh)
        .unwrap_or_else(|e| panic!("copy {} to {}: {e}", built.display(), gh.display()));
}
