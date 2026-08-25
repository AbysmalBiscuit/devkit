//! End-to-end coverage of the `lockm` binary: conflict detection, JSON output,
//! release, and exit codes. Each test is isolated via a private temp project
//! (with a `.git` marker) and a private `XDG_STATE_HOME`.

use std::path::Path;
use std::process::{Command, Output};

fn project() -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(p.path().join(".git")).unwrap();
    p
}

fn run(project: &Path, state: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lockm"))
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
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    let a = run(
        proj.path(),
        state.path(),
        &["acquire", "scenes", "--as", "alice"],
    );
    assert!(a.status.success(), "alice should acquire");

    let b = run(
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
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    run(
        proj.path(),
        state.path(),
        &["acquire", "scenes", "--as", "alice"],
    );

    let b = run(
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
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    run(
        proj.path(),
        state.path(),
        &["acquire", "scenes", "--as", "alice"],
    );
    let r = run(
        proj.path(),
        state.path(),
        &["release", "scenes", "--as", "alice"],
    );
    assert!(r.status.success());

    let b = run(
        proj.path(),
        state.path(),
        &["acquire", "scenes", "--as", "bob"],
    );
    assert!(b.status.success(), "bob can acquire after alice releases");
}

#[test]
fn same_holder_reacquire_is_ok() {
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    assert!(
        run(
            proj.path(),
            state.path(),
            &["acquire", "scenes", "--as", "alice"]
        )
        .status
        .success()
    );
    assert!(
        run(
            proj.path(),
            state.path(),
            &["acquire", "scenes", "--as", "alice"]
        )
        .status
        .success()
    );
}
