//! The edit guard's stdout, pinned.
//!
//! A context-injection stage attached to this hook has to stay off the deny
//! path, and this file is what keeps it there. A second JSON object appended
//! after a denial makes the whole of stdout unparseable, and a harness that
//! cannot parse a hook's stdout treats it as plain text carrying no decision.
//! The denial is lost and the write proceeds.

#[path = "common/testenv.rs"]
mod testenv;

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

/// A private git project with the write harness enforced.
pub fn project() -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    devkit_common::git::Git::fixture(p.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    std::fs::write(
        p.path().join("devkit.toml"),
        "[harness]\nenforce_writes = true\n",
    )
    .unwrap();
    p
}

/// `devkit hook pre-tool-use` against a private HOME and state home, so a run
/// started from inside a coding agent resolves the same holder CI does.
pub fn run_hook(project: &Path, state: &Path, payload: &str) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.args(["hook", "pre-tool-use", "--harness", "claude-code"])
        .current_dir(project)
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_ENFORCE_WRITES")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    testenv::scrub_identity(&mut cmd);
    let mut child = cmd.spawn().expect("spawn the devkit hook");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    child.wait_with_output().expect("hook output")
}

/// Claim `path` for `holder`, the way another session would have. `devkit locks
/// acquire` is the same verb `lockm acquire` reaches, so no shim is needed.
pub fn hold(project: &Path, state: &Path, path: &str, holder: &str) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.args(["locks", "acquire", path, "--as", holder])
        .current_dir(project)
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("DEVKIT_SKIP_AUTOLINK", "1");
    testenv::scrub_identity(&mut cmd);
    let out = cmd.output().expect("spawn devkit locks acquire");
    assert!(out.status.success(), "the other holder should acquire");
}

pub fn write_payload(session: &str, agent: Option<&str>, cwd: &Path, target: &str) -> String {
    let mut payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "tool_input": { "file_path": target }
    });
    if let Some(agent) = agent {
        payload["agent_id"] = serde_json::Value::String(agent.to_string());
    }
    payload.to_string()
}

/// Stdout as exactly one JSON object, or `None` when it is empty. Parsing is
/// the assertion: two objects would fail here, which is the whole point.
pub fn one_object(out: &Output) -> Option<serde_json::Value> {
    assert_eq!(
        out.status.code(),
        Some(0),
        "the guard always exits 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.trim().is_empty() {
        return None;
    }
    Some(serde_json::from_str(&stdout).expect("stdout parses as exactly one JSON object"))
}

#[test]
fn a_conflicting_write_denies_with_one_object() {
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    hold(proj.path(), state.path(), "src/a.rs", "other-session");

    let payload = write_payload("S", None, proj.path(), "src/a.rs");
    let out = run_hook(proj.path(), state.path(), &payload);

    let v = one_object(&out).expect("a conflict denies");
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
}

#[test]
fn an_unconflicted_write_emits_nothing() {
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    let payload = write_payload("S", None, proj.path(), "src/a.rs");
    let out = run_hook(proj.path(), state.path(), &payload);
    assert_eq!(one_object(&out), None, "an allow is silent");
}
