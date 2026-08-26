//! End-to-end coverage of the `lockm` binary: conflict detection, JSON output,
//! release, and exit codes. Each test is isolated via a private temp project
//! (a real git repository) and a private `XDG_STATE_HOME`.

#[path = "common/shimtest.rs"]
mod shimtest;
use std::path::Path;
use std::process::{Command, Output};

fn project() -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    devkit_common::git::Git::fixture(p.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    p
}

fn run(exe: &Path, project: &Path, state: &Path, args: &[&str]) -> Output {
    Command::new(exe)
        .args(args)
        .current_dir(project)
        .env("XDG_STATE_HOME", state)
        // Override HOME too: the binary runs migrate_legacy_state() at startup, which
        // reads $HOME/.claude/state/devkit. Pointing HOME at the throwaway temp dir
        // keeps the test from ever touching the developer's real state home.
        .env("HOME", state)
        .env_remove("DEVKIT_SESSION")
        .env_remove("TMUX_PANE")
        .output()
        .expect("spawn lockm")
}

#[test]
fn second_holder_conflicts_with_overlap() {
    let (_dir, link) = shimtest::linked("lockm");
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    let a = run(
        &link,
        proj.path(),
        state.path(),
        &["acquire", "scenes", "--as", "alice"],
    );
    assert!(a.status.success(), "alice should acquire");

    let b = run(
        &link,
        proj.path(),
        state.path(),
        &["acquire", "scenes/player.tscn", "--as", "bob"],
    );
    assert_eq!(
        b.status.code(),
        Some(1),
        "bob conflicts on an overlapping path"
    );
    let text = String::from_utf8_lossy(&b.stderr);
    assert!(text.contains("alice"), "conflict names the holder: {text}");
}

#[test]
fn json_conflict_shape() {
    let (_dir, link) = shimtest::linked("lockm");
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    run(
        &link,
        proj.path(),
        state.path(),
        &["acquire", "scenes", "--as", "alice"],
    );

    let b = run(
        &link,
        proj.path(),
        state.path(),
        &["check", "scenes/x", "--as", "bob", "--json"],
    );
    assert_eq!(b.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&b.stdout).expect("json on stdout");
    assert_eq!(v["ok"], serde_json::json!(false));
    assert_eq!(v["conflicts"][0]["held_by"], serde_json::json!("alice"));
}

#[test]
fn release_frees_for_other_holder() {
    let (_dir, link) = shimtest::linked("lockm");
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    run(
        &link,
        proj.path(),
        state.path(),
        &["acquire", "scenes", "--as", "alice"],
    );
    let r = run(
        &link,
        proj.path(),
        state.path(),
        &["release", "scenes", "--as", "alice"],
    );
    assert!(r.status.success());

    let b = run(
        &link,
        proj.path(),
        state.path(),
        &["acquire", "scenes", "--as", "bob"],
    );
    assert!(b.status.success(), "bob can acquire after alice releases");
}

#[test]
fn same_holder_reacquire_is_ok() {
    let (_dir, link) = shimtest::linked("lockm");
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    assert!(
        run(
            &link,
            proj.path(),
            state.path(),
            &["acquire", "scenes", "--as", "alice"]
        )
        .status
        .success()
    );
    assert!(
        run(
            &link,
            proj.path(),
            state.path(),
            &["acquire", "scenes", "--as", "alice"]
        )
        .status
        .success()
    );
}

/// Feeds a `pretooluse` hook payload to `lockm hook` over stdin, isolated the
/// same way `run` is: a private `XDG_STATE_HOME`/`HOME` so the harness's
/// global-config read (`$HOME/.config/devkit/config.toml`) finds nothing, and
/// `DEVKIT_ENFORCE_WRITES` stripped so an inherited override cannot decide
/// the result instead of the layer stack.
fn run_hook(cwd: &Path, state: &Path, holder: &str, target: &Path) -> Output {
    use std::io::Write;
    let payload = serde_json::json!({
        "session_id": holder,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Write",
        "tool_input": { "file_path": target.to_string_lossy() },
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_lockm"))
        .args(["hook", "pretooluse"])
        .env("XDG_STATE_HOME", state)
        .env("HOME", state)
        .env_remove("DEVKIT_ENFORCE_WRITES")
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_SESSION")
        .env_remove("TMUX_PANE")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn lockm hook");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    child.wait_with_output().expect("lockm hook output")
}

/// Whether a hook invocation's stdout carries a deny decision. Panics with
/// the exit status and stderr if the process did not exit successfully: a
/// crashed hook must be diagnosed, not misread as "allowed".
fn is_deny(label: &str, out: &Output) -> bool {
    assert!(
        out.status.success(),
        "{label}: lockm hook exited with {:?}; stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stdout = stdout.trim();
    if stdout.is_empty() {
        return false;
    }
    let v: serde_json::Value = serde_json::from_str(stdout).expect("hook stdout is JSON");
    v["hookSpecificOutput"]["permissionDecision"] == "deny"
}

/// A harness declaration below the checkout root must be seen by the deployed
/// hook binary, not only by the library function it calls: the enforcement
/// gate reads `cwd`, not a pre-resolved checkout root, so a directory between
/// the root and the write is part of the answer.
#[test]
fn hook_honors_a_harness_declaration_in_a_nested_directory() {
    let proj = project();
    std::fs::write(proj.path().join("devkit.toml"), "").unwrap();
    let nested = proj.path().join("packages/thing");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(
        nested.join("devkit.local.toml"),
        "[harness]\nenforce_writes = true\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();

    // Enforcement is on at the nested directory: the first write claims the
    // file, the second is denied.
    let target = nested.join("a.rs");
    let first = run_hook(&nested, state.path(), "alice", &target);
    assert!(
        !is_deny("first write (nested)", &first),
        "first write should be allowed: {first:?}"
    );
    let second = run_hook(&nested, state.path(), "bob", &target);
    assert!(
        is_deny("second write (nested)", &second),
        "second write should be denied: {second:?}"
    );

    // Enforcement is off at the checkout root: neither write is denied.
    let other = proj.path().join("b.rs");
    let third = run_hook(proj.path(), state.path(), "carol", &other);
    assert!(
        !is_deny("first write (root)", &third),
        "root has no opt-in: {third:?}"
    );
    let fourth = run_hook(proj.path(), state.path(), "dave", &other);
    assert!(
        !is_deny("second write (root)", &fourth),
        "root has no opt-in: {fourth:?}"
    );
}
