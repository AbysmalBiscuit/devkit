//! The CLI shapes agents naturally guess must work (or point at the right
//! command): subcommand aliases, a defaulted `--holder`, positional app/issue
//! arguments. Each test isolates state via a private HOME/XDG_STATE_HOME.

use std::path::Path;
use std::process::{Command, Output};

/// A real git repo (so `--show-toplevel` resolves) with a two-app devkit.toml.
fn project() -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    devkit_common::git::Git::fixture(p.path())
        .args(["init", "-q"])
        .output()
        .unwrap();
    std::fs::write(
        p.path().join("devkit.toml"),
        r#"
[defaults]
worktree_root = "wts"
branch_prefix = "x/"
baseline_ref = "origin/main"
baseline_path = "b"

[apps.api]
base_port = 39400
path = "."
launch = ["git", "version"]

[apps.web]
base_port = 39500
path = "."
launch = ["git", "version"]
"#,
    )
    .unwrap();
    p
}

fn run(bin: &str, project: &Path, state: &Path, args: &[&str]) -> Output {
    let exe = match bin {
        "lockm" => env!("CARGO_BIN_EXE_lockm"),
        "portm" => env!("CARGO_BIN_EXE_portm"),
        "issue" => env!("CARGO_BIN_EXE_issue"),
        other => panic!("unknown bin {other}"),
    };
    Command::new(exe)
        .args(args)
        .current_dir(project)
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("XDG_CONFIG_HOME", state.join("config"))
        .env_remove("DEVKIT_SESSION")
        .env_remove("TMUX_PANE")
        .output()
        .unwrap_or_else(|e| panic!("spawn {bin}: {e}"))
}

fn toplevel(project: &Path) -> String {
    devkit_common::git::checkout_root(project)
        .expect("git rev-parse")
        .to_string_lossy()
        .into_owned()
}

// ---- lockm ----

#[test]
fn lockm_list_aliases_status() {
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    let out = run("lockm", proj.path(), state.path(), &["list"]);
    assert!(
        out.status.success(),
        "`lockm list` should work as an alias of `status`: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn lockm_status_with_paths_points_at_check() {
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    let out = run(
        "lockm",
        proj.path(),
        state.path(),
        &["status", "src/some/file.rs"],
    );
    assert!(!out.status.success(), "status with paths is an error");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("lockm check"),
        "error should point at `lockm check`: {err}"
    );
}

// ---- portm ----

#[test]
fn portm_reserve_aliases_alloc() {
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    let out = run("portm", proj.path(), state.path(), &["reserve", "--help"]);
    assert!(
        out.status.success(),
        "`portm reserve` should work as an alias of `alloc`: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn portm_alloc_defaults_holder_to_worktree_root() {
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    let out = run("portm", proj.path(), state.path(), &["alloc", "api"]);
    assert!(
        out.status.success(),
        "alloc without --holder should default to the worktree root: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("api="),
        "prints the reserved port: {stdout}"
    );

    let status = run("portm", proj.path(), state.path(), &["status"]);
    let table = String::from_utf8_lossy(&status.stdout);
    let leaf = Path::new(&toplevel(proj.path()))
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(
        table.contains(&leaf),
        "status shows the worktree root as holder: {table}"
    );
}

#[test]
fn portm_release_positional_apps_frees_only_those() {
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    let alloc = run("portm", proj.path(), state.path(), &["alloc", "api", "web"]);
    assert!(
        alloc.status.success(),
        "alloc: {}",
        String::from_utf8_lossy(&alloc.stderr)
    );

    let rel = run("portm", proj.path(), state.path(), &["release", "api"]);
    assert!(
        rel.status.success(),
        "`portm release api` should release that app's port: {}",
        String::from_utf8_lossy(&rel.stderr)
    );

    let status = run("portm", proj.path(), state.path(), &["status"]);
    let table = String::from_utf8_lossy(&status.stdout);
    assert!(!table.contains("api"), "api's reservation is gone: {table}");
    assert!(table.contains("web"), "web's reservation survives: {table}");
}

#[test]
fn portm_release_without_holder_frees_current_worktree() {
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    run("portm", proj.path(), state.path(), &["alloc", "api"]);

    let rel = run("portm", proj.path(), state.path(), &["release"]);
    assert!(
        rel.status.success(),
        "bare `portm release` should default to the worktree root: {}",
        String::from_utf8_lossy(&rel.stderr)
    );
    let status = run("portm", proj.path(), state.path(), &["status"]);
    let table = String::from_utf8_lossy(&status.stdout);
    assert!(!table.contains("api"), "reservation released: {table}");
}

// ---- issue ----

#[test]
fn issue_setup_accepts_positional_issue_id() {
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    // No full config here — the point is only that clap accepts the shape
    // (a later config/network error exits 1, never clap's usage error 2).
    let out = run(
        "issue",
        proj.path(),
        state.path(),
        &["setup", "ABC-123", "--slug", "s", "--dry-run"],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert_ne!(
        out.status.code(),
        Some(2),
        "positional issue id must parse: {err}"
    );
    assert!(
        !err.contains("unexpected argument"),
        "no clap usage error: {err}"
    );
}

#[test]
fn issue_setup_rejects_both_positional_and_flag() {
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    let out = run(
        "issue",
        proj.path(),
        state.path(),
        &["setup", "ABC-123", "--issue", "ABC-999", "--slug", "s"],
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "conflicting issue ids are a usage error"
    );
}
