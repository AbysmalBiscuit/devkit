//! `devrun task` end-to-end: listing, dry-run rendering with a registry-
//! allocated port, execution, and exit-code propagation. Uses an isolated
//! HOME/XDG_STATE_HOME so the port registry never touches the real one.

use std::path::Path;
use std::process::Command;

fn devrun() -> Command {
    Command::new(env!("CARGO_BIN_EXE_devrun"))
}

/// A temp dir that is a git repo (cmd_task resolves the worktree root) with a
/// devkit.toml defining one app and three tasks.
fn setup() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .expect("git")
            .success();
        assert!(ok, "git {args:?} failed");
    };
    git(&["init", "-q"]);
    std::fs::write(
        root.join("devkit.toml"),
        r#"
[defaults]
worktree_root = "wts"
branch_prefix = "x/"
baseline_ref = "origin/main"
baseline_path = "b"

[apps.api]
base_port = 39140
path = "."
launch = ["git", "version"]

[tasks.hello]
description = "prints git version"
run = ["git", "version"]

[tasks.show-port]
run = ["git", "--url=http://localhost:{{ ports['api'] }}", "version"]

[tasks.fail]
run = ["git", "definitely-not-a-subcommand"]
"#,
    )
    .expect("write devkit.toml");
    dir
}

fn run_in(dir: &Path, args: &[&str]) -> std::process::Output {
    let state = dir.join("state");
    devrun()
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_STATE_HOME", &state)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("LOCALAPPDATA", &state) // Windows: keep the registry off the real one
        .env("USERPROFILE", dir) // Windows: keep config resolution off the real home
        .output()
        .expect("run devrun")
}

#[test]
fn task_lists_names_and_descriptions() {
    let dir = setup();
    let out = run_in(dir.path(), &["task"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("hello"),
        "listing missing task name: {stdout}"
    );
    assert!(
        stdout.contains("prints git version"),
        "listing missing description: {stdout}"
    );
}

#[test]
fn task_dry_run_renders_allocated_port() {
    let dir = setup();
    let out = run_in(dir.path(), &["task", "show-port", "--dry-run"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("http://localhost:391"),
        "dry-run must show a rendered port at/above base 39140: {stdout}"
    );
    assert!(
        !stdout.contains("{{"),
        "no unrendered templates in dry-run: {stdout}"
    );
}

#[test]
fn task_runs_and_propagates_exit_codes() {
    let dir = setup();
    let ok = run_in(dir.path(), &["task", "hello"]);
    assert!(ok.status.success(), "{ok:?}");

    let bad = run_in(dir.path(), &["task", "fail"]);
    assert!(!bad.status.success(), "failing task must exit non-zero");

    let missing = run_in(dir.path(), &["task", "nope"]);
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("unknown task"),
        "{missing:?}"
    );
}
