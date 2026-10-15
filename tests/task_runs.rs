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

[tasks.mix]
run = ["git", "-c", "alias.mix=!echo out1; echo err1 >&2; echo out2; echo err2 >&2; echo out3", "mix"]

[tasks.peek]
run = ["git", "-c", "alias.peek=!ls -A \"$XDG_STATE_HOME/devkit/output\"; echo end", "peek"]
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
        std::fs::write(
            f.repo.join(".gitignore"),
            "CLAUDE.local.md\ndevkit.local.toml\n",
        )
        .unwrap();
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

impl Fixture {
    /// Send a pre-tool-use payload carrying `call` and assert the hook denies
    /// it for writing into the run ledger.
    fn assert_ledger_denied(&self, call: Value) {
        let mut payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "S",
            "cwd": self.repo.to_string_lossy(),
        });
        payload
            .as_object_mut()
            .unwrap()
            .extend(call.as_object().unwrap().clone());
        let mut child = self
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
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        let v: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("no denial envelope for {call}: {e}: {out:?}"));
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
    assert!(
        String::from_utf8_lossy(&bg.stdout).contains("now"),
        "{bg:?}"
    );
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

/// A capture file has no name while the task runs, so a devrun killed before
/// it cleans up leaves nothing behind in the state directory.
#[cfg(unix)]
#[test]
fn a_capture_file_is_unlinked_while_the_task_runs() {
    let f = Fixture::new();
    let out = f.task("peek");
    assert!(out.status.success(), "{out:?}");
    assert!(f.state.join("devkit/output").is_dir(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "end",
        "the capture directory held a file while the task ran"
    );
}

/// With stdout and stderr on one file, as `2>&1` leaves them, the task's
/// lines reach it in the order the task wrote them.
#[cfg(unix)]
#[test]
fn stdout_and_stderr_on_one_file_keep_their_interleaving() {
    let f = Fixture::new();
    let log = f.state.join("mix.log");
    let file = std::fs::File::create(&log).unwrap();
    let status = f
        .devkit()
        .args(["run", "task", "mix"])
        .stdout(file.try_clone().unwrap())
        .stderr(file)
        .status()
        .unwrap();
    assert!(status.success());
    let text = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("out") || l.starts_with("err"))
        .collect();
    assert_eq!(lines, ["out1", "err1", "out2", "err2", "out3"], "{text}");
    let runs = f.runs();
    assert_eq!(
        runs[0]["tail"],
        serde_json::json!(["out1", "err1", "out2", "err2", "out3"])
    );
}

/// `--env` and `--env-file` change what the task runs, so a run given either
/// is not clean, and its record names what they set without the values.
#[test]
fn an_env_override_makes_a_run_not_clean_and_records_names_only() {
    let f = Fixture::new();
    let sha = f.head();
    assert!(
        f.run(&["task", "pass", "--env", "GIT_SECRET_TOKEN=hunter2"])
            .status
            .success()
    );
    let env_file = f.state.join("task.env");
    std::fs::write(&env_file, "FROM_FILE=s3cr3t\n").unwrap();
    assert!(
        f.run(&["task", "pass", "--env-file", env_file.to_str().unwrap()])
            .status
            .success()
    );

    let runs = f.runs();
    assert_eq!(runs.len(), 2, "{runs:#?}");
    assert_eq!(runs[0]["clean"], false, "{:#}", runs[0]);
    assert_eq!(runs[0]["env"], serde_json::json!(["GIT_SECRET_TOKEN"]));
    assert_eq!(runs[1]["clean"], false, "{:#}", runs[1]);
    assert_eq!(runs[1]["env"], serde_json::json!(["FROM_FILE"]));
    assert_eq!(runs[1]["env_file"], env_file.to_str().unwrap());
    let text = serde_json::to_string(&runs).unwrap();
    assert!(
        !text.contains("hunter2") && !text.contains("s3cr3t"),
        "{text}"
    );
    assert_eq!(f.passed("pass", Some(&sha)).status.code(), Some(1));
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
fn passed_counts_only_this_repositorys_runs() {
    let f = Fixture::new();
    let sha = f.head();
    assert!(f.task("pass").status.success());

    let other = f.repo.with_file_name("other");
    devkit_git::Git::fixture(f.repo.parent().unwrap())
        .args(["clone", "-q", "repo", "other"])
        .output()
        .unwrap();
    let out = f
        .devkit()
        .current_dir(&other)
        .args(["run", "runs", "passed", "pass", "--sha", &sha])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a run in another repository at the same commit proves nothing here: {out:?}"
    );
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
        f.assert_ledger_denied(serde_json::json!({
            "tool_name": "Write",
            "tool_input": { "file_path": target, "content": "{}" }
        }));
    }
}

#[test]
fn a_shell_redirect_into_the_ledger_is_denied() {
    let f = Fixture::new();
    assert!(f.task("pass").status.success());
    let runs = f.runs();
    let record = Path::new(runs[0]["path"].as_str().unwrap());
    let forged = record.with_file_name("forged.json");
    let relative = Path::new("..")
        .join(forged.strip_prefix(f.repo.parent().unwrap()).unwrap())
        .to_string_lossy()
        .into_owned();
    for target in [forged.to_string_lossy().into_owned(), relative] {
        f.assert_ledger_denied(serde_json::json!({
            "tool_name": "Bash",
            "tool_input": { "command": format!("echo '{{}}' > '{target}'") }
        }));
    }
}

#[test]
fn a_run_started_with_a_relative_dir_records_that_dir() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.repo.join("sub")).unwrap();
    let out = f.run(&["-C", "sub", "task", "pass"]);
    assert!(out.status.success(), "{out:?}");
    let runs = f.runs();
    let run = by_task(&runs, "pass")[0];
    assert_eq!(run["dir"], "sub", "{run:#}");
}
