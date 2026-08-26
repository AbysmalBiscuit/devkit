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

[apps.api]
base_port = 39140
path = "."
launch = ["git", "version"]
static_env = { FROM_APP = "static" }

[tasks.hello]
description = "prints git version"
run = ["git", "version"]

[tasks.show-port]
run = ["git", "--url=http://localhost:{{ ports['api'] }}", "version"]

[tasks.fail]
run = ["git", "definitely-not-a-subcommand"]

[tasks.seq]
steps = [{ up = "api" }, { task = "hello" }]
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
fn task_seq_dry_run_renders_up_step_plan() {
    let dir = setup();
    let out = run_in(dir.path(), &["task", "seq", "--dry-run"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("up api\n") && !stdout.trim_end().ends_with("up api"),
        "up step must render its full plan, not a bare `up api` line: {stdout}"
    );
    // `cmd_up`'s dry-run block is the only place that prints a `log:` line
    // (`run_task_step`'s dry-run branch prints only cwd/argv/env), so its
    // presence proves the up step went through the same rendering as
    // `up --dry-run` rather than the old one-line summary.
    assert!(
        stdout.contains("[issue] api :") && stdout.contains("log:"),
        "up step must render the same [role] app :port / cwd / argv / env / log \
         plan that `up --dry-run` prints: {stdout}"
    );
    assert!(
        stdout.contains("argv: git version"),
        "up step's rendered argv missing: {stdout}"
    );
    assert!(
        !stdout.contains("{{"),
        "no unrendered templates in dry-run: {stdout}"
    );

    // Sanity: dry-run never executes; `run_task_step`'s real-run branch is the
    // only place that prints an exec-marker line.
    assert!(
        !stdout.contains("→ "),
        "dry-run must not execute steps: {stdout}"
    );
}

#[test]
fn task_seq_env_overrides_up_step_static_env() {
    let dir = setup();
    let out = run_in(
        dir.path(),
        &["task", "seq", "--env", "FROM_APP=user", "--dry-run"],
    );
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("FROM_APP=user"),
        "--env override missing from rendered env: {stdout}"
    );
    assert!(
        !stdout.contains("FROM_APP=static"),
        "up step fell back to static_env instead of the --env override: {stdout}"
    );
}

#[test]
fn task_env_file_feeds_steps_and_env_wins() {
    let dir = setup();
    let envfile = dir.path().join("task.env");
    std::fs::write(&envfile, "# comment\nFROM_APP=filed\nEXTRA=1\n").expect("write env file");
    let envfile = envfile.to_string_lossy().into_owned();

    let out = run_in(
        dir.path(),
        &["task", "seq", "--env-file", &envfile, "--dry-run"],
    );
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("FROM_APP=filed") && stdout.contains("EXTRA=1"),
        "--env-file vars missing from rendered env: {stdout}"
    );
    assert!(
        !stdout.contains("FROM_APP=static"),
        "up step fell back to static_env instead of the --env-file value: {stdout}"
    );

    let out = run_in(
        dir.path(),
        &[
            "task",
            "seq",
            "--env-file",
            &envfile,
            "--env",
            "FROM_APP=cli",
            "--dry-run",
        ],
    );
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("FROM_APP=cli") && !stdout.contains("FROM_APP=filed"),
        "--env must win over --env-file, matching `up`: {stdout}"
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
