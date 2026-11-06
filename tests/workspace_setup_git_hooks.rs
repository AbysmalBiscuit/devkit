//! `workspace setup --no-git-hooks` keeps the repository's hooks out of its
//! `git worktree add`, and out of nothing else.

#[path = "common/shimtest.rs"]
mod shimtest;
#[path = "common/testenv.rs"]
mod testenv;

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

const CONFIG: &str = r#"
[defaults]
worktree_root = "wts"
branch_prefix = "x/"
baseline_ref = "origin/main"

[hooks]
after_worktree_create = [["git", "commit", "--allow-empty", "-qm", "from the create hook"]]
"#;

const AUTHOR: &str = "devkit test";
const EMAIL: &str = "test@example.invalid";

fn git(dir: &Path, args: &[&str]) {
    devkit_git::Git::fixture(dir)
        .args(args.iter().copied())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e:#}"));
}

/// Install a hook that appends a line to `<hook>-fired` in the common git
/// directory, which every worktree of the repository shares.
fn install_hook(project: &Path, hook: &str) {
    let path = project.join(".git/hooks").join(hook);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        format!("#!/bin/sh\necho fired >> \"$(git rev-parse --git-common-dir)/{hook}-fired\"\n"),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn times_fired(project: &Path, hook: &str) -> usize {
    std::fs::read_to_string(project.join(".git").join(format!("{hook}-fired")))
        .map_or(0, |fired| fired.lines().count())
}

/// A clone with an `origin` to fetch and the two hooks installed. The guard
/// owns the tree.
fn project() -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let seed = t.path().join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"]);
    std::fs::write(seed.join("devkit.toml"), CONFIG).unwrap();
    git(&seed, &["add", "-A"]);
    git(&seed, &["commit", "-qm", "init"]);
    let project = t.path().join("project");
    git(t.path(), &[
        "clone",
        "-q",
        seed.to_str().unwrap(),
        project.to_str().unwrap(),
    ]);
    install_hook(&project, "post-checkout");
    install_hook(&project, "post-commit");
    (t, project)
}

fn setup(project: &Path, flags: &[&str]) -> Output {
    let state = tempfile::tempdir().unwrap();
    let (_dir, exe) = shimtest::linked("workspace");
    let mut cmd = Command::new(&exe);
    cmd.arg("setup")
        .args(flags)
        .current_dir(project)
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .env("XDG_CONFIG_HOME", state.path().join("config"))
        .env("GIT_CONFIG_GLOBAL", state.path().join("no-gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", AUTHOR)
        .env("GIT_AUTHOR_EMAIL", EMAIL)
        .env("GIT_COMMITTER_NAME", AUTHOR)
        .env("GIT_COMMITTER_EMAIL", EMAIL)
        .env("DEVKIT_SKIP_AUTOLINK", "1");
    testenv::scrub_identity(&mut cmd);
    cmd.output().unwrap_or_else(|e| panic!("spawn setup: {e}"))
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The commit in `after_worktree_create` and the one made after setup returns
/// each fire `post-commit`, so the skip ends with the `git worktree add`.
#[test]
fn no_git_hooks_skips_post_checkout_and_keeps_later_hooks() {
    let (_t, project) = project();

    let out = setup(&project, &[
        "--slug",
        "demo",
        "--no-gitignore",
        "--no-git-hooks",
    ]);

    assert!(out.status.success(), "{}", stderr(&out));
    let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let worktree = Path::new(result["worktree"].as_str().unwrap());
    assert!(
        worktree.join("devkit.toml").exists(),
        "worktree is populated"
    );
    assert_eq!(times_fired(&project, "post-checkout"), 0);
    assert_eq!(
        times_fired(&project, "post-commit"),
        1,
        "the after_worktree_create commit fires post-commit"
    );

    git(worktree, &["commit", "--allow-empty", "-qm", "later"]);

    assert_eq!(times_fired(&project, "post-commit"), 2);
}

#[test]
fn setup_without_the_flag_fires_post_checkout() {
    let (_t, project) = project();

    let out = setup(&project, &["--slug", "demo", "--no-gitignore"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(times_fired(&project, "post-checkout"), 1);
}

#[test]
fn no_git_hooks_is_refused_with_here_which_creates_no_worktree() {
    let (_t, project) = project();

    let out = setup(&project, &["--here", "7", "--no-git-hooks"]);

    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("cannot be used with"),
        "{}",
        stderr(&out)
    );
}
