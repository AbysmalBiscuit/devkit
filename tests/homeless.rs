//! Systemd units, cron and some sandboxes run devkit with no `HOME`. Startup
//! must survive that: hooks fail open and `devkit doctor` diagnoses a broken
//! environment, so neither can afford a panic before it parses its arguments.

#[path = "common/testenv.rs"]
mod testenv;

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

/// The exit code Rust gives a process that panicked.
const PANIC_EXIT: i32 = 101;

/// `devkit <args>` in `cwd` with neither `HOME` nor `XDG_STATE_HOME` set,
/// `stdin` piped in.
fn homeless_in(cwd: &Path, args: &[&str], stdin: &str) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    testenv::scrub_identity(&mut cmd);
    testenv::scrub_xdg_homes(&mut cmd)
        .env_remove("HOME")
        .env_remove("XDG_STATE_HOME")
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_ENFORCE_WRITES")
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .unwrap_or_else(|e| panic!("spawn devkit {args:?}: {e}"));
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("devkit output");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(
        out.status.code(),
        Some(PANIC_EXIT),
        "devkit {args:?} panicked without HOME: {stderr}"
    );
    assert!(
        !stderr.contains("panicked"),
        "devkit {args:?} reported a panic without HOME: {stderr}"
    );
    out
}

fn homeless(args: &[&str]) -> Output {
    let cwd = tempfile::tempdir().expect("temp cwd");
    homeless_in(cwd.path(), args, "")
}

/// A git project with the write harness enforced.
fn enforced_project() -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    devkit_git::Git::fixture(p.path())
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

fn pre_tool_use(project: &Path, tool: &str, input: serde_json::Value) -> Output {
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "session_id": "S",
        "cwd": project.to_string_lossy(),
        "tool_input": input,
    });
    homeless_in(
        project,
        &["hook", "pre-tool-use", "--harness", "claude-code"],
        &payload.to_string(),
    )
}

fn decision(out: &Output) -> Option<serde_json::Value> {
    assert_eq!(
        out.status.code(),
        Some(0),
        "a hook verb exits 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    (!stdout.trim().is_empty())
        .then(|| serde_json::from_str(&stdout).expect("stdout is one JSON object"))
}

#[test]
fn version_runs_without_home() {
    let out = homeless(&["--version"]);
    assert!(out.status.success(), "devkit --version should exit 0");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("devkit {}", devkit::VERSION)
    );
}

#[test]
fn doctor_reports_the_missing_home() {
    let out = homeless(&["doctor", "--json"]);
    let rows: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("doctor --json prints its rows");
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "state_dir")
        .expect("a state_dir row");
    assert_eq!(row["status"], "invalid", "{row}");
    assert!(
        row["detail"].as_str().unwrap().contains("HOME"),
        "the finding names HOME: {row}"
    );
    assert_eq!(out.status.code(), Some(1), "an invalid row fails doctor");
}

#[test]
fn a_shell_command_is_allowed_without_home() {
    let project = enforced_project();
    let out = pre_tool_use(
        project.path(),
        "Bash",
        serde_json::json!({ "command": "ls" }),
    );
    if let Some(v) = decision(&out) {
        assert_ne!(v["hookSpecificOutput"]["permissionDecision"], "deny", "{v}");
    }
}

#[test]
fn a_gated_write_is_denied_naming_home() {
    let project = enforced_project();
    let out = pre_tool_use(
        project.path(),
        "Write",
        serde_json::json!({ "file_path": "src/a.rs" }),
    );
    let v = decision(&out).expect("the write gate answers");
    let output = &v["hookSpecificOutput"];
    assert_eq!(output["permissionDecision"], "deny", "{v}");
    let reason = output["permissionDecisionReason"].as_str().unwrap();
    assert!(reason.contains("HOME"), "{reason}");
}

#[test]
fn startup_still_migrates_the_legacy_state_home() {
    let (home, mut cmd) = testenv::isolated(env!("CARGO_BIN_EXE_devkit"));
    let legacy = home.path().join(".claude/state/devkit");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("ports.json"), b"{}").unwrap();
    let xdg = home.path().join("xdg-state");

    let out = cmd
        .env("XDG_STATE_HOME", &xdg)
        .arg("--version")
        .output()
        .expect("spawn devkit --version");

    assert!(out.status.success(), "devkit --version should exit 0");
    assert!(
        xdg.join("devkit/ports.json").exists(),
        "the legacy state moved under XDG_STATE_HOME"
    );
    assert!(
        !legacy.exists(),
        "the legacy state home is gone after the move"
    );
}
