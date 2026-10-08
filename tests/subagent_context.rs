//! What a subagent starts with: the project brief and the repository's rules,
//! answered to `SubagentStart` the way each harness reads a hook's context,
//! while the main session's start stays as it was.

#[path = "common/testenv.rs"]
mod testenv;

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use serde_json::{Value, json};

/// A git checkout with the fixture rule index, so both the brief and
/// `rules context` have something to say.
fn project() -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    devkit_git::Git::fixture(p.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    let index = p.path().join("index.json");
    std::fs::copy("crates/devkit-rules/tests/fixtures/index.json", &index).unwrap();
    std::fs::write(
        p.path().join("devkit.toml"),
        // A literal string: a Windows path's backslashes are not escapes.
        format!("[rules]\nenabled = true\nindex = '{}'\n", index.display()),
    )
    .unwrap();
    p
}

fn devkit(cwd: &Path, home: &Path, args: &[&str], stdin: &Value) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("XDG_STATE_HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_ENFORCE_WRITES")
        .env_remove("CURSOR_PROJECT_DIR")
        .env_remove("CURSOR_PLUGIN_ROOT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    testenv::scrub_identity(&mut cmd);
    let mut child = cmd.spawn().expect("spawn devkit");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn claude_subagent(cwd: &Path) -> Value {
    json!({
        "session_id": "S",
        "hook_event_name": "SubagentStart",
        "agent_id": "a1",
        "agent_type": "general-purpose",
        "cwd": cwd.to_string_lossy(),
    })
}

/// Codex's `SubagentStart` as it was captured.
fn codex_subagent(cwd: &Path) -> Value {
    let line = std::fs::read_to_string("tests/fixtures/todo/codex-subagent-start.jsonl").unwrap();
    let mut v: Value = serde_json::from_str(line.lines().next().unwrap()).unwrap();
    v["cwd"] = json!(cwd.to_string_lossy());
    v["transcript_path"] = Value::Null;
    v
}

/// The context a `SubagentStart` answer adds, asserting it is one.
fn subagent_context(out: &Output) -> String {
    let v: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SubagentStart");
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext")
        .to_string()
}

#[test]
fn a_claude_code_subagent_gets_the_brief_and_the_rules() {
    let (proj, home) = (project(), tempfile::tempdir().unwrap());
    let payload = claude_subagent(proj.path());
    let brief = devkit(
        proj.path(),
        home.path(),
        &["brief", "--harness", "claude-code"],
        &payload,
    );
    assert!(subagent_context(&brief).contains("## devkit project context"));
    let rules = devkit(
        proj.path(),
        home.path(),
        &["rules", "context", "--harness", "claude-code"],
        &payload,
    );
    assert!(subagent_context(&rules).contains("Root must"));
}

#[test]
fn a_codex_subagent_gets_the_brief_and_the_rules() {
    let (proj, home) = (project(), tempfile::tempdir().unwrap());
    let payload = codex_subagent(proj.path());
    let brief = devkit(
        proj.path(),
        home.path(),
        &["brief", "--harness", "codex"],
        &payload,
    );
    assert!(subagent_context(&brief).contains("## devkit project context"));
    let rules = devkit(
        proj.path(),
        home.path(),
        &["rules", "context", "--harness", "codex"],
        &payload,
    );
    assert!(subagent_context(&rules).contains("Root must"));
}

/// A fork inherits its parent's context, brief and rules included.
#[test]
fn a_fork_gets_neither() {
    let (proj, home) = (project(), tempfile::tempdir().unwrap());
    let mut fork = claude_subagent(proj.path());
    fork.as_object_mut().unwrap().remove("agent_type");
    let brief: &[&str] = &["brief", "--harness", "claude-code"];
    let rules: &[&str] = &["rules", "context", "--harness", "claude-code"];
    for args in [brief, rules] {
        let out = devkit(proj.path(), home.path(), args, &fork);
        assert!(
            out.stdout.is_empty(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

/// Claude Code's main session reads both as plain stdout.
#[test]
fn the_main_session_start_is_unchanged() {
    let (proj, home) = (project(), tempfile::tempdir().unwrap());
    let payload = json!({
        "session_id": "S",
        "hook_event_name": "SessionStart",
        "source": "startup",
        "cwd": proj.path().to_string_lossy(),
    });
    let brief = devkit(proj.path(), home.path(), &["brief"], &payload);
    let brief = String::from_utf8(brief.stdout).unwrap();
    assert!(brief.starts_with("## devkit project context\n"), "{brief}");
    let rules = devkit(proj.path(), home.path(), &["rules", "context"], &payload);
    let rules = String::from_utf8(rules.stdout).unwrap();
    assert!(
        rules.starts_with("## Rules for all code in this repository\n"),
        "{rules}"
    );
}

/// The context a `PreToolUse` write by `agent` adds, if any.
fn write_context(cwd: &Path, home: &Path, agent: Option<&str>) -> Option<String> {
    let mut payload = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "session_id": "S",
        "cwd": cwd.to_string_lossy(),
        "tool_input": { "file_path": cwd.join("notes.md").to_string_lossy() },
    });
    if let Some(agent) = agent {
        payload["agent_id"] = json!(agent);
        payload["agent_type"] = json!("general-purpose");
    }
    let out = devkit(
        cwd,
        home,
        &["hook", "pre-tool-use", "--harness", "claude-code"],
        &payload,
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    (!stdout.trim().is_empty()).then(|| {
        let v: Value = serde_json::from_str(&stdout).unwrap();
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    })
}

/// What a subagent was shown at its start is not shown again by its first
/// write, and the main session's own record is untouched by it.
#[test]
fn rules_a_subagent_started_with_are_not_injected_again_on_its_first_write() {
    let (proj, home) = (project(), tempfile::tempdir().unwrap());
    devkit(
        proj.path(),
        home.path(),
        &["rules", "context", "--harness", "claude-code"],
        &claude_subagent(proj.path()),
    );
    let subagent = write_context(proj.path(), home.path(), Some("a1")).unwrap_or_default();
    assert!(!subagent.contains("Root must"), "{subagent}");
    let session = write_context(proj.path(), home.path(), None).unwrap_or_default();
    assert!(session.contains("Root must"), "{session}");
}

/// A subagent's payload carries its parent's session id, so a brief stamped
/// under it would tell the session it had already been briefed.
#[test]
fn a_subagent_brief_leaves_the_session_watermark_alone() {
    let (proj, home) = (project(), tempfile::tempdir().unwrap());
    devkit(
        proj.path(),
        home.path(),
        &["brief", "--harness", "claude-code"],
        &claude_subagent(proj.path()),
    );
    let session = json!({
        "session_id": "S",
        "hook_event_name": "CwdChanged",
        "cwd": proj.path().to_string_lossy(),
    });
    let out = devkit(
        proj.path(),
        home.path(),
        &["brief", "--if-changed"],
        &session,
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("## devkit project context\n"), "{text}");
}
