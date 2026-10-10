//! The pre-tool-use hook gates an issue write through a tracker's MCP tool on
//! the receipts `devkit ticket render` leaves, and session end clears them.

#[path = "common/testenv.rs"]
mod testenv;

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use serde_json::{Value, json};

const CONFIG: &str = r#"
[harness.issue_tools.linear]
servers    = ["*linear*"]
tools      = ["save_issue"]
absent     = ["id"]
title      = "title"
body       = "description"
body_patch = ["patch"]

[harness.issue_tools.github]
servers = ["*github*"]
tools   = ["issue_write"]
equals  = { method = "create" }
title   = "title"
body    = "body"

[templates]
issue_body = """
{{ input }}

## Acceptance criteria
{{ acceptance }}
"""

[templates.variables]
acceptance = { required = "agents", description = "observable outcomes that mean the issue is done" }
"#;

const LINEAR: &str = "mcp__claude_ai_Linear__save_issue";

fn project() -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    devkit_git::Git::fixture(p.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    std::fs::write(p.path().join("devkit.toml"), CONFIG).unwrap();
    p
}

fn devkit(project: &Path, state: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.current_dir(project)
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_ENFORCE_WRITES")
        .env_remove("DEVKIT_ENFORCE_COMMANDS");
    testenv::scrub_identity(&mut cmd);
    cmd
}

fn hook(harness: &str, event: &str, project: &Path, payload: &Value) -> Output {
    let state = tempfile::tempdir().unwrap();
    let mut child = devkit(project, state.path())
        .args(["hook", event, "--harness", harness])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the devkit hook");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("hook output");
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    out
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `devkit ticket render` in `project` as an agent whose harness exports
/// `vars`, returning the rendered title and body.
fn render_as(project: &Path, vars: &[(&str, &str)], title: &str, body: &str) -> (String, String) {
    let state = tempfile::tempdir().unwrap();
    let mut cmd = devkit(project, state.path());
    cmd.args(["ticket", "render", "--title", title, "--body", body])
        .args(["--arg", "acceptance=A"])
        .env("DEVKIT_CALLER", "agent");
    for (k, v) in vars {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn devkit ticket render");
    assert!(out.status.success(), "{}", stderr(&out));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    (
        v["title"].as_str().unwrap().to_string(),
        v["body"].as_str().unwrap().to_string(),
    )
}

fn render(project: &Path, session: &str, title: &str, body: &str) -> (String, String) {
    render_as(project, &[("CLAUDE_CODE_SESSION_ID", session)], title, body)
}

fn claude(project: &Path, session: &str, tool: &str, input: Value) -> Value {
    json!({
        "hook_event_name": "PreToolUse",
        "session_id": session,
        "cwd": project,
        "tool_name": tool,
        "tool_input": input,
    })
}

fn pre_tool_use(project: &Path, payload: &Value) -> Output {
    hook("claude-code", "pre-tool-use", project, payload)
}

/// The denial's reason, or `None` for an allow (empty stdout).
fn denial(out: &Output) -> Option<String> {
    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.trim().is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(stdout.trim()).expect("stdout is one JSON envelope");
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny", "{v}");
    Some(
        v["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

fn create(title: &str, body: &str) -> Value {
    json!({"title": title, "description": body, "teamId": "T1"})
}

#[test]
fn an_unrendered_create_is_denied_with_the_render_command() {
    let p = project();
    let out = pre_tool_use(p.path(), &claude(p.path(), "S1", LINEAR, create("T", "B")));
    let reason = denial(&out).expect("denied");
    assert!(
        reason.contains("devkit ticket render") && !reason.contains("issue render"),
        "{reason}"
    );
    assert!(reason.contains("--arg acceptance=..."), "{reason}");
}

#[test]
fn a_rendered_create_is_allowed() {
    let p = project();
    let (t, b) = render(p.path(), "S1", "T", "B");
    let out = pre_tool_use(p.path(), &claude(p.path(), "S1", LINEAR, create(&t, &b)));
    assert_eq!(denial(&out), None);
}

#[test]
fn crlf_and_trailing_spaces_still_match() {
    let p = project();
    let (t, b) = render(p.path(), "S1", "T", "B");
    let b = format!("{}  \r\n", b.replace('\n', " \r\n"));
    let out = pre_tool_use(p.path(), &claude(p.path(), "S1", LINEAR, create(&t, &b)));
    assert_eq!(denial(&out), None);
}

#[test]
fn a_changed_word_is_denied_as_differing() {
    let p = project();
    let (t, b) = render(p.path(), "S1", "T", "B");
    let b = b.replace("Acceptance", "Accepted");
    let out = pre_tool_use(p.path(), &claude(p.path(), "S1", LINEAR, create(&t, &b)));
    let reason = denial(&out).expect("denied");
    assert!(reason.contains("differs"), "{reason}");
}

#[test]
fn another_sessions_receipt_does_not_count() {
    let p = project();
    let (t, b) = render(p.path(), "S1", "T", "B");
    let out = pre_tool_use(p.path(), &claude(p.path(), "S2", LINEAR, create(&t, &b)));
    assert!(denial(&out).is_some());
}

#[test]
fn a_traversal_session_id_is_denied() {
    let p = project();
    let out = pre_tool_use(
        p.path(),
        &claude(p.path(), "../x", LINEAR, create("T", "B")),
    );
    assert!(denial(&out).is_some());
}

#[test]
fn an_update_with_state_only_is_allowed() {
    let p = project();
    let input = json!({"id": "ENG-1", "state": "Done"});
    let out = pre_tool_use(p.path(), &claude(p.path(), "S1", LINEAR, input));
    assert_eq!(denial(&out), None);
}

#[test]
fn a_patch_update_is_denied() {
    let p = project();
    let input = json!({"id": "ENG-1", "patch": [{"op": "replace"}]});
    let out = pre_tool_use(p.path(), &claude(p.path(), "S1", LINEAR, input));
    let reason = denial(&out).expect("denied");
    assert!(reason.contains("patch"), "{reason}");
}

#[test]
fn an_unmatched_mcp_tool_is_silent() {
    let p = project();
    let input = json!({"file": "src/main.rs", "line": 1});
    let out = pre_tool_use(
        p.path(),
        &claude(p.path(), "S1", "mcp__mcpls__get_hover", input),
    );
    assert_eq!(denial(&out), None);
}

#[test]
fn github_create_through_the_hosted_connector_is_gated() {
    let p = project();
    let mut payload = claude(
        p.path(),
        "S1",
        "mcp__claude_ai_GitHub__issue_write",
        json!({"method": "create", "owner": "o", "repo": "r", "title": "T", "body": "B"}),
    );
    payload["mcp_server"] = json!({"name": "claude.ai GitHub"});
    assert!(denial(&pre_tool_use(p.path(), &payload)).is_some());
}

#[test]
fn non_ascii_text_round_trips() {
    let p = project();
    let (t, b) = render(p.path(), "S1", "Fix: café ☕", "naïve 🚀");
    let out = pre_tool_use(p.path(), &claude(p.path(), "S1", LINEAR, create(&t, &b)));
    assert_eq!(denial(&out), None);
}

#[test]
fn a_devkit_file_denies_a_matched_call() {
    let p = project();
    std::fs::write(p.path().join(".devkit"), "").unwrap();
    let out = pre_tool_use(p.path(), &claude(p.path(), "S1", LINEAR, create("T", "B")));
    let reason = denial(&out).expect("denied");
    assert!(reason.contains("is a file"), "{reason}");
}

#[test]
fn a_second_harness_id_is_honoured() {
    let p = project();
    let (t, b) = render_as(
        p.path(),
        &[("CLAUDE_CODE_SESSION_ID", "S1"), ("CODEX_SESSION_ID", "S2")],
        "T",
        "B",
    );
    let out = pre_tool_use(p.path(), &claude(p.path(), "S2", LINEAR, create(&t, &b)));
    assert_eq!(denial(&out), None);
}

#[test]
fn codex_payloads_are_gated_too() {
    let p = project();
    let payload = |t: &str, b: &str| {
        json!({
            "hook_event_name": "PreToolUse",
            "turn_id": "t1",
            "session_id": "S",
            "cwd": p.path(),
            "tool_name": "mcp__linear__save_issue",
            "tool_input": create(t, b),
        })
    };
    let before = hook("codex", "pre-tool-use", p.path(), &payload("T", "B"));
    let reason = denial(&before).expect("denied before the render");
    assert!(
        reason.contains("devkit ticket render") && !reason.contains("issue render"),
        "{reason}"
    );

    let (t, b) = render_as(p.path(), &[("CODEX_SESSION_ID", "S")], "T", "B");
    let after = hook("codex", "pre-tool-use", p.path(), &payload(&t, &b));
    assert_eq!(denial(&after), None);
}

fn session_end(project: &Path, session: &str) -> Output {
    let payload = json!({
        "hook_event_name": "SessionEnd",
        "session_id": session,
        "cwd": project,
    });
    hook("claude-code", "session-end", project, &payload)
}

fn receipts_dir(project: &Path, session: &str) -> std::path::PathBuf {
    project.join(".devkit").join("issue-receipts").join(session)
}

#[test]
fn session_end_clears_only_its_receipts() {
    let p = project();
    render(p.path(), "S1", "T", "B");
    render(p.path(), "S2", "T", "B");
    let out = session_end(p.path(), "S1");
    assert!(out.stdout.is_empty());
    assert!(!receipts_dir(p.path(), "S1").exists());
    assert!(receipts_dir(p.path(), "S2").exists());
}

#[test]
fn session_end_ignores_a_traversal_id() {
    let p = project();
    render(p.path(), "S1", "T", "B");
    let out = session_end(p.path(), "..");
    assert!(out.stdout.is_empty());
    assert!(receipts_dir(p.path(), "S1").exists());
    assert!(p.path().join("devkit.toml").exists());
}

#[cfg(unix)]
#[test]
fn session_end_skips_a_symlinked_receipt_store_and_clears_the_rest() {
    let p = project();
    render(p.path(), "S1", "T", "B");
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir(outside.path().join("S1")).unwrap();
    std::os::unix::fs::symlink(outside.path(), p.path().join(".devkit/pr-receipts")).unwrap();

    let out = session_end(p.path(), "S1");

    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        outside.path().join("S1").exists(),
        "removed through the symlink"
    );
    assert!(!receipts_dir(p.path(), "S1").exists());
}

/// Claude Code's payload `cwd` is where the session started, which need not be
/// the worktree the agent `cd`ed into to render, so every worktree of one
/// repository shares one receipt store.
#[test]
fn a_render_in_a_linked_worktree_counts_at_the_primary() {
    let p = project();
    let git = || devkit_git::Git::fixture(p.path());
    git().args(["add", "-A"]).output().unwrap();
    git().args(["commit", "-q", "-m", "init"]).output().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let wt = parent.path().join("wt");
    git()
        .args(["worktree", "add", "-q", "-b", "feat", &wt.to_string_lossy()])
        .output()
        .unwrap();
    assert!(wt.join("devkit.toml").exists(), "worktree created");

    let (t, b) = render(&wt, "S1", "T", "B");
    let out = pre_tool_use(p.path(), &claude(p.path(), "S1", LINEAR, create(&t, &b)));
    assert_eq!(denial(&out), None);

    let (t, b) = render(p.path(), "S2", "T2", "B2");
    let out = pre_tool_use(&wt, &claude(&wt, "S2", LINEAR, create(&t, &b)));
    assert_eq!(denial(&out), None);
}

#[test]
fn a_create_missing_its_body_is_told_to_add_it() {
    let p = project();
    let (t, _) = render(p.path(), "S1", "T", "B");
    let input = json!({"title": t, "teamId": "T1"});
    let out = pre_tool_use(p.path(), &claude(p.path(), "S1", LINEAR, input));
    let reason = denial(&out).expect("denied");
    assert!(reason.contains("no `description`"), "{reason}");
    assert!(!reason.contains("differs"), "{reason}");
}
