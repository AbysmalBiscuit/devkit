//! `std::env::set_var` is unsafe because `Command::spawn` reads the
//! environment concurrently — a hazard `cargo test`'s multithreaded unit-test
//! binary cannot rule out. An integration test file compiles to its own
//! binary, and every mutation below happens inside one `#[test]` function, so
//! no other thread can be reading the environment while it is set.

use devkit_common::git::Git;
use std::path::Path;

/// Every variable in `REDIRECTING_VARS` that could repoint a git call at
/// another repository, or inject config into it, must have no effect on what
/// the builder resolves. Kept as one test, each case set and cleared before
/// the next begins, because the variables are process-global and parallel
/// `#[test]` functions in this binary would race on them. A failure here
/// collects every regressed variable into one assertion rather than stopping
/// at the first, so cutting `REDIRECTING_VARS` down names all of them at once.
#[test]
fn redirecting_vars_cannot_change_resolution() {
    let repo = tempfile::tempdir().unwrap();
    Git::fixture(repo.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();

    let decoy = tempfile::tempdir().unwrap();
    Git::fixture(decoy.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();

    let mut failures = Vec::new();

    // GIT_DIR redirects git to another repository's git-dir.
    with_env(&[("GIT_DIR", decoy.path().join(".git"))], || {
        let out = Git::at(repo.path())
            .args(["rev-parse", "--absolute-git-dir"])
            .output();
        let git_dir = std::fs::canonicalize(out.unwrap().trim()).unwrap();
        if git_dir != std::fs::canonicalize(repo.path().join(".git")).unwrap() {
            failures.push("GIT_DIR");
        }
    });

    // GIT_COMMON_DIR redirects the shared repository state (refs, objects,
    // config) that a linked worktree reads. It has no visible effect on the
    // main checkout, whose common dir is always its own `.git` — a linked
    // worktree is what exposes it.
    let linked = tempfile::tempdir().unwrap();
    let linked = linked.path().join("wt");
    Git::fixture(repo.path())
        .args([
            "worktree",
            "add",
            "-q",
            linked.to_str().unwrap(),
            "-b",
            "side",
        ])
        .output()
        .unwrap();
    with_env(&[("GIT_COMMON_DIR", decoy.path().join(".git"))], || {
        let out = Git::at(&linked)
            .args(["rev-parse", "--git-common-dir"])
            .output()
            .unwrap();
        let reported = Path::new(out.trim());
        let common_dir = std::fs::canonicalize(if reported.is_absolute() {
            reported.to_path_buf()
        } else {
            linked.join(reported)
        })
        .unwrap();
        if common_dir != std::fs::canonicalize(repo.path().join(".git")).unwrap() {
            failures.push("GIT_COMMON_DIR");
        }
    });

    // GIT_WORK_TREE redirects which directory git treats as the checkout's
    // top level.
    with_env(&[("GIT_WORK_TREE", decoy.path())], || {
        let out = Git::at(repo.path())
            .args(["rev-parse", "--show-toplevel"])
            .output();
        let toplevel = std::fs::canonicalize(out.unwrap().trim()).unwrap();
        if toplevel != std::fs::canonicalize(repo.path()).unwrap() {
            failures.push("GIT_WORK_TREE");
        }
    });

    // GIT_INDEX_FILE redirects which staged state git reads and writes.
    std::fs::write(decoy.path().join("decoy-only.txt"), "x").unwrap();
    Git::fixture(decoy.path())
        .args(["add", "decoy-only.txt"])
        .output()
        .unwrap();
    with_env(
        &[("GIT_INDEX_FILE", decoy.path().join(".git/index"))],
        || {
            let out = Git::at(repo.path()).args(["ls-files"]).output().unwrap();
            if out.contains("decoy-only.txt") {
                failures.push("GIT_INDEX_FILE");
            }
        },
    );

    // GIT_CONFIG_COUNT, with a GIT_CONFIG_KEY_0/GIT_CONFIG_VALUE_0 pair, can
    // set an arbitrary config value — core.fsmonitor turns into a command git
    // executes on every invocation were this honored.
    with_env(
        &[
            ("GIT_CONFIG_COUNT", "1".to_string()),
            ("GIT_CONFIG_KEY_0", "core.fsmonitor".to_string()),
            (
                "GIT_CONFIG_VALUE_0",
                "touch injected-by-git-config-count".to_string(),
            ),
        ],
        || {
            let out = Git::fixture(repo.path())
                .args(["config", "core.fsmonitor"])
                .output();
            if out.is_ok() {
                failures.push("GIT_CONFIG_COUNT");
            }
        },
    );

    // GIT_CEILING_DIRECTORIES can stop repository discovery before it reaches
    // an ancestor .git, turning a legitimate call into a false "not a git
    // repository".
    let sub = repo.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    with_env(&[("GIT_CEILING_DIRECTORIES", repo.path())], || {
        let out = Git::at(&sub)
            .args(["rev-parse", "--show-toplevel"])
            .output();
        match out {
            Ok(toplevel) => {
                let toplevel = std::fs::canonicalize(toplevel.trim()).unwrap();
                if toplevel != std::fs::canonicalize(repo.path()).unwrap() {
                    failures.push("GIT_CEILING_DIRECTORIES");
                }
            }
            Err(_) => failures.push("GIT_CEILING_DIRECTORIES"),
        }
    });

    assert!(
        failures.is_empty(),
        "these variables changed what the builder resolved: {failures:?}"
    );
}

/// Set each `(key, value)` pair, run `body`, then remove every key —
/// regardless of whether `body` panics, so a failing case never leaves the
/// environment poisoned for the ones after it.
///
/// SAFETY: this is the only place in the process mutating the environment;
/// see the module docs.
fn with_env<V: AsRef<std::ffi::OsStr>>(vars: &[(&str, V)], body: impl FnOnce()) {
    for (key, value) in vars {
        unsafe { std::env::set_var(key, value) };
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
    for (key, _) in vars {
        unsafe { std::env::remove_var(key) };
    }
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}
