//! `devkit brief`, `devkit doctor` and `devkit_ports::load` must all see the
//! main checkout's config from a linked worktree that carries none of its
//! own — the arrangement `main_checkout` exists to resolve, since the
//! worktree's own upward walk never reaches it. Each covers a distinct
//! caller of that threading, over one shared fixture shape.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn git(args: &[&str], cwd: &Path) {
    devkit_common::git::Git::fixture(cwd)
        .args(args.iter().copied())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
}

const DEFAULTS: &str = "[defaults]\n\
                         worktree_root = \"wts\"\n\
                         branch_prefix = \"x/\"\n\
                         baseline_ref = \"origin/main\"\n\
                         baseline_path = \"b\"\n";

/// A main checkout carrying `body` as its `devkit.toml`, plus a linked
/// worktree beside it with no config of its own. The file is written after
/// the worktree is created and is never committed, so nothing about
/// `git worktree add` could have copied it into the worktree.
fn project_with_main_config(body: &str) -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let main = t.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    git(&["init", "-q", "-b", "main"], &main);
    std::fs::write(main.join("f.txt"), "x\n").unwrap();
    git(&["add", "-A"], &main);
    git(&["commit", "-qm", "init"], &main);

    let worktree = t.path().join("wt");
    git(
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            worktree.to_str().unwrap(),
        ],
        &main,
    );

    std::fs::write(main.join("devkit.toml"), body).unwrap();

    (t, worktree)
}

fn devkit(args: &[&str], cwd: &Path, state: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_devkit"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("XDG_CONFIG_HOME", state.join("config"))
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_SESSION")
        .env_remove("TMUX_PANE")
        .output()
        .unwrap_or_else(|e| panic!("spawn devkit {args:?}: {e}"))
}

/// A config that fails to load in the main checkout is exactly as fatal from
/// a linked worktree carrying none of its own: the worktree inherits the
/// fault, not silence.
#[test]
fn brief_reports_a_fault_that_lives_only_in_the_main_checkout() {
    let (_t, worktree) = project_with_main_config("not valid toml [[[");
    let state = tempfile::tempdir().unwrap();

    let out = devkit(&["brief"], &worktree, state.path());
    assert!(
        out.status.success(),
        "devkit brief exited nonzero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("devkit.toml does not load"),
        "the main checkout's broken config should be reported from the worktree: {stdout:?}"
    );
}

/// `devkit doctor` reads the same broken config `devkit brief` does. Reported
/// as a fault (not silence) here too, or the two commands would disagree
/// about the same worktree's config.
#[test]
fn doctor_reports_a_fault_that_lives_only_in_the_main_checkout() {
    let (_t, worktree) = project_with_main_config("not valid toml [[[");
    let state = tempfile::tempdir().unwrap();

    let out = devkit(&["doctor", "--json"], &worktree, state.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("doctor --json: {e}: {stdout}"));
    let config_row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "config")
        .expect("a config row");
    assert_eq!(
        config_row["status"], "invalid",
        "the main checkout's broken config should be reported from the worktree: {config_row:?}"
    );
}

/// A `[brief]` setting declared only in the main checkout still governs a
/// linked worktree's brief: `enabled = false` there must suppress output
/// from the worktree too, even though the worktree's config would otherwise
/// have plenty to report.
#[test]
fn brief_honors_settings_that_live_only_in_the_main_checkout() {
    let body = format!(
        "{DEFAULTS}[brief]\nenabled = false\n[apps.web]\nbase_port = 1\nlaunch = [\"run\"]\npath = \"web\"\n"
    );
    let (_t, worktree) = project_with_main_config(&body);
    let state = tempfile::tempdir().unwrap();

    let out = devkit(&["brief"], &worktree, state.path());
    assert!(
        out.status.success(),
        "devkit brief exited nonzero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.trim().is_empty(),
        "a brief disabled only in the main checkout should stay silent from the worktree: {stdout:?}"
    );
}

/// `devkit_ports::load` (which `devkit brief`'s devrun section is built on)
/// must resolve an app declared only in the main checkout's config from a
/// linked worktree that has none of its own.
#[test]
fn brief_lists_an_app_declared_only_in_the_main_checkout() {
    let body = format!("{DEFAULTS}[apps.web]\nbase_port = 1\nlaunch = [\"run\"]\npath = \"web\"\n");
    let (_t, worktree) = project_with_main_config(&body);
    let state = tempfile::tempdir().unwrap();

    let out = devkit(&["brief"], &worktree, state.path());
    assert!(
        out.status.success(),
        "devkit brief exited nonzero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("web (web)"),
        "an app declared only in the main checkout should be listed from the worktree: {stdout:?}"
    );
}
