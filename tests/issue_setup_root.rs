//! `issue setup` and `issue checkout-pr` operate on the repository's primary
//! checkout, which they resolve through git rather than guessing at a
//! directory name under `worktree_root`. A project whose checkout is named
//! anything else has to work.

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

/// A bare `origin.git` plus a primary checkout at `app/`, deliberately not
/// named `monorepo`. `worktree_root` is repo-relative, so worktrees land under
/// `app/wts`. The guard owns the tree, so callers hold it while reading paths
/// under it.
fn project() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let origin = t.path().join("origin.git");
    std::fs::create_dir_all(&origin).unwrap();
    git(&["init", "-q", "--bare", "-b", "main"], &origin);

    let app = t.path().join("app");
    std::fs::create_dir_all(&app).unwrap();
    git(&["init", "-q", "-b", "main"], &app);
    std::fs::write(app.join("f.txt"), "x\n").unwrap();
    std::fs::write(
        app.join("devkit.toml"),
        r#"
[defaults]
worktree_root = "wts"
branch_prefix = "x/"
baseline_ref = "origin/main"
baseline_path = "b"
"#,
    )
    .unwrap();
    git(&["add", "-A"], &app);
    git(&["commit", "-qm", "init"], &app);
    git(&["remote", "add", "origin", origin.to_str().unwrap()], &app);
    git(&["push", "-q", "origin", "main"], &app);
    t
}

fn run(project: &Path, state: &Path, args: &[&str]) -> Output {
    let (_dir, exe) = shimtest::linked("issue");
    Command::new(&exe)
        .args(args)
        .current_dir(project.join("app"))
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

#[test]
fn setup_resolves_the_primary_checkout_by_git_not_by_name() {
    let t = project();
    let state = tempfile::tempdir().unwrap();
    let out = run(
        t.path(),
        state.path(),
        &["setup", "ENG-1", "--slug", "fix-auth", "--no-gitignore"],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "setup failed in a checkout not named `monorepo`: {stderr}"
    );
    assert!(
        t.path().join("app/wts/fix-auth").is_dir(),
        "worktree should exist under the resolved checkout: {stderr}"
    );
}
