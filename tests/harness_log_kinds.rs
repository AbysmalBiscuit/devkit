//! Every verb beyond `pre-tool-use`: the kind it records, and the silence it
//! keeps on stdout.
//!
//! That silence is a requirement rather than an observation. `UserPromptSubmit`
//! appends a hook's plain stdout to the prompt on Claude Code and Codex parses
//! it, `Stop` and `PermissionRequest` honour a JSON decision, and Cursor's
//! `beforeSubmitPrompt` reads a `continue` field — so a stray `println!` in a
//! record-only verb would inject text into a prompt or veto a turn.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

struct Env {
    project: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Env {
    fn log_dir(&self) -> PathBuf {
        self.home.path().join("logs")
    }
}

fn env_with(global_extra: &str) -> Env {
    let project = tempfile::tempdir().unwrap();
    devkit_common::git::Git::fixture(project.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    std::fs::write(project.path().join("devkit.toml"), "[harness]\n").unwrap();

    let home = tempfile::tempdir().unwrap();
    let cfg = home.path().join(".config/devkit");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("config.toml"),
        format!(
            "[harness.log]\nenabled = true\ndir = {:?}\n{global_extra}",
            home.path().join("logs").to_string_lossy()
        ),
    )
    .unwrap();
    Env { project, home }
}

fn enabled_project() -> Env {
    env_with("")
}

fn run_argv(e: &Env, argv: &[&str], payload: &str) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.args(argv)
        .current_dir(e.project.path())
        .env("HOME", e.home.path())
        .env("XDG_STATE_HOME", e.home.path())
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_HARNESS_LOG")
        .env_remove("DEVKIT_HARNESS_LOG_FAULT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn the devkit hook");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    child.wait_with_output().expect("hook output")
}

fn records(dir: &Path) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let Ok(days) = std::fs::read_dir(dir) else {
        return out;
    };
    for day in days.filter_map(Result::ok).map(|e| e.path()) {
        if !day.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(&day).unwrap().filter_map(Result::ok) {
            for line in std::fs::read_to_string(f.path()).unwrap().lines() {
                out.push(serde_json::from_str(line).expect("a record must parse"));
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

fn clear_records(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

fn payload_for(e: &Env, verb: &str) -> String {
    let mut p = serde_json::json!({
        "hook_event_name": verb,
        "session_id": "s1",
        "cwd": e.project.path().to_string_lossy(),
    });
    match verb {
        "post-tool-use" | "post-tool-use-failure" => {
            p["tool_name"] = "Bash".into();
            p["tool_use_id"] = "u1".into();
            p["tool_response"] = serde_json::json!({ "stdout": "hello", "exit_code": 0 });
        }
        "permission-request" | "permission-denied" => {
            p["tool_name"] = "Bash".into();
            p["reason"] = "not on the allowlist".into();
        }
        "worktree-create" | "worktree-remove" => {
            p["worktree_path"] = "/tmp/wt".into();
        }
        "user-prompt-submit" => {
            p["prompt"] = "my secret plan".into();
        }
        "subagent-start" | "subagent-stop" => {
            p["agent_id"] = "a1".into();
        }
        _ => {}
    }
    p.to_string()
}

#[test]
fn each_verb_writes_the_kind_its_table_row_names() {
    let e = enabled_project();
    for (verb, kind) in [
        ("post-tool-use", "shell_post"),
        ("post-tool-use-failure", "shell_post"),
        ("session-start", "session"),
        ("session-end", "session"),
        ("subagent-start", "session"),
        ("subagent-stop", "session"),
        ("permission-request", "permission"),
        ("permission-denied", "permission"),
        ("stop", "lifecycle"),
        ("stop-failure", "lifecycle"),
        ("pre-compact", "lifecycle"),
        ("post-compact", "lifecycle"),
        ("cwd-changed", "lifecycle"),
        ("worktree-create", "worktree"),
        ("worktree-remove", "worktree"),
        ("user-prompt-submit", "prompt"),
    ] {
        clear_records(&e.log_dir());
        let out = run_argv(&e, &["hook", verb], &payload_for(&e, verb));
        assert!(
            out.stdout.is_empty(),
            "{verb} must be silent on stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert_eq!(out.status.code(), Some(0), "{verb}");
        let rec = sole_record(&e.log_dir());
        assert_eq!(rec["kind"], kind, "{verb}");
        assert_eq!(rec["event"], verb, "{verb}");
        assert_eq!(rec["vendor_event"], verb, "{verb}: the payload's own name");
    }
}

#[test]
fn a_session_frame_names_which_end_it_is_and_whose() {
    let e = enabled_project();
    for (verb, end, subagent) in [
        ("session-start", "start", false),
        ("session-end", "end", false),
        ("subagent-start", "start", true),
        ("subagent-stop", "end", true),
    ] {
        clear_records(&e.log_dir());
        run_argv(&e, &["hook", verb], &payload_for(&e, verb));
        let rec = sole_record(&e.log_dir());
        assert_eq!(rec["end"], end, "{verb}");
        assert_eq!(rec["subagent"], subagent, "{verb}");
    }
}

#[test]
fn a_permission_record_separates_an_ask_from_a_block() {
    let e = enabled_project();
    run_argv(
        &e,
        &["hook", "permission-request"],
        &payload_for(&e, "permission-request"),
    );
    assert_eq!(sole_record(&e.log_dir())["blocked"], false);

    clear_records(&e.log_dir());
    run_argv(
        &e,
        &["hook", "permission-denied"],
        &payload_for(&e, "permission-denied"),
    );
    let rec = sole_record(&e.log_dir());
    assert_eq!(rec["blocked"], true);
    assert_eq!(rec["detail"], "not on the allowlist");
}

#[test]
fn a_worktree_record_names_which_way_it_moved() {
    let e = enabled_project();
    run_argv(
        &e,
        &["hook", "worktree-remove"],
        &payload_for(&e, "worktree-remove"),
    );
    let rec = sole_record(&e.log_dir());
    assert_eq!(rec["change"], "remove");
    assert_eq!(rec["path"], "/tmp/wt");
}

#[test]
fn a_prompt_is_not_recorded_at_the_default_fidelity() {
    let e = enabled_project(); // prompt defaults to "off"
    run_argv(
        &e,
        &["hook", "user-prompt-submit"],
        &payload_for(&e, "user-prompt-submit"),
    );
    let rec = sole_record(&e.log_dir());
    assert_eq!(rec["kind"], "prompt");
    assert!(
        rec["text"].is_null(),
        "off records that a prompt happened, not its text"
    );
    assert_eq!(rec["chars"], 14, "the shape survives, the content does not");
}

#[test]
fn a_prompt_is_recorded_when_the_global_config_asks_for_it() {
    let e = env_with("prompt = \"full\"\n");
    run_argv(
        &e,
        &["hook", "user-prompt-submit"],
        &payload_for(&e, "user-prompt-submit"),
    );
    assert_eq!(sole_record(&e.log_dir())["text"], "my secret plan");
}

/// Codex's post payload exposes `tool_response` as output text rather than a
/// structured result, so no exit code reaches the hook. Byte length still does.
#[test]
fn a_shell_post_carries_absent_rather_than_zero_for_what_codex_omits() {
    let e = enabled_project();
    let codex = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "s1",
        "turn_id": "t1",
        "tool_use_id": "u1",
        "tool_name": "Bash",
        "cwd": e.project.path().to_string_lossy(),
        "tool_response": "some output text",
    })
    .to_string();
    run_argv(&e, &["hook", "post-tool-use"], &codex);
    let rec = sole_record(&e.log_dir());
    assert!(rec["exit_code"].is_null(), "Codex sends no exit code");
    assert_eq!(rec["stdout_bytes"], 16);
    assert_eq!(rec["tool_use_id"], "u1", "so a pre record can join to it");
}

#[test]
fn a_claude_code_post_carries_the_exit_code_it_sends() {
    let e = enabled_project();
    run_argv(
        &e,
        &["hook", "post-tool-use"],
        &payload_for(&e, "post-tool-use"),
    );
    let rec = sole_record(&e.log_dir());
    assert_eq!(rec["exit_code"], 0);
    assert_eq!(rec["stdout_bytes"], 5);
}

/// The failure verb is an error whatever the payload says: it is the vendor
/// event that carries that fact, not the response body.
#[test]
fn the_failure_verb_records_an_error() {
    let e = enabled_project();
    run_argv(
        &e,
        &["hook", "post-tool-use-failure"],
        &payload_for(&e, "post-tool-use-failure"),
    );
    assert_eq!(sole_record(&e.log_dir())["error"], true);
}

#[test]
fn nothing_is_recorded_with_logging_off() {
    let e = enabled_project();
    std::fs::remove_file(e.home.path().join(".config/devkit/config.toml")).unwrap();
    for verb in ["stop", "session-start", "user-prompt-submit"] {
        let out = run_argv(&e, &["hook", verb], &payload_for(&e, verb));
        assert_eq!(out.status.code(), Some(0), "{verb}");
        assert!(out.stdout.is_empty(), "{verb}");
    }
    assert!(records(&e.log_dir()).is_empty());
}

/// A verb a harness fires with nothing on stdin still records that it fired.
#[test]
fn a_verb_with_no_payload_still_lands_a_record() {
    let e = enabled_project();
    let out = run_argv(&e, &["hook", "stop"], "");
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
    let rec = sole_record(&e.log_dir());
    assert_eq!(rec["kind"], "lifecycle");
    assert!(rec["session_id"].is_null());
    assert!(rec["vendor_event"].is_null());
}

/// `session-end` releases, records and sweeps, in that order: release first
/// because it is the one step with a correctness consequence, the sweep last
/// because it is the only one that can be skipped without loss.
#[test]
fn session_end_sweeps_after_it_records() {
    let e = env_with("max_age_days = 30\n");
    let stale = e.log_dir().join("2020-01-01");
    std::fs::create_dir_all(&stale).unwrap();
    let old = stale.join("gone.jsonl");
    std::fs::write(&old, "{}\n").unwrap();
    let f = std::fs::File::options().write(true).open(&old).unwrap();
    f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(400 * 86_400))
        .unwrap();
    drop(f);

    let out = run_argv(
        &e,
        &["hook", "session-end"],
        &payload_for(&e, "session-end"),
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty(), "a release event emits no decision");
    assert!(!old.exists(), "the sweep ran");
    assert_eq!(
        sole_record(&e.log_dir())["kind"],
        "session",
        "and its own record survived it"
    );
}

/// `auto_prune = false` leaves the sweep to `devkit hook-log prune`.
#[test]
fn session_end_does_not_sweep_when_auto_prune_is_off() {
    let e = env_with("max_age_days = 30\nauto_prune = false\n");
    let stale = e.log_dir().join("2020-01-01");
    std::fs::create_dir_all(&stale).unwrap();
    let old = stale.join("kept.jsonl");
    std::fs::write(&old, "{}\n").unwrap();
    let f = std::fs::File::options().write(true).open(&old).unwrap();
    f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(400 * 86_400))
        .unwrap();
    drop(f);

    run_argv(
        &e,
        &["hook", "session-end"],
        &payload_for(&e, "session-end"),
    );
    assert!(old.exists());
}

#[test]
fn hook_log_path_prints_the_resolved_directory() {
    let e = enabled_project();
    let out = run_argv(&e, &["hook-log", "path"], "");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        e.log_dir().to_string_lossy()
    );
}

#[test]
fn hook_log_prune_says_so_when_no_cap_is_set() {
    let e = enabled_project();
    let out = run_argv(&e, &["hook-log", "prune"], "");
    assert_eq!(out.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("no retention caps"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn hook_log_prune_reports_what_it_removed() {
    let e = env_with("max_age_days = 30\n");
    let stale = e.log_dir().join("2020-01-01");
    std::fs::create_dir_all(&stale).unwrap();
    let old = stale.join("gone.jsonl");
    std::fs::write(&old, "{}\n").unwrap();
    let f = std::fs::File::options().write(true).open(&old).unwrap();
    f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(400 * 86_400))
        .unwrap();
    drop(f);

    let out = run_argv(&e, &["hook-log", "prune"], "");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("removed 1 file"), "{text}");
    assert!(!old.exists());
}
