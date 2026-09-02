//! `devrun up` names the configured apps on the two paths that leave the
//! caller without one: an app that is not in the catalog, and a bare `up`
//! whose diff detects nothing. Drives `devkit run` directly (not the `devrun`
//! shim). Both paths fail before anything spawns, so no server is ever
//! started. Uses an isolated HOME/XDG_STATE_HOME so the port registry never
//! touches the real one.

use std::path::Path;
use std::process::Command;

/// A temp dir that is a git repo (`cmd_up` resolves the worktree root) with a
/// devkit.toml defining two apps and no tasks. `baseline_ref` names a ref that
/// does not exist here, so the diff-inference pass finds nothing and the bare
/// `up` reaches the "no apps to run" arm.
fn setup() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let git = |args: &[&str]| {
        devkit_common::git::Git::fixture(root)
            .args(args.iter().copied())
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
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

[apps.web]
base_port = 39240
path = "."
launch = ["git", "version"]

[apps.api]
base_port = 39250
path = "."
launch = ["git", "version"]
"#,
    )
    .expect("write devkit.toml");
    dir
}

fn run_in(dir: &Path, args: &[&str]) -> std::process::Output {
    let state = dir.join("state");
    Command::new(env!("CARGO_BIN_EXE_devkit"))
        .arg("run")
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_STATE_HOME", &state)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("LOCALAPPDATA", &state) // Windows: keep the registry off the real one
        .env("USERPROFILE", dir) // Windows: keep config resolution off the real home
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .output()
        .expect("run devkit run")
}

#[test]
fn an_unknown_app_names_the_configured_ones() {
    let dir = setup();
    let out = run_in(dir.path(), &["up", "nope"]);
    assert!(!out.status.success(), "up with an unknown app should fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("unknown app `nope`"),
        "error should name the app that was not found: {err}"
    );
    assert!(
        err.contains("available apps: api, web"),
        "error should list the configured apps in sorted order: {err}"
    );
}

#[test]
fn a_bare_up_with_nothing_detected_names_the_configured_apps() {
    let dir = setup();
    let out = run_in(dir.path(), &["up"]);
    assert!(!out.status.success(), "bare up with no diff should fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no apps to run"),
        "error should say nothing was resolved: {err}"
    );
    assert!(
        err.contains("available apps: api, web"),
        "error should list the configured apps in sorted order: {err}"
    );
}
