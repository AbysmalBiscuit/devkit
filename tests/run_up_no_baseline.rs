//! `devkit run up` with explicit apps needs no baseline: apps named on the
//! command line skip diff-detection entirely, so a repository with no
//! `origin` remote and no configured `baseline_ref` must not fail baseline
//! resolution. Drives `devkit run` directly (not the `devrun` shim). Uses an
//! isolated HOME/XDG_STATE_HOME so the port registry never touches the real
//! one.

use std::path::Path;
use std::process::Command;

fn devkit_run() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.arg("run");
    cmd
}

/// A temp dir that is a git repo with no `origin` remote and a devkit.toml
/// that leaves `baseline_ref` unset, defining one app.
fn setup() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    devkit_common::git::Git::fixture(root)
        .args(["init", "-q"])
        .output()
        .unwrap();
    std::fs::write(
        root.join("devkit.toml"),
        r#"
[defaults]
worktree_root = "wts"
branch_prefix = "x/"

[apps.web]
base_port = 39340
path = "."
launch = ["git", "version"]
"#,
    )
    .expect("write devkit.toml");
    dir
}

fn run_in(dir: &Path, args: &[&str]) -> std::process::Output {
    let state = dir.join("state");
    devkit_run()
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
fn naming_apps_explicitly_skips_baseline_resolution() {
    let dir = setup();
    let out = run_in(dir.path(), &["up", "web", "--dry-run"]);
    assert!(
        out.status.success(),
        "explicit apps must not require a resolvable baseline: {out:?}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("web"), "{stdout}");
}
