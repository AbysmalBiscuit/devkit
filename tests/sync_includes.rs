//! `issue sync-includes` pushes the `defaults.worktree_include` files from the
//! monorepo into worktrees that already exist. Each test drives the real binary
//! against a real git repo, with a private HOME/XDG_STATE_HOME.

use std::path::Path;
use std::process::{Command, Output};

fn git(args: &[&str], cwd: &Path) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A monorepo at `main/` with two worktrees beside it, a `devkit.toml` whose
/// `worktree_include` is `include` verbatim, and untracked `.env.local`
/// and `.tool-versions` present only in the monorepo. The guard owns the whole tree, so callers must
/// hold it for as long as they read the paths under it.
fn project(include: &str) -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let main = t.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    git(&["init", "-q", "-b", "main"], &main);
    std::fs::write(main.join("f.txt"), "x\n").unwrap();
    git(&["add", "-A"], &main);
    git(&["commit", "-qm", "init"], &main);
    std::fs::write(
        main.join("devkit.toml"),
        format!(
            r#"
[defaults]
worktree_root = "wts"
branch_prefix = "x/"
baseline_ref = "origin/main"
baseline_path = "b"
worktree_include = [{include}]
"#
        ),
    )
    .unwrap();
    std::fs::write(main.join(".env.local"), "FRESH=1\n").unwrap();
    std::fs::write(main.join(".tool-versions"), "node 20\n").unwrap();
    for (dir, branch) in [("wt-eng-1", "eng-1"), ("wt-eng-2", "eng-2")] {
        let p = t.path().join(dir);
        git(
            &["worktree", "add", "-q", "-b", branch, p.to_str().unwrap()],
            &main,
        );
    }
    t
}

fn run(project: &Path, state: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_issue"))
        .args(args)
        .current_dir(project.join("main"))
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("XDG_CONFIG_HOME", state.join("config"))
        .env_remove("DEVKIT_SESSION")
        .env_remove("TMUX_PANE")
        .output()
        .unwrap_or_else(|e| panic!("spawn issue: {e}"))
}

fn env_local(project: &Path, worktree: &str) -> Option<String> {
    std::fs::read_to_string(project.join(worktree).join(".env.local")).ok()
}

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "sync-includes failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_missing_file_is_copied_into_every_worktree() {
    let t = project("\".env.local\"");
    let state = tempfile::tempdir().unwrap();
    let out = run(t.path(), state.path(), &["sync-includes"]);
    ok(&out);
    assert_eq!(
        env_local(t.path(), "wt-eng-1").as_deref(),
        Some("FRESH=1\n")
    );
    assert_eq!(
        env_local(t.path(), "wt-eng-2").as_deref(),
        Some("FRESH=1\n")
    );
}

#[test]
fn an_existing_file_is_left_alone_and_named_in_a_warning() {
    let t = project("\".env.local\"");
    let state = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("wt-eng-1").join(".env.local"), "OLD=1\n").unwrap();

    let out = run(t.path(), state.path(), &["sync-includes"]);
    ok(&out);
    assert_eq!(env_local(t.path(), "wt-eng-1").as_deref(), Some("OLD=1\n"));
    assert_eq!(
        env_local(t.path(), "wt-eng-2").as_deref(),
        Some("FRESH=1\n")
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains(".env.local") && err.contains("--overwrite"),
        "the untouched file is named, with the way to replace it: {err}"
    );
}

#[test]
fn dry_run_writes_nothing() {
    let t = project("\".env.local\"");
    let state = tempfile::tempdir().unwrap();
    let out = run(t.path(), state.path(), &["sync-includes", "--dry-run"]);
    ok(&out);
    assert_eq!(env_local(t.path(), "wt-eng-1"), None);
    assert_eq!(env_local(t.path(), "wt-eng-2"), None);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(".env.local"),
        "the dry run names what it would copy: {stdout}"
    );
}

#[test]
fn overwrite_with_all_and_yes_replaces_an_existing_file() {
    let t = project("\".env.local\"");
    let state = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("wt-eng-1").join(".env.local"), "OLD=1\n").unwrap();

    let out = run(
        t.path(),
        state.path(),
        &["sync-includes", "--overwrite", "--all", "--yes"],
    );
    ok(&out);
    assert_eq!(
        env_local(t.path(), "wt-eng-1").as_deref(),
        Some("FRESH=1\n")
    );
    assert_eq!(
        env_local(t.path(), "wt-eng-2").as_deref(),
        Some("FRESH=1\n")
    );
}

/// `--overwrite` replaces untracked files git cannot restore, so it may not
/// run against every worktree in the repo by accident.
#[test]
fn overwrite_without_a_scope_is_refused() {
    let t = project("\".env.local\"");
    let state = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("wt-eng-1").join(".env.local"), "OLD=1\n").unwrap();

    let out = run(
        t.path(),
        state.path(),
        &["sync-includes", "--overwrite", "--yes"],
    );
    assert!(!out.status.success(), "an unscoped --overwrite must fail");
    assert_eq!(env_local(t.path(), "wt-eng-1").as_deref(), Some("OLD=1\n"));
    assert_eq!(env_local(t.path(), "wt-eng-2"), None);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--overwrite") && err.contains("--all"),
        "the refusal names both ways to scope the run: {err}"
    );
}

#[test]
fn a_selector_scopes_an_overwrite() {
    let t = project("\".env.local\"");
    let state = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("wt-eng-1").join(".env.local"), "OLD=1\n").unwrap();

    let out = run(
        t.path(),
        state.path(),
        &["sync-includes", "--overwrite", "--yes", "eng-1"],
    );
    ok(&out);
    assert_eq!(
        env_local(t.path(), "wt-eng-1").as_deref(),
        Some("FRESH=1\n")
    );
    assert_eq!(env_local(t.path(), "wt-eng-2"), None);
}

/// Without `--yes` the overwrite asks first, and an empty answer (stdin is
/// closed here, as it is for an agent) is a no.
#[test]
fn overwrite_without_yes_leaves_an_unconfirmed_file_alone() {
    let t = project("\".env.local\"");
    let state = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("wt-eng-1").join(".env.local"), "OLD=1\n").unwrap();

    let out = run(
        t.path(),
        state.path(),
        &["sync-includes", "--overwrite", "--all"],
    );
    ok(&out);
    assert_eq!(env_local(t.path(), "wt-eng-1").as_deref(), Some("OLD=1\n"));
    assert_eq!(
        env_local(t.path(), "wt-eng-2").as_deref(),
        Some("FRESH=1\n")
    );
}

/// Declining the prompt refuses the clobber, not the safe half of the run: the
/// worktree still receives the files it simply does not have.
#[test]
fn declining_the_overwrite_still_copies_what_is_missing() {
    let t = project("\".env.local\", \".tool-versions\"");
    let state = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("wt-eng-1").join(".env.local"), "OLD=1\n").unwrap();

    let out = run(
        t.path(),
        state.path(),
        &["sync-includes", "--overwrite", "eng-1"],
    );
    ok(&out);
    assert_eq!(env_local(t.path(), "wt-eng-1").as_deref(), Some("OLD=1\n"));
    assert_eq!(
        std::fs::read_to_string(t.path().join("wt-eng-1").join(".tool-versions")).ok(),
        Some("node 20\n".to_string())
    );
}

#[test]
fn a_selector_limits_the_run_to_one_worktree() {
    let t = project("\".env.local\"");
    let state = tempfile::tempdir().unwrap();
    let out = run(t.path(), state.path(), &["sync-includes", "eng-1"]);
    ok(&out);
    assert_eq!(
        env_local(t.path(), "wt-eng-1").as_deref(),
        Some("FRESH=1\n")
    );
    assert_eq!(env_local(t.path(), "wt-eng-2"), None);
}

#[test]
fn an_empty_include_list_exits_zero_without_copying() {
    let t = project("");
    let state = tempfile::tempdir().unwrap();
    let out = run(t.path(), state.path(), &["sync-includes"]);
    ok(&out);
    assert_eq!(env_local(t.path(), "wt-eng-1"), None);
    assert_eq!(env_local(t.path(), "wt-eng-2"), None);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("worktree_include"),
        "an empty include list says so: {stdout}"
    );
}
