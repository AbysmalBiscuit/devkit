//! `devkit harness shell` with write enforcement: claims, conflicts, policy,
//! and the deadline, through the real binary. Each test has a private
//! project, HOME, and state directory, so no developer config, daemon, or
//! registry is reached.

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

struct Env {
    project: tempfile::TempDir,
    state: tempfile::TempDir,
}

fn env(config: &str) -> Env {
    let project = tempfile::tempdir().unwrap();
    devkit_common::git::Git::fixture(project.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    std::fs::write(project.path().join("devkit.toml"), config).unwrap();
    Env {
        project,
        state: tempfile::tempdir().unwrap(),
    }
}

const WRITES: &str = "[harness]\nenforce_writes = true\n";

fn devkit(e: &Env, args: &[&str], stdin: Option<&str>, extra: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.args(args)
        .current_dir(e.project.path())
        .env("HOME", e.state.path())
        .env("XDG_STATE_HOME", e.state.path())
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_ENFORCE_WRITES")
        .env_remove("DEVKIT_ENFORCE_COMMANDS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let mut pipe = child.stdin.take().unwrap();
    if let Some(s) = stdin {
        pipe.write_all(s.as_bytes()).unwrap();
    }
    drop(pipe);
    child.wait_with_output().unwrap()
}

fn payload(e: &Env, session: Option<&str>, tool: &str, command: &str) -> String {
    let mut p = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "prompt_id": "p",
        "tool_input": { "command": command },
        "cwd": e.project.path().to_string_lossy(),
    });
    if let Some(s) = session {
        p["session_id"] = s.into();
    }
    p.to_string()
}

fn unusable_shell_payload(e: &Env, codex: bool) -> String {
    let mut p = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "prompt_id": "p",
        "session_id": if codex { "C1" } else { "S1" },
        "tool_input": {},
        "cwd": e.project.path().to_string_lossy(),
    });
    if codex {
        p["turn_id"] = "t".into();
        p["model"] = "m".into();
    }
    p.to_string()
}

fn hook(e: &Env, session: Option<&str>, command: &str) -> Output {
    devkit(
        e,
        &["harness", "shell"],
        Some(&payload(e, session, "Bash", command)),
        &[],
    )
}

fn acquire(e: &Env, holder: &str, path: &str) {
    let out = devkit(e, &["locks", "acquire", "--as", holder, path], None, &[]);
    assert!(
        out.status.success(),
        "acquire: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn envelope(out: &Output) -> Option<serde_json::Value> {
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    (!s.trim().is_empty()).then(|| serde_json::from_str(&s).expect("stdout is JSON"))
}

fn denial(out: &Output) -> Option<String> {
    envelope(out).and_then(|v| {
        (v["hookSpecificOutput"]["permissionDecision"] == "deny").then(|| {
            v["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
    })
}

/// `(root-relative path, holder)` for every live row.
fn rows(e: &Env) -> Vec<(String, String)> {
    let file = e.state.path().join("devkit/locks.json");
    let Ok(body) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let mut out: Vec<(String, String)> = v["locks"]
        .as_object()
        .map(|m| {
            m.values()
                .map(|r| {
                    (
                        r["path"].as_str().unwrap().to_string(),
                        r["holder"].as_str().unwrap().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

#[test]
fn a_free_redirect_target_is_claimed_for_the_session() {
    let e = env(WRITES);
    let out = hook(&e, Some("S1"), "printf '%s\\n' 'print(1)' > .temp_demo.py");
    assert_eq!(denial(&out), None);
    assert_eq!(rows(&e), [(".temp_demo.py".to_string(), "S1".to_string())]);
    assert!(
        !e.project.path().join(".temp_demo.py").exists(),
        "the hook never runs the command"
    );
}

#[test]
fn another_sessions_lock_denies() {
    let e = env(WRITES);
    acquire(&e, "S2", "a.txt");
    let reason = denial(&hook(&e, Some("S1"), "echo x > a.txt")).expect("denied");
    assert!(reason.contains("S2"), "{reason}");
    assert_eq!(rows(&e), [("a.txt".to_string(), "S2".to_string())]);
}

#[test]
fn own_and_ancestor_claims_allow() {
    let e = env(WRITES);
    acquire(&e, "S1", "a.txt");
    assert_eq!(denial(&hook(&e, Some("S1"), "echo x > a.txt")), None);
    let sub = devkit(
        &e,
        &["harness", "shell"],
        Some(&{
            let mut p: serde_json::Value =
                serde_json::from_str(&payload(&e, Some("S1"), "Bash", "echo x > a.txt")).unwrap();
            p["agent_id"] = "a1".into();
            p.to_string()
        }),
        &[],
    );
    assert_eq!(denial(&sub), None);
}

#[test]
fn both_ends_of_a_rename_are_checked() {
    let e = env(WRITES);
    acquire(&e, "S2", "b.txt");
    assert!(denial(&hook(&e, Some("S1"), "mv a.txt b.txt")).is_some());
}

#[test]
fn an_outer_redirect_around_a_devkit_command_is_enforced() {
    let e = env("[harness]\nenforce_writes = true\nenforce_commands = true\n");
    acquire(&e, "S2", "shared.txt");
    assert!(denial(&hook(&e, Some("S1"), "devrun task check > shared.txt")).is_some());
}

#[test]
fn an_unresolved_write_blocks_by_default_and_warns_when_configured() {
    let e = env(WRITES);
    let reason = denial(&hook(&e, Some("S1"), "echo x > \"$OUT\"")).expect("denied");
    assert!(reason.contains("literal"), "{reason}");

    let w = env("[harness]\nenforce_writes = true\nunresolved_writes = \"warn\"\n");
    let v = envelope(&hook(&w, Some("S1"), "echo x > \"$OUT\"")).expect("a warning envelope");
    assert!(
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .is_some_and(|s| s.contains("could not be determined")),
        "{v}"
    );
    assert!(
        v["hookSpecificOutput"].get("permissionDecision").is_none(),
        "{v}"
    );
}

#[test]
fn a_warning_cannot_override_a_known_conflict() {
    let e = env("[harness]\nenforce_writes = true\nunresolved_writes = \"warn\"\n");
    acquire(&e, "S2", "a.txt");
    assert!(denial(&hook(&e, Some("S1"), "echo x > a.txt; echo y > \"$OUT\"")).is_some());
}

#[test]
fn an_argv_bound_python_edit_claims_its_target() {
    let e = env(WRITES);
    let command = "f=src/a.ts; python3 - \"$f\" <<'PY'\nimport sys\nfrom pathlib import Path\nPath(sys.argv[1]).write_text('x')\nPY\n";
    assert_eq!(denial(&hook(&e, Some("S1"), command)), None);
    assert_eq!(rows(&e), [("src/a.ts".to_string(), "S1".to_string())]);
}

#[test]
fn read_only_commands_and_quoted_text_claim_nothing() {
    let e = env(WRITES);
    for command in [
        "cat README.md | rg foo",
        "git commit -m \"echo x > y.txt\"",
        "node -e \"console.log(require.resolve('x'))\"",
    ] {
        assert_eq!(denial(&hook(&e, Some("S1"), command)), None, "{command}");
    }
    assert!(rows(&e).is_empty(), "{:?}", rows(&e));
}

#[test]
fn a_tree_writer_is_checked_and_claims_nothing() {
    let e = env(WRITES);
    assert_eq!(denial(&hook(&e, Some("S1"), "cargo fmt")), None);
    assert!(rows(&e).is_empty());
    acquire(&e, "S2", "src/lib.rs");
    assert!(denial(&hook(&e, Some("S1"), "cargo fmt")).is_some());
    assert_eq!(rows(&e), [("src/lib.rs".to_string(), "S2".to_string())]);
}

#[test]
fn unsupported_language_and_script_file_policies() {
    let e = env(WRITES);
    assert!(denial(&hook(&e, Some("S1"), "perl -e 'print 1'")).is_some());
    assert_eq!(denial(&hook(&e, Some("S1"), "python3 tools/gen.py")), None);

    let open = env(
        "[harness]\nenforce_writes = true\nunsupported_language = \"allow\"\nscript_files = \"block\"\n",
    );
    assert_eq!(denial(&hook(&open, Some("S1"), "perl -e 'print 1'")), None);
    assert!(denial(&hook(&open, Some("S1"), "python3 tools/gen.py")).is_some());
}

#[test]
fn a_write_without_a_session_id_is_denied() {
    let e = env(WRITES);
    let reason = denial(&hook(&e, None, "echo x > a.txt")).expect("denied");
    assert!(reason.contains("session_id"), "{reason}");
}

#[test]
fn with_write_enforcement_off_the_hook_writes_no_registry_row() {
    let e = env("[harness]\nenforce_commands = true\n");
    assert_eq!(denial(&hook(&e, Some("S1"), "echo x > a.txt")), None);
    assert!(!e.state.path().join("devkit/locks.json").exists());
}

#[test]
fn a_malformed_rule_does_not_disable_write_enforcement() {
    let e = env(
        "[harness]\nenforce_writes = true\nenforce_commands = true\n[harness.commands.bad]\nprograms = \"git\"\n",
    );
    assert_eq!(denial(&hook(&e, Some("S1"), "echo x > a.txt")), None);
    assert_eq!(rows(&e), [("a.txt".to_string(), "S1".to_string())]);
}

/// An advisory lock on `path`, created if absent. The caller takes the guard
/// from it and holds the guard for as long as the lock should be held.
fn hold(path: &Path) -> fd_lock::RwLock<std::fs::File> {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .unwrap();
    fd_lock::RwLock::new(file)
}

#[test]
fn a_stalled_registry_denies_within_the_deadline() {
    let e = env(WRITES);
    let mut lock = hold(&e.state.path().join("devkit/locks.lock"));
    let _held = lock.write().unwrap();
    let start = Instant::now();
    let reason = denial(&hook(&e, Some("S1"), "echo x > a.txt")).expect("denied");
    assert!(reason.contains("did not answer"), "{reason}");
    assert!(
        start.elapsed() < Duration::from_secs(20),
        "took {:?}",
        start.elapsed()
    );
}

#[test]
fn a_registry_failure_denies() {
    let e = env(WRITES);
    let mut gate = hold(&e.state.path().join("devkit/devkitd.lock"));
    let _held = gate.write().unwrap();
    let reason = denial(&hook(&e, Some("S1"), "echo x > a.txt")).expect("denied");
    assert!(reason.contains("registry error"), "{reason}");
}

#[test]
fn the_powershell_tool_is_read_as_powershell_whatever_the_hook_env_says() {
    let e = env(WRITES);
    let out = devkit(
        &e,
        &["harness", "shell"],
        Some(&payload(
            &e,
            Some("S1"),
            "PowerShell",
            "Set-Content -Path out.txt -Value x",
        )),
        &[("SHELL", "/usr/bin/bash"), ("MSYSTEM", "MINGW64")],
    );
    assert_eq!(denial(&out), None);
    assert_eq!(rows(&e), [("out.txt".to_string(), "S1".to_string())]);
}

#[test]
fn a_codex_payload_claims_through_the_same_path() {
    let e = env(WRITES);
    let p = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "turn_id": "t",
        "model": "m",
        "session_id": "C1",
        "tool_input": { "command": "echo x > c.txt" },
        "cwd": e.project.path().to_string_lossy(),
    });
    assert_eq!(
        denial(&devkit(&e, &["harness", "shell"], Some(&p.to_string()), &[],)),
        None
    );
    assert_eq!(rows(&e), [("c.txt".to_string(), "C1".to_string())]);
}

#[test]
fn a_cursor_payload_never_claims() {
    let e = env(WRITES);
    let p = serde_json::json!({
        "command": "echo x > a.txt",
        "cwd": e.project.path().to_string_lossy(),
        "conversation_id": "x",
    });
    let out = devkit(&e, &["harness", "shell"], Some(&p.to_string()), &[]);
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
    assert!(rows(&e).is_empty());
}

#[test]
fn an_unusable_claude_shell_payload_denies_when_writes_are_enabled() {
    let e = env(WRITES);
    let out = devkit(
        &e,
        &["harness", "shell"],
        Some(&unusable_shell_payload(&e, false)),
        &[],
    );
    let reason = denial(&out).expect("fail-closed denial");
    assert!(
        reason.contains("shell payload") && reason.contains("fail-closed"),
        "{reason}"
    );
}

#[test]
fn an_unusable_codex_shell_payload_denies_when_writes_are_enabled() {
    let e = env(WRITES);
    let out = devkit(
        &e,
        &["harness", "shell"],
        Some(&unusable_shell_payload(&e, true)),
        &[],
    );
    let reason = denial(&out).expect("fail-closed denial");
    assert!(
        reason.contains("shell payload") && reason.contains("fail-closed"),
        "{reason}"
    );
}

#[test]
fn an_unusable_shell_payload_is_silent_when_writes_are_disabled() {
    let e = env("[harness]\n");
    for codex in [false, true] {
        let out = devkit(
            &e,
            &["harness", "shell"],
            Some(&unusable_shell_payload(&e, codex)),
            &[],
        );
        assert_eq!(denial(&out), None, "codex={codex}");
        assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
    }
}

#[test]
fn a_non_shell_claude_payload_stays_silent_when_writes_are_enabled() {
    let e = env(WRITES);
    let p = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "prompt_id": "p",
        "session_id": "S1",
        "tool_input": {},
        "cwd": e.project.path().to_string_lossy(),
    });
    let out = devkit(&e, &["harness", "shell"], Some(&p.to_string()), &[]);
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
    assert!(rows(&e).is_empty());
}
