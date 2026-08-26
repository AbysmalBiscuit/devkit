//! `std::env::set_var` is unsafe because `Command::spawn` reads the
//! environment concurrently — a hazard `cargo test`'s multithreaded unit-test
//! binary cannot rule out. An integration test file compiles to its own
//! binary, and this is its only test, so no other thread can be reading the
//! environment while this one mutates it.

use devkit_common::git::Git;

/// An ambient `GIT_DIR` redirects git to another repository's git-dir. Every
/// call in the workspace goes through this builder, so stripping the
/// redirecting variables is what stops a stranger's config being read as this
/// repository's.
#[test]
fn ambient_git_dir_cannot_redirect_a_call() {
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

    // SAFETY: this file is its own test binary with a single test, so no
    // other thread reads or writes the environment while it is set.
    unsafe { std::env::set_var("GIT_DIR", decoy.path().join(".git")) };
    let out = Git::at(repo.path())
        .args(["rev-parse", "--absolute-git-dir"])
        .output();
    unsafe { std::env::remove_var("GIT_DIR") };

    let git_dir = std::fs::canonicalize(out.unwrap().trim()).unwrap();
    assert_eq!(
        git_dir,
        std::fs::canonicalize(repo.path().join(".git")).unwrap()
    );
}
