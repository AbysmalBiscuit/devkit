//! The run ledger end to end: `devrun task` records every run it executes,
//! `devrun runs` reads them back, and an agent's edit tool cannot write them.
//! Drives `devkit run` directly against an isolated state home kept outside
//! the repository.

#[path = "common/testenv.rs"]
mod testenv;

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use serde_json::Value;

const CONFIG: &str = r#"
[tasks.pass]
run = ["git", "version"]

[tasks.boom]
run = ["git", "-c", "alias.boom=!echo going down; exit 3", "boom"]

[tasks.say]
run = ["git", "-c", "x.msg={{ msg }}", "version"]

[tasks.both]
steps = [{ task = "pass" }, { task = "boom" }]

[tasks.bg]
run = ["git", "-c", "alias.bg=!(sleep 3; echo late; echo x > ../marker) & echo now", "bg"]
"#;

/// A repository with one commit, and a state home beside it rather than in it.
struct Fixture {
    _tmp: tempfile::TempDir,
    repo: std::path::PathBuf,
    state: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let state = tmp.path().join("state");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        let f = Self {
            _tmp: tmp,
            repo,
            state,
        };
        f.git(&["init", "-q", "-b", "main"]);
        std::fs::write(f.repo.join("devkit.toml"), CONFIG).unwrap();
        std::fs::write(f.repo.join(".gitignore"), "CLAUDE.local.md\n").unwrap();
        f.commit("init");
        f
    }

    fn git(&self, args: &[&str]) -> String {
        devkit_git::Git::fixture(&self.repo)
            .args(args.iter().copied())
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
    }

    fn commit(&self, msg: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", msg]);
        self.head()
    }

    fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"]).trim().to_string()
    }

    fn devkit(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
        cmd.current_dir(&self.repo)
            .env("HOME", &self.state)
            .env("XDG_STATE_HOME", &self.state)
            .env("XDG_CONFIG_HOME", self.state.join("config"))
            .env("LOCALAPPDATA", &self.state)
            .env("USERPROFILE", &self.state)
            .env("DEVKIT_SKIP_AUTOLINK", "1")
            .env_remove("DEVKIT_CONFIG")
            .env_remove("DEVKIT_ENFORCE_WRITES")
            .env_remove("DEVKIT_CALLER");
        testenv::scrub_identity(&mut cmd);
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.devkit().arg("run").args(args).output().unwrap()
    }

    fn task(&self, name: &str) -> Output {
        self.run(&["task", name])
    }

    fn runs(&self) -> Vec<Value> {
        let out = self.run(&["runs", "list", "--json"]);
        assert!(out.status.success(), "{out:?}");
        serde_json::from_slice(&out.stdout).expect("runs list --json is a JSON array")
    }

    fn passed(&self, task: &str, sha: Option<&str>) -> Output {
        let mut args = vec!["runs", "passed", task];
        if let Some(sha) = sha {
            args.extend(["--sha", sha]);
        }
        self.run(&args)
    }
}

fn by_task<'a>(runs: &'a [Value], task: &str) -> Vec<&'a Value> {
    runs.iter().filter(|r| r["task"] == task).collect()
}

#[test]
fn a_passing_then_a_failing_task_records_two_runs_with_real_exit_codes() {
    let f = Fixture::new();
    let sha = f.head();

    assert_eq!(f.task("pass").status.code(), Some(0));
    let boom = f.task("boom");
    assert_eq!(boom.status.code(), Some(3), "{boom:?}");

    let runs = f.runs();
    assert_eq!(runs.len(), 2, "{runs:#?}");
    let pass = by_task(&runs, "pass")[0];
    let fail = by_task(&runs, "boom")[0];
    assert_eq!(pass["exit_code"], 0);
    assert_eq!(fail["exit_code"], 3);
    for run in [pass, fail] {
        assert_eq!(run["head"], sha.as_str(), "{run:#}");
        assert_eq!(run["clean"], true, "{run:#}");
        assert!(run["started_at"].is_string() && run["ended_at"].is_string());
    }
    let tail: Vec<&str> = fail["tail"]
        .as_array()
        .expect("piped output is captured")
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    assert!(tail.contains(&"going down"), "{tail:?}");
}

/// A writer a task leaves in the background outlives the run: its output goes
/// to a capture file rather than a pipe devrun abandons, so it never meets a
/// closed reader, and devrun returns as soon as the task itself exits.
#[cfg(unix)]
#[test]
fn a_background_writer_left_by_a_task_survives_and_the_run_returns_at_once() {
    let f = Fixture::new();
    let started = std::time::Instant::now();
    assert!(f.task("pass").status.success());
    let baseline = started.elapsed();

    let started = std::time::Instant::now();
    let bg = f.task("bg");
    let elapsed = started.elapsed();
    assert!(bg.status.success(), "{bg:?}");
    assert!(String::from_utf8_lossy(&bg.stdout).contains("now"), "{bg:?}");
    assert!(
        elapsed < baseline + std::time::Duration::from_millis(1500),
        "devrun waited on the background writer: {elapsed:?} against {baseline:?}"
    );

    let marker = f.repo.parent().unwrap().join("marker");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !marker.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(marker.exists(), "the background writer died before writing");
}

#[test]
fn a_sequence_records_one_run_with_its_failing_steps_code() {
    let f = Fixture::new();
    assert_eq!(f.task("both").status.code(), Some(3));
    let runs = f.runs();
    assert_eq!(runs.len(), 1, "{runs:#?}");
    assert_eq!(runs[0]["task"], "both");
    assert_eq!(runs[0]["exit_code"], 3);
}

#[test]
fn template_args_are_recorded_and_a_dry_run_is_not() {
    let f = Fixture::new();
    assert!(f.run(&["task", "pass", "--dry-run"]).status.success());
    assert!(
        f.run(&["task", "say", "--arg", "msg=a  b"])
            .status
            .success()
    );
    let runs = f.runs();
    assert_eq!(runs.len(), 1, "{runs:#?}");
    assert_eq!(runs[0]["args"]["msg"], "a  b");
}

#[test]
fn a_run_on_a_dirty_tree_is_recorded_as_not_clean() {
    let f = Fixture::new();
    std::fs::write(f.repo.join("CLAUDE.local.md"), "ignored harness file").unwrap();
    assert!(f.task("pass").status.success());

    std::fs::write(f.repo.join("devkit.toml"), format!("{CONFIG}\n# edited\n")).unwrap();
    assert!(f.task("pass").status.success());
    f.git(&["checkout", "--", "devkit.toml"]);

    std::fs::write(f.repo.join("new.rs"), "untracked").unwrap();
    assert!(f.task("pass").status.success());

    let clean: Vec<Option<bool>> = f.runs().iter().map(|r| r["clean"].as_bool()).collect();
    assert_eq!(
        clean,
        [Some(true), Some(false), Some(false)],
        "an ignored file keeps the tree clean; an edit or an untracked file does not"
    );
}

#[test]
fn passed_answers_yes_only_for_a_clean_zero_exit_at_that_exact_sha() {
    let f = Fixture::new();
    let parent = f.head();
    assert!(f.task("pass").status.success());

    let yes = f.passed("pass", Some(&parent));
    assert_eq!(yes.status.code(), Some(0), "{yes:?}");
    assert!(String::from_utf8_lossy(&yes.stdout).starts_with("yes"));
    assert_eq!(
        f.passed("pass", None).status.code(),
        Some(0),
        "HEAD is the default"
    );

    assert_eq!(f.passed("boom", Some(&parent)).status.code(), Some(1));
    assert_eq!(
        f.passed("pass", Some(&parent[..12])).status.code(),
        Some(1),
        "a prefix is not an exact match"
    );

    let child = f.commit("child");
    let no = f.passed("pass", Some(&child));
    assert_eq!(
        no.status.code(),
        Some(1),
        "an ancestor's run must not count: {no:?}"
    );
    assert!(String::from_utf8_lossy(&no.stdout).starts_with("no"));
    assert_eq!(f.passed("pass", None).status.code(), Some(1));

    assert_eq!(f.task("boom").status.code(), Some(3));
    assert_eq!(
        f.passed("boom", Some(&child)).status.code(),
        Some(1),
        "a failing run is no"
    );

    std::fs::write(f.repo.join("new.rs"), "untracked").unwrap();
    assert!(f.task("pass").status.success());
    assert_eq!(
        f.passed("pass", Some(&child)).status.code(),
        Some(1),
        "a dirty run is no"
    );

    let json = f.run(&["runs", "passed", "pass", "--sha", &parent, "--json"]);
    let run: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(run["head"], parent.as_str());
}

#[test]
fn the_ledger_lives_outside_the_worktree_and_an_edit_tool_cannot_write_it() {
    let f = Fixture::new();
    assert!(f.task("pass").status.success());
    let runs = f.runs();
    let path = runs[0]["path"].as_str().expect("each run names its file");
    let record = Path::new(path);
    assert!(record.is_file(), "{path}");
    assert!(
        record.starts_with(&f.state),
        "{path} is not under the state home"
    );
    assert!(
        !record.starts_with(&f.repo),
        "{path} is inside the worktree"
    );
    assert_eq!(f.git(&["status", "--porcelain"]), "");

    for target in [
        path.to_string(),
        record
            .with_file_name("forged.json")
            .to_string_lossy()
            .into_owned(),
    ] {
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Write",
            "session_id": "S",
            "cwd": f.repo.to_string_lossy(),
            "tool_input": { "file_path": target, "content": "{}" }
        })
        .to_string();
        let mut child = f
            .devkit()
            .args(["hook", "pre-tool-use", "--harness", "claude-code"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        let v: Value = serde_json::from_slice(&out.stdout).expect("a denial envelope");
        assert_eq!(
            v["hookSpecificOutput"]["permissionDecision"], "deny",
            "{v:#}"
        );
        let reason = v["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(reason.contains("run ledger"), "{reason}");
    }
}
