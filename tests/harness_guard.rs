//! End-to-end coverage of `devkit harness shell`: the gate, the deny envelopes,
//! and the fail-open contract. Each test runs in a private temp project with a
//! private HOME so it never reads the developer's real global config.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn project(config: &str) -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    devkit_common::git::Git::fixture(p.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    std::fs::write(p.path().join("devkit.toml"), config).unwrap();
    p
}

fn run_hook(project: &Path, home: &Path, payload: &str) -> Output {
    run_hook_with(project, home, payload, &[])
}

/// [`run_hook`], plus environment overrides applied after the private-HOME
/// setup, so a caller can force the enforcement env var one way or the other
/// without losing the isolation every test relies on.
fn run_hook_with(project: &Path, home: &Path, payload: &str, extra_env: &[(&str, &str)]) -> Output {
    let exe = Path::new(env!("CARGO_BIN_EXE_devkit"));
    let mut cmd = Command::new(exe);
    cmd.args(["harness", "shell"])
        .current_dir(project)
        .env("HOME", home)
        .env("XDG_STATE_HOME", home)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_ENFORCE_COMMANDS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    let mut child = cmd.spawn().expect("spawn devkit harness shell");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    child.wait_with_output().expect("hook output")
}

fn claude_payload(command: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command }
    })
    .to_string()
}

fn denied(out: &Output) -> bool {
    assert_eq!(
        out.status.code(),
        Some(0),
        "the guard must always exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.trim().is_empty() {
        return false;
    }
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    v["hookSpecificOutput"]["permissionDecision"] == "deny" || v["permission"] == "deny"
}

const GUARDED: &str = r#"
[harness]
enforce_commands = true

[harness.commands.bun-only]
programs = ["node"]
reason = "This workspace is bun-only."
"#;

const GATE_OFF: &str = r#"
[harness]
enforce_commands = false

[harness.commands.bun-only]
programs = ["node"]
reason = "This workspace is bun-only."
"#;

#[test]
fn a_user_rule_denies_through_the_binary() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(GUARDED);
    let out = run_hook(proj.path(), home.path(), &claude_payload("node server.js"));
    assert!(denied(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("bun-only"),
        "reason reaches the agent: {stdout}"
    );
}

#[test]
fn the_gate_off_allows_everything() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(GATE_OFF);
    let out = run_hook(proj.path(), home.path(), &claude_payload("node server.js"));
    assert!(
        !denied(&out),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The same rule in the same config, with only the env override flipped:
    // proves the assertion above passed because the gate was off, not
    // because the rule never matched anything.
    let out = run_hook_with(
        proj.path(),
        home.path(),
        &claude_payload("node server.js"),
        &[("DEVKIT_ENFORCE_COMMANDS", "1")],
    );
    assert!(
        denied(&out),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_cursor_payload_gets_the_cursor_envelope() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(GUARDED);
    let payload = serde_json::json!({ "command": "node server.js" }).to_string();
    let out = run_hook(proj.path(), home.path(), &payload);
    assert!(denied(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(v["permission"], "deny");
    assert!(v["agent_message"].is_string());
}

#[test]
fn an_unparseable_config_fails_open() {
    // The gate must be forced on: an unparseable layer makes `project_layers`
    // fail, so the gate would read false on its own and this would prove only
    // that the gate-off path works.
    let home = tempfile::tempdir().unwrap();
    let proj = project("[[[ not toml");
    let out = run_hook_with(
        proj.path(),
        home.path(),
        &claude_payload("node server.js"),
        &[("DEVKIT_ENFORCE_COMMANDS", "1")],
    );
    assert!(!denied(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("devkit.toml failed to load"),
        "a broken config must be reported, not silently dropped: {stderr}"
    );
}

#[test]
fn the_env_override_disables_the_guard() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(GUARDED);
    let out = run_hook_with(
        proj.path(),
        home.path(),
        &claude_payload("node server.js"),
        &[("DEVKIT_ENFORCE_COMMANDS", "0")],
    );
    assert!(!denied(&out));
}

#[test]
fn a_garbage_payload_exits_zero_and_says_nothing() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(GUARDED);
    let out = run_hook(proj.path(), home.path(), "not json at all");
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
}

#[test]
fn a_task_denies_through_the_binary_and_writes_no_registry_row() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(&format!(
        "{GUARDED}\n[tasks.check]\nrun = [\"bun\", \"test\"]\napp = \"web\"\n\
         [apps.web]\nbase_port = 3000\npath = \"apps/web\"\nlaunch = [\"vite\"]\n"
    ));
    std::fs::create_dir_all(proj.path().join("apps/web")).unwrap();

    // Assert the denial first: without it, the no-registry-row assertion
    // below would also pass against a project whose config failed to
    // resolve, proving nothing about the guard.
    let out = run_hook(proj.path(), home.path(), &claude_payload("bun test"));
    assert!(
        denied(&out),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("devrun task check"));

    let ports = home.path().join("devkit/ports.json");
    assert!(
        !ports.exists(),
        "the guard allocated a port: {}",
        ports.display()
    );
}

#[test]
fn the_cwd_names_the_app_for_a_catalog_hit() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(&format!(
        "{GUARDED}\n[apps.web]\nbase_port = 3000\npath = \"apps/web\"\n\
         launch = [\"vite\", \"--port\", \"{{{{ port }}}}\"]\n"
    ));
    let app_dir = proj.path().join("apps/web");
    std::fs::create_dir_all(&app_dir).unwrap();
    let out = run_hook(&app_dir, home.path(), &claude_payload("vite"));
    assert!(
        denied(&out),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("devrun up web"));
}
