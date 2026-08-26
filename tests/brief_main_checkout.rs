//! `devkit brief` must see the main checkout's config from a linked worktree
//! that carries none of its own — the arrangement `main_checkout` exists to
//! resolve, since the worktree's own upward walk never reaches it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn git(args: &[&str], cwd: &Path) {
    devkit_common::git::Git::fixture(cwd)
        .args(args.iter().copied())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
}

/// A main checkout with a broken `devkit.toml` that exists only there, plus a
/// linked worktree beside it with no config of its own. The file is written
/// after the worktree is created and is never committed, so nothing about
/// `git worktree add` could have copied it into the worktree.
fn project() -> (tempfile::TempDir, PathBuf) {
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

    std::fs::write(main.join("devkit.toml"), "not valid toml [[[").unwrap();

    (t, worktree)
}

fn run_brief(cwd: &Path, state: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_devkit"))
        .arg("brief")
        .current_dir(cwd)
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("XDG_CONFIG_HOME", state.join("config"))
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_SESSION")
        .env_remove("TMUX_PANE")
        .output()
        .unwrap_or_else(|e| panic!("spawn devkit brief: {e}"))
}

/// A config that fails to load in the main checkout is exactly as fatal from
/// a linked worktree carrying none of its own: the worktree inherits the
/// fault, not silence.
#[test]
fn brief_reports_a_fault_that_lives_only_in_the_main_checkout() {
    let (_t, worktree) = project();
    let state = tempfile::tempdir().unwrap();

    let out = run_brief(&worktree, state.path());
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
