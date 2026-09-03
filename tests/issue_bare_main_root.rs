//! `defaults.worktree_root` is derived from the repository's non-bare main
//! worktree, and a bare main worktree has none. Joining a directory name onto
//! the empty value gives a bare relative path, and `git -C <primary> worktree
//! add` resolves that inside the primary checkout — placing the worktree and
//! its branch there. Both placement paths refuse instead, and the explicit
//! `--worktree-path` stays available as the way to work in such a project.

#[path = "common/shimtest.rs"]
mod shimtest;

use std::path::Path;
use std::process::{Command, Output};

fn git(args: &[&str], cwd: &Path) {
    devkit_common::git::Git::fixture(cwd)
        .args(args.iter().copied())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
}

/// A bare repository plus a linked worktree of it, with a `devkit.toml` that
/// names no `worktree_root`. The guard owns the tree, so callers hold it while
/// reading paths under it.
fn project() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let seed = t.path().join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&["init", "-q", "-b", "main"], &seed);
    std::fs::write(
        seed.join("devkit.toml"),
        "[defaults]\nbranch_prefix = \"x/\"\nbaseline_ref = \"main\"\n",
    )
    .unwrap();
    git(&["add", "-A"], &seed);
    git(&["commit", "-qm", "init"], &seed);

    let bare = t.path().join("origin.git");
    git(
        &[
            "clone",
            "-q",
            "--bare",
            seed.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
        t.path(),
    );
    let wt = t.path().join("wt");
    git(
        &["worktree", "add", "-q", wt.to_str().unwrap(), "main"],
        &bare,
    );
    t
}

fn run(project: &Path, state: &Path, args: &[&str]) -> Output {
    let (_dir, exe) = shimtest::linked("issue");
    Command::new(&exe)
        .args(args)
        .current_dir(project.join("wt"))
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("XDG_CONFIG_HOME", state.join("config"))
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_SESSION")
        .env_remove("TMUX_PANE")
        .env_remove("LINEAR_API_KEY")
        .output()
        .unwrap_or_else(|e| panic!("spawn issue: {e}"))
}

/// The refusal comes before the plan: a dry run that printed a placement it
/// would refuse to carry out would read as a working configuration.
#[test]
fn setup_refuses_a_placement_it_cannot_derive() {
    let t = project();
    let state = tempfile::tempdir().unwrap();
    let out = run(
        t.path(),
        state.path(),
        &["setup", "ENG-1", "--slug", "demo", "--dry-run"],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "setup should refuse: {stderr}");
    assert!(
        stderr.contains("defaults.worktree_root"),
        "the refusal must name the key: {stderr}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "a refused setup printed a plan"
    );
}

/// An explicit `<WORKTREE_PATH>` places the worktree itself, so it needs no
/// root to derive.
/// The run still fails — there is no PR to resolve here — but never on the
/// root, which is what keeps the workaround available.
#[test]
fn checkout_with_an_explicit_path_needs_no_root() {
    let t = project();
    let state = tempfile::tempdir().unwrap();
    let target = t.path().join("elsewhere");
    let out = run(
        t.path(),
        state.path(),
        &["checkout-pr", "1", target.to_str().unwrap()],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("defaults.worktree_root"),
        "an explicit path was refused over a root it does not use: {stderr}"
    );
}
