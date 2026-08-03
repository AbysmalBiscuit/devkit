//! `devrun up` surfaces an app's configured URL template: the summary table's URL
//! column and the `url_env` wired into consumers. Uses an isolated
//! HOME/XDG_STATE_HOME so the port registry never touches the real one.

use std::path::Path;
use std::process::Command;

fn devrun() -> Command {
    Command::new(env!("CARGO_BIN_EXE_devrun"))
}

/// A temp dir that is a git repo with a devkit.toml defining a URL provider with
/// a custom `url`, a consumer on the default `url`, and an app whose `url`
/// references another app's port.
fn setup() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let ok = Command::new("git")
        .args(["init", "-q"])
        .current_dir(root)
        .status()
        .expect("git")
        .success();
    assert!(ok, "git init failed");
    std::fs::write(
        root.join("devkit.toml"),
        r#"
[defaults]
worktree_root = "wts"
branch_prefix = "x/"
baseline_ref = "origin/main"
baseline_path = "b"

[apps.front]
base_port = 39240
path = "."
url = "https://app.localhost:{{ port }}/dashboard"
provides_url = true
launch = ["git", "version"]

[apps.web]
base_port = 39260
path = "."
url_env = "FRONT_URL"
launch = ["git", "version"]

[apps.peer]
base_port = 39280
path = "."
url = "https://localhost:{{ ports['front'] }}/peer"
launch = ["git", "version"]
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
fn up_summary_renders_the_configured_url_per_app() {
    let dir = setup();
    let out = run_in(dir.path(), &["up", "front", "web", "--dry-run"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("https://app.localhost:39240/dashboard"),
        "custom url not rendered: {stdout}"
    );
    assert!(
        stdout.contains("http://localhost:39260"),
        "app without a url should keep the http://localhost default: {stdout}"
    );
}

#[test]
fn a_url_may_reference_another_apps_port() {
    let dir = setup();
    let out = run_in(dir.path(), &["up", "peer", "--dry-run"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("https://localhost:39240/peer"),
        "cross-app port reference not resolved in url: {stdout}"
    );
}

#[test]
fn url_env_wiring_uses_the_providers_url() {
    let dir = setup();
    let out = run_in(dir.path(), &["up", "web", "--dry-run"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("FRONT_URL=https://app.localhost:39240/dashboard"),
        "consumer wired to something other than the provider's url: {stdout}"
    );
}
