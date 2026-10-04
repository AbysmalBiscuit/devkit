//! A `gh` stand-in on `PATH`, for driving the PR commands end to end without a
//! GitHub account.
//!
//! The stand-in is the `ghfake` example binary, copied in under the name `gh`.
//! It answers each verb from a file in the same directory, so a test that wants
//! a different answer writes one before the run.
//!
//! It is a compiled binary rather than a script because a Windows spawn of
//! `gh` looks for `gh.exe` and consults no PATHEXT, so a `.cmd` would never be
//! found. It is an example rather than a workspace member because
//! `cargo test --no-run` builds examples but skips the bins of a member crate
//! with no tests of its own.
//!
//! Compile-time unused helpers are expected: different test binaries include
//! this module via `#[path]` and use different subsets of it.
#![allow(dead_code)]

use std::{path::Path, process::Command};

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
        Self::build(extra, std::slice::from_ref(pr), None)
    }

    /// The same project, with `gh pr list` reporting every one of `prs` for
    /// the branch.
    pub fn with_prs(extra: &str, prs: &[Pr]) -> Self {
        Self::build(extra, prs, None)
    }

    /// The same project, with `gh pr list` reporting no PR at all.
    pub fn without_pr(extra: &str) -> Self {
        Self::build(extra, &[], None)
    }

    /// A project with no `[forge]` table and `origin` set to `url`, so the
    /// forge and every repository it acts on are detected from the remote.
    pub fn with_origin(url: &str) -> Self {
        Self::build("", &[], Some(url))
    }

    /// [`Fake::with_origin`] with `extra` appended to the config, which may
    /// open tables of its own (`[forge]`, say).
    pub fn with_origin_and(extra: &str, url: &str) -> Self {
        Self::build(extra, &[], Some(url))
    }

    fn build(extra_defaults: &str, prs: &[Pr], origin: Option<&str>) -> Self {
        let project = tempfile::tempdir().expect("project dir");
        let git = || devkit_git::Git::fixture(project.path());
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
        let github = match origin {
            Some(url) => {
                git()
                    .args(["remote", "add", "origin", url])
                    .output()
                    .expect("git remote add");
                ""
            }
            None => "[forge]\nkind = \"github\"\nrepo = \"o/r\"\n\n[github]\nissues_repo = \"o/r\"",
        };

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

{github}

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
        let listed: Vec<String> = prs.iter().map(|pr| pr_json(pr, &head)).collect();
        std::fs::write(
            bin.path().join("pr_list.json"),
            format!("[{}]", listed.join(",")),
        )
        .expect("write pr list payload");
        install_fake_gh(bin.path());

        let state = tempfile::tempdir().expect("state dir");
        Fake {
            project,
            bin,
            state,
            head,
        }
    }

    /// Answer `gh pr view <n>` with `pr`, at this project's head. Without one
    /// the fake reports that the PR does not exist.
    pub fn serve_pr(&self, pr: &Pr) {
        std::fs::write(
            self.bin.path().join("pr_view.json"),
            pr_json(pr, &self.head),
        )
        .expect("write pr view");
    }

    /// Have `gh pr create` open `pr`, which `gh pr view` then serves.
    pub fn create_opens(&self, pr: &Pr) {
        std::fs::write(
            self.bin.path().join("pr_create.txt"),
            format!("https://github.com/o/r/pull/{}\n", pr.number),
        )
        .expect("write pr create answer");
        self.serve_pr(pr);
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

    /// Answer `gh api graphql` with issue `body`, the way the tracker's issue
    /// query reads it. Without one the fake fails the call.
    pub fn serve_issue(&self, body: &str) {
        let payload = serde_json::json!({ "data": { "repository": { "issue": {
            "title": "An issue",
            "url": "https://github.com/o/r/issues/7",
            "body": body,
            "state": "OPEN",
            "stateReason": null,
            "assignees": { "pageInfo": { "hasNextPage": false }, "nodes": [] },
            "labels": { "pageInfo": { "hasNextPage": false }, "nodes": [] },
        } } } });
        std::fs::write(self.bin.path().join("graphql.json"), payload.to_string())
            .expect("write graphql answer");
    }

    /// Answer every `gh api graphql` query with `body`. Without one the fake
    /// fails the call.
    pub fn serve_graphql(&self, body: &str) {
        std::fs::write(self.bin.path().join("graphql.json"), body).expect("write graphql answer");
    }

    /// Answer every `gh api graphql` mutation with `body`. Without one the
    /// fake fails the call.
    pub fn serve_mutation(&self, body: &str) {
        std::fs::write(self.bin.path().join("graphql_mutation.json"), body)
            .expect("write mutation answer");
    }

    /// Add `keys` to the config's `[github]` table, which `extra` cannot
    /// reopen.
    pub fn github_keys(&self, keys: &str) {
        let path = self.project().join("devkit.toml");
        let toml = std::fs::read_to_string(&path).expect("read devkit.toml");
        let toml = toml.replacen("[github]\n", &format!("[github]\n{keys}\n"), 1);
        std::fs::write(&path, toml).expect("write devkit.toml");
    }

    /// Make the project an `issue setup` worktree for issue `id`.
    pub fn record_issue(&self, id: &str) {
        devkit_common::record::write(self.project(), &devkit_common::record::IssueRecord {
            issue: id.into(),
            slug: "fix".into(),
            apps: Vec::new(),
            summary: None,
            pr: None,
            baseline: None,
            ..Default::default()
        })
        .expect("write issue record");
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
        self.issue_cmd(args).output().expect("spawn devkit issue")
    }

    /// [`Fake::issue`] with `DEVKIT_CALLER` set to `caller`.
    pub fn issue_as(&self, caller: &str, args: &[&str]) -> std::process::Output {
        self.issue_cmd(args)
            .env("DEVKIT_CALLER", caller)
            .output()
            .expect("spawn devkit issue")
    }

    /// [`Fake::issue`] with `stdin` piped in.
    pub fn issue_with_stdin(&self, args: &[&str], stdin: &[u8]) -> std::process::Output {
        with_stdin(self.issue_cmd(args), stdin)
    }

    /// Run `devkit ARGS` against the fake `gh`, with `stdin` piped in, in the
    /// environment [`Fake::issue`] sets up. A process it spawns inherits that
    /// environment.
    pub fn devkit_with_stdin(&self, args: &[&str], stdin: &[u8]) -> std::process::Output {
        let mut cmd = self.devkit_cmd();
        cmd.args(args);
        with_stdin(cmd, stdin)
    }

    fn issue_cmd(&self, args: &[&str]) -> Command {
        let mut cmd = self.devkit_cmd();
        cmd.args(["issue", "-C"])
            .arg(self.project.path())
            .args(args);
        cmd
    }

    fn devkit_cmd(&self) -> Command {
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(self.bin.path().to_path_buf()).chain(std::env::split_paths(&inherited)),
        )
        .expect("join PATH");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
        cmd.env("DEVKIT_SKIP_AUTOLINK", "1")
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
            .env_remove("DEVKIT_CALLER");
        cmd
    }
}

fn with_stdin(mut cmd: Command, stdin: &[u8]) -> std::process::Output {
    use std::{io::Write, process::Stdio};

    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn devkit");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(stdin)
        .expect("write stdin");
    child.wait_with_output().expect("wait devkit")
}

/// `pr` as `gh --json` reports it, on this project's branch at `head`.
fn pr_json(pr: &Pr, head: &str) -> String {
    format!(
        r#"{{"number":{n},"state":"{state}","url":"https://github.com/o/r/pull/{n}",
             "headRefName":"lev/eng-1-fix","headRefOid":"{head}","isDraft":{draft},
             "author":{{"login":"{author}"}}}}"#,
        n = pr.number,
        state = pr.state,
        draft = pr.is_draft,
        author = pr.author,
    )
}

/// Copy the `ghfake` example binary into `bin_dir` under the name `gh`, so a
/// `PATH` carrying that directory resolves it the way `Command::new("gh")`
/// looks: exact name plus the platform's executable suffix, never a script.
/// `pub(crate)` so another test binary's own fixture (built around a plain
/// project directory rather than `Fake`) can install the same stand-in.
pub(crate) fn install_fake_gh(bin_dir: &Path) {
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
