//! `issue sync-includes` pushes the `defaults.worktree_include` files from the
//! monorepo into worktrees that already exist. Each test drives an `issue` shim
//! (a hardlinked `devkit`) against a real git repo, with a private
//! HOME/XDG_STATE_HOME.

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

/// A monorepo at `main/` with two worktrees beside it, a committed `devkit.toml`
/// whose `worktree_include` is `include` verbatim, and untracked `.env.local`
/// and `.tool-versions` files present only in the monorepo. The guard owns the
/// whole tree, so callers must hold it for as long as they read the paths under
/// it.
fn project(include: &str) -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let main = t.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    git(&["init", "-q", "-b", "main"], &main);
    std::fs::write(main.join("f.txt"), "x\n").unwrap();
    std::fs::write(
        main.join("devkit.toml"),
        format!(
            r#"
[defaults]
worktree_root = "wts"
branch_prefix = "x/"
baseline_ref = "origin/main"
worktree_include = [{include}]
"#
        ),
    )
    .unwrap();
    // Committed, so a run started from a worktree finds the same config.
    git(&["add", "-A"], &main);
    git(&["commit", "-qm", "init"], &main);
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
    run_in(project, "main", state, args)
}

/// Drive the binary from `cwd`, a directory name under the project tree, so a
/// test can run from a worktree rather than from the monorepo.
fn run_in(project: &Path, cwd: &str, state: &Path, args: &[&str]) -> Output {
    // The link's guard lives to the end of this function, which is safe because
    // `output()` waits for the child before returning.
    let (_dir, exe) = shimtest::linked("issue");
    Command::new(&exe)
        .args(args)
        .current_dir(project.join(cwd))
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("XDG_CONFIG_HOME", state.join("config"))
        .env("DEVKIT_SKIP_AUTOLINK", "1")
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

/// A symlink whose destination is already occupied is never counted as
/// created and never routed through the file-listing warning (the planning
/// walk keeps a matched symlink out of `existing`), so it needs a report of
/// its own.
#[test]
fn an_existing_symlink_destination_is_left_alone_and_reported() {
    let t = project("\"inc/\"");
    let main = t.path().join("main");
    std::fs::create_dir_all(main.join("inc")).unwrap();
    std::fs::write(main.join("real.txt"), "content").unwrap();
    let target = Path::new("..").join("real.txt");
    if let Err(e) = devkit_common::sys::symlink(&target, &main.join("inc/link.txt"), false) {
        eprintln!("skipping: this platform refuses symlink creation ({e})");
        return;
    }
    let wt1_inc = t.path().join("wt-eng-1").join("inc");
    std::fs::create_dir_all(&wt1_inc).unwrap();
    std::fs::write(wt1_inc.join("link.txt"), "already here").unwrap();

    let state = tempfile::tempdir().unwrap();
    let out = run(t.path(), state.path(), &["sync-includes"]);
    ok(&out);
    assert_eq!(
        std::fs::read_to_string(wt1_inc.join("link.txt")).unwrap(),
        "already here",
        "the occupied destination was not replaced"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    let rel = Path::new("inc").join("link.txt").display().to_string();
    let target_str = target.display().to_string();
    assert!(
        err.contains("already in") && err.contains(&rel) && err.contains(&target_str),
        "the skip names the link and its target: {err}"
    );
    assert!(
        !err.contains("warning: linking"),
        "a skip is not reported as a failed link creation: {err}"
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

/// A dry run partitions matched links by whether their destination is
/// already occupied, the same test `sync-includes` makes for real. Inverting
/// that predicate would silently swap which links land under "would link"
/// and which under the leave-alone block, and nothing else in the suite
/// notices.
#[test]
fn dry_run_partitions_links_by_occupied_destination() {
    let t = project("\"inc/\"");
    let main = t.path().join("main");
    std::fs::create_dir_all(main.join("inc")).unwrap();
    std::fs::write(main.join("real.txt"), "content").unwrap();
    let target = Path::new("..").join("real.txt");
    if devkit_common::sys::symlink(&target, &main.join("inc/free.txt"), false).is_err()
        || devkit_common::sys::symlink(&target, &main.join("inc/occupied.txt"), false).is_err()
    {
        eprintln!("skipping: this platform refuses symlink creation");
        return;
    }
    let wt1_inc = t.path().join("wt-eng-1").join("inc");
    std::fs::create_dir_all(&wt1_inc).unwrap();
    std::fs::write(wt1_inc.join("occupied.txt"), "already here").unwrap();

    let state = tempfile::tempdir().unwrap();
    let out = run(
        t.path(),
        state.path(),
        &["sync-includes", "--dry-run", "eng-1"],
    );
    ok(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let split = stdout
        .find("would leave alone")
        .expect("leave-alone block present");
    let (would_link, leave_alone) = stdout.split_at(split);
    assert!(
        would_link.contains("free.txt"),
        "a free destination is listed under would link: {stdout}"
    );
    assert!(
        !would_link.contains("occupied.txt"),
        "an occupied destination is not listed under would link: {stdout}"
    );
    assert!(
        leave_alone.contains("occupied.txt"),
        "an occupied destination is listed under the leave-alone block: {stdout}"
    );
    assert!(
        !leave_alone.contains("free.txt"),
        "a free destination is not listed under the leave-alone block: {stdout}"
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

/// A dry run writes nothing, so the scope gate would only stop you from
/// surveying what `--overwrite` would clobber before you opt into `--all`.
#[test]
fn an_unscoped_overwrite_dry_run_is_allowed() {
    let t = project("\".env.local\"");
    let state = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("wt-eng-1").join(".env.local"), "OLD=1\n").unwrap();

    let out = run(
        t.path(),
        state.path(),
        &["sync-includes", "--overwrite", "--dry-run"],
    );
    assert!(
        out.status.success(),
        "a dry run writes nothing and needs no scope: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(env_local(t.path(), "wt-eng-1").as_deref(), Some("OLD=1\n"));
    assert_eq!(env_local(t.path(), "wt-eng-2"), None);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(".env.local"),
        "the survey names the file it would clobber: {stdout}"
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

/// The monorepo is the copy source and never a target, and the worktree the
/// command runs from is a target like any other.
#[test]
fn syncing_from_a_worktree_leaves_the_monorepo_alone() {
    let t = project("\".env.local\"");
    let state = tempfile::tempdir().unwrap();

    let out = run_in(t.path(), "wt-eng-1", state.path(), &["sync-includes"]);
    ok(&out);
    assert_eq!(
        env_local(t.path(), "wt-eng-1").as_deref(),
        Some("FRESH=1\n")
    );
    assert_eq!(
        env_local(t.path(), "wt-eng-2").as_deref(),
        Some("FRESH=1\n")
    );
    assert_eq!(env_local(t.path(), "main").as_deref(), Some("FRESH=1\n"));
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
