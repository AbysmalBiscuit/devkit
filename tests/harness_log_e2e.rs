//! `devkit hook pre-tool-use` with logging on, through the real binary.
//!
//! Two of these are the failure contract, and without them it is a comment: a
//! panic in the logging path leaves the verdict byte-identical, and a record
//! that blocks past its deadline neither delays nor alters the envelope.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

/// A project, a private HOME, and a global config that turns logging on. The
/// global layer is where it has to be: a project layer cannot enable logging.
struct Env {
    project: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Env {
    fn log_dir(&self) -> PathBuf {
        self.home.path().join("logs")
    }
}

fn env_with(project_config: &str, global_extra: &str) -> Env {
    let project = tempfile::tempdir().unwrap();
    devkit_common::git::Git::fixture(project.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    std::fs::write(project.path().join("devkit.toml"), project_config).unwrap();

    let home = tempfile::tempdir().unwrap();
    let cfg_dir = home.path().join(".config/devkit");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        format!(
            "[harness.log]\nenabled = true\nauto_prune = false\ndir = {:?}\n{global_extra}",
            home.path().join("logs").to_string_lossy()
        ),
    )
    .unwrap();
    Env { project, home }
}

fn enabled_project() -> Env {
    env_with("[harness]\nenforce_commands = true\n", "")
}

fn run_argv(e: &Env, argv: &[&str], payload: &str) -> Output {
    run_argv_env(e, argv, payload, &[])
}

fn run_argv_env(e: &Env, argv: &[&str], payload: &str, extra: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.args(argv)
        .current_dir(e.project.path())
        .env("HOME", e.home.path())
        .env("XDG_STATE_HOME", e.home.path())
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_HARNESS_LOG")
        .env_remove("DEVKIT_HARNESS_LOG_FAULT")
        .env_remove("DEVKIT_ENFORCE_COMMANDS")
        .env_remove("DEVKIT_ENFORCE_WRITES")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn the devkit hook");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    child.wait_with_output().expect("hook output")
}

/// A Claude Code shell payload, carrying the project as `cwd` the way a real
/// one does: without it the analyser has nothing to resolve a relative write
/// target against.
fn claude_payload(e: &Env, command: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "session_id": "s1",
        "tool_use_id": "u1",
        "cwd": e.project.path().to_string_lossy(),
        "tool_input": { "command": command }
    })
    .to_string()
}

/// Every record under the log directory, oldest first by file.
fn records(dir: &Path) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let Ok(days) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut days: Vec<_> = days.filter_map(Result::ok).map(|e| e.path()).collect();
    days.sort();
    for day in days.iter().filter(|p| p.is_dir()) {
        let mut files: Vec<_> = std::fs::read_dir(day)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
        files.sort();
        for f in files {
            for line in std::fs::read_to_string(&f).unwrap().lines() {
                out.push(serde_json::from_str(line).unwrap_or_else(|e| {
                    panic!("{}: a record must parse: {e}\n{line}", f.display())
                }));
            }
        }
    }
    out
}

fn sole_record(dir: &Path) -> serde_json::Value {
    let mut all = records(dir);
    assert_eq!(all.len(), 1, "expected exactly one record, got {all:?}");
    all.remove(0)
}

#[test]
fn a_guarded_command_lands_a_record_carrying_its_verdict() {
    let e = env_with(
        "[harness]\nenforce_commands = true\n[harness.commands.no-node]\nprograms = [\"node\"]\nreason = \"use devrun\"\n",
        "",
    );
    let out = run_argv(
        &e,
        &["hook", "pre-tool-use"],
        &claude_payload(&e, "node server.js"),
    );
    assert_eq!(out.status.code(), Some(0));
    let rec = sole_record(&e.log_dir());
    assert_eq!(rec["kind"], "shell_pre");
    assert_eq!(rec["event"], "pre-tool-use");
    assert_eq!(rec["vendor_event"], "PreToolUse");
    assert_eq!(rec["session_id"], "s1");
    assert_eq!(rec["tool_use_id"], "u1", "so a post record can join to it");
    assert_eq!(rec["verdict"]["decision"], "deny");
    assert!(
        !rec["verdict"]["blocks"].as_array().unwrap().is_empty(),
        "the correction devkit offered is what says whether a denial was a good one"
    );
    assert_eq!(rec["command"], "node server.js");
    assert!(rec["analysis"]["counts"]["invocations"].as_u64().unwrap() >= 1);
}

#[test]
fn nothing_is_recorded_when_the_global_config_does_not_enable_it() {
    let e = enabled_project();
    std::fs::remove_file(e.home.path().join(".config/devkit/config.toml")).unwrap();
    run_argv(&e, &["hook", "pre-tool-use"], &claude_payload(&e, "ls"));
    assert!(records(&e.log_dir()).is_empty(), "off by default");
}

/// The failure contract, half one: a panic anywhere in the logging path leaves
/// the verdict byte-identical. `guard_shell` turns a panic raised after its
/// write stage is live into a denial, so a logging panic that escaped would
/// deny a command the guard had already allowed.
#[test]
fn a_panic_in_the_logging_path_leaves_the_verdict_unchanged() {
    let e = enabled_project();
    let clean = run_argv(&e, &["hook", "pre-tool-use"], &claude_payload(&e, "ls"));
    let faulted = run_argv_env(&e, &["hook", "pre-tool-use"], &claude_payload(&e, "ls"), &[
        ("DEVKIT_HARNESS_LOG_FAULT", "panic"),
    ]);
    assert_eq!(
        clean.stdout, faulted.stdout,
        "logging must never reach the verdict"
    );
    assert_eq!(faulted.status.code(), Some(0));
}

/// The failure contract, half two: a record that blocks past its deadline
/// neither delays nor alters the envelope. A stall before the envelope exists
/// would run into the manifest's 30-second timeout, and a harness timeout
/// allows the call — turning a denial into an allow.
#[test]
fn a_blocked_write_never_delays_or_changes_the_envelope() {
    let e = env_with(
        "[harness]\nenforce_commands = true\n[harness.commands.no-node]\nprograms = [\"node\"]\nreason = \"use devrun\"\n",
        "",
    );
    let started = std::time::Instant::now();
    let out = run_argv_env(
        &e,
        &["hook", "pre-tool-use"],
        &claude_payload(&e, "node server.js"),
        &[("DEVKIT_HARNESS_LOG_FAULT", "block")],
    );
    let elapsed = started.elapsed();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "well inside the manifest's 30s timeout: {elapsed:?}"
    );
}

#[test]
fn a_payload_that_does_not_parse_is_still_recorded() {
    let e = enabled_project();
    run_argv(&e, &["hook", "pre-tool-use"], "{not json");
    let rec = sole_record(&e.log_dir());
    assert_eq!(rec["kind"], "shell_pre");
    assert_eq!(
        rec["verdict"]["decision"], "undecided",
        "a payload devkit could not read is precisely what this is for"
    );
}

#[test]
fn a_payload_that_is_not_a_shell_command_is_recorded_as_undecided() {
    let e = enabled_project();
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "session_id": "s1",
        "tool_input": {}
    })
    .to_string();
    run_argv(&e, &["hook", "pre-tool-use"], &payload);
    let rec = sole_record(&e.log_dir());
    assert_eq!(rec["kind"], "shell_pre");
    assert_eq!(rec["verdict"]["decision"], "undecided");
}

/// With logging on and enforcement off, the analysis runs anyway: a record with
/// an empty verdict is half a record.
#[test]
fn logging_on_with_enforcement_off_still_analyses() {
    let e = env_with("[harness]\n", "");
    run_argv(
        &e,
        &["hook", "pre-tool-use"],
        &claude_payload(&e, "cat a > b.txt"),
    );
    let rec = sole_record(&e.log_dir());
    assert_eq!(rec["verdict"]["decision"], "allow");
    assert!(
        rec["analysis"]["resolved_writes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().ends_with("b.txt")),
        "{rec}"
    );
}

/// A project layer may lower the fidelity the global config set, and nothing
/// else about the log.
#[test]
fn a_project_layer_lowers_the_command_fidelity() {
    let e = env_with(
        "[harness]\n[harness.log]\ncommand = \"hashed\"\n",
        "command = \"full\"\n",
    );
    run_argv(
        &e,
        &["hook", "pre-tool-use"],
        &claude_payload(&e, "echo hi"),
    );
    let rec = sole_record(&e.log_dir());
    let command = rec["command"].as_str().unwrap();
    assert!(command.starts_with("sha256:"), "{command}");
    assert!(!command.contains("echo"));
}

#[test]
fn a_token_in_a_command_is_redacted_by_default() {
    let e = enabled_project();
    run_argv(
        &e,
        &["hook", "pre-tool-use"],
        &claude_payload(&e, "GH_TOKEN=ghp_abcdefghijklmnop gh pr list"),
    );
    let rec = sole_record(&e.log_dir());
    let command = rec["command"].as_str().unwrap();
    assert!(!command.contains("ghp_abcdefghijklmnop"), "{command}");
    assert!(command.contains("gh pr list"), "{command}");
    assert_eq!(rec["redacted"], true);
}

#[test]
fn an_edit_payload_lands_an_edit_record() {
    let e = env_with("[harness]\nenforce_writes = true\n", "");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "session_id": "s1",
        "cwd": e.project.path().to_string_lossy(),
        "tool_input": { "file_path": "src/a.rs" }
    })
    .to_string();
    run_argv(&e, &["hook", "pre-tool-use"], &payload);
    let rec = sole_record(&e.log_dir());
    assert_eq!(rec["kind"], "edit_pre");
    assert_eq!(rec["verdict"]["decision"], "allow");
    assert_eq!(rec["targets"][0], "src/a.rs");
}

/// The dialect a record names parses back through `Dialect::from_name`, the
/// function the corpus reader uses, so renaming a variant cannot orphan a
/// recorded corpus.
#[test]
fn a_recorded_dialect_reads_back_as_the_same_dialect() {
    use devkit_command::Dialect;
    for (tool, want) in [("Bash", Dialect::Bash), ("PowerShell", Dialect::PowerShell)] {
        let e = enabled_project();
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": tool,
            "session_id": "s1",
            "cwd": e.project.path().to_string_lossy(),
            "tool_input": { "command": "ls" }
        })
        .to_string();
        run_argv(&e, &["hook", "pre-tool-use"], &payload);
        let rec = sole_record(&e.log_dir());
        let name = rec["dialect"].as_str().unwrap_or_default();
        assert_eq!(Dialect::from_name(name), Some(want), "{tool}: {name}");
    }
}

/// The payload carries none of the fields inference reads for Codex, so only
/// the declared harness can name it.
#[test]
fn an_edit_record_names_the_harness_the_manifest_declared() {
    let e = env_with("[harness]\nenforce_writes = true\n", "");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "session_id": "s1",
        "cwd": e.project.path().to_string_lossy(),
        "tool_input": { "file_path": "src/a.rs" }
    })
    .to_string();
    run_argv(
        &e,
        &["hook", "pre-tool-use", "--harness", "codex"],
        &payload,
    );
    let rec = sole_record(&e.log_dir());
    assert_eq!(rec["kind"], "edit_pre");
    assert_eq!(rec["harness"], "codex");
}

/// A subagent writes to its own file: parallel subagents share a session id and
/// would otherwise contend on one.
#[test]
fn a_subagent_writes_beside_its_session_not_into_it() {
    let e = enabled_project();
    let with_agent = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "session_id": "s1",
        "agent_id": "a1",
        "cwd": e.project.path().to_string_lossy(),
        "tool_input": { "command": "ls" }
    })
    .to_string();
    run_argv(&e, &["hook", "pre-tool-use"], &claude_payload(&e, "ls"));
    run_argv(&e, &["hook", "pre-tool-use"], &with_agent);
    let day = std::fs::read_dir(e.log_dir())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut names: Vec<String> = std::fs::read_dir(day)
        .unwrap()
        .map(|f| f.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["s1-a1.jsonl", "s1.jsonl"]);
}

/// Every `git` the hook spawns, one argument line per spawn, read through a
/// wrapper that logs and then hands off to the real git. Unix-only: the wrapper
/// is a shell script, and a Windows spawn of `git` looks only for `git.exe`.
#[cfg(unix)]
fn git_spawns(e: &Env, argv: &[&str], payload: &str) -> Vec<String> {
    use std::os::unix::fs::PermissionsExt;

    let path = std::env::var_os("PATH").unwrap_or_default();
    let real = std::env::split_paths(&path)
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("git on PATH");
    let bin = tempfile::tempdir().unwrap();
    let trace = bin.path().join("spawns");
    let wrapper = bin.path().join("git");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            trace.to_string_lossy(),
            real.to_string_lossy(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin.path().to_path_buf()];
    dirs.extend(std::env::split_paths(&path));
    let joined = std::env::join_paths(dirs).unwrap();

    run_argv_env(e, argv, payload, &[("PATH", &joined.to_string_lossy())]);
    std::fs::read_to_string(&trace)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// With logging and both enforcement flags on, a guarded shell command asks git
/// about its checkout once. The log settings, the gates, the rule layers, the
/// config load, the lock scoping and the record's project root all share it.
#[cfg(unix)]
#[test]
fn a_logged_shell_command_spawns_git_once() {
    let e = env_with(
        "[harness]\nenforce_commands = true\nenforce_writes = true\n",
        "",
    );
    let spawns = git_spawns(
        &e,
        &["hook", "pre-tool-use"],
        &claude_payload(&e, "echo hi > out.txt"),
    );
    assert_eq!(spawns.len(), 1, "{spawns:#?}");
    assert_eq!(sole_record(&e.log_dir())["verdict"]["decision"], "allow");
}

#[cfg(unix)]
#[test]
fn a_logged_edit_spawns_git_once() {
    let e = env_with("[harness]\nenforce_writes = true\n", "");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "session_id": "s1",
        "cwd": e.project.path().to_string_lossy(),
        "tool_input": { "file_path": "src/a.rs" }
    })
    .to_string();
    let spawns = git_spawns(&e, &["hook", "pre-tool-use"], &payload);
    assert_eq!(spawns.len(), 1, "{spawns:#?}");
    assert_eq!(sole_record(&e.log_dir())["kind"], "edit_pre");
}

#[cfg(unix)]
#[test]
fn a_logged_record_only_verb_spawns_git_once() {
    let e = enabled_project();
    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "session_id": "s1",
        "cwd": e.project.path().to_string_lossy(),
        "tool_input": { "command": "ls" },
        "tool_response": { "stdout": "", "stderr": "", "interrupted": false }
    })
    .to_string();
    let spawns = git_spawns(&e, &["hook", "post-tool-use"], &payload);
    assert_eq!(spawns.len(), 1, "{spawns:#?}");
    assert!(sole_record(&e.log_dir())["project_root"].is_string());
}
