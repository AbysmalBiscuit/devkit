//! Every variable in `REDIRECTING_VARS` that could repoint a git call at
//! another repository, or inject config into it, must have no effect on what
//! the builder resolves.

use std::path::{Path, PathBuf};

use devkit_git::{Git, child_env::child_env};
use tempfile::TempDir;

fn repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    Git::fixture(dir.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    dir
}

fn canonical(path: impl AsRef<Path>) -> PathBuf {
    std::fs::canonicalize(path).unwrap()
}

/// GIT_DIR redirects git to another repository's git-dir.
#[test]
fn git_dir_cannot_change_resolution() {
    let decoy = repo();
    child_env()
        .set("GIT_DIR", decoy.path().join(".git"))
        .run(|| {
            let repo = repo();
            let out = Git::at(repo.path())
                .args(["rev-parse", "--absolute-git-dir"])
                .output();
            assert_eq!(
                canonical(out.unwrap().trim()),
                canonical(repo.path().join(".git"))
            );
        });
}

/// GIT_COMMON_DIR redirects the shared repository state (refs, objects, config)
/// that every worktree of a repository reads. A linked worktree is where devkit
/// resolves a project's config from, so that is the setting this checks it in.
#[test]
fn git_common_dir_cannot_change_resolution() {
    let decoy = repo();
    child_env()
        .set("GIT_COMMON_DIR", decoy.path().join(".git"))
        .run(|| {
            let repo = repo();
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
            let out = Git::at(&linked)
                .args(["rev-parse", "--git-common-dir"])
                .output()
                .unwrap();
            let reported = Path::new(out.trim());
            let common_dir = if reported.is_absolute() {
                reported.to_path_buf()
            } else {
                linked.join(reported)
            };
            assert_eq!(canonical(common_dir), canonical(repo.path().join(".git")));
        });
}

/// GIT_WORK_TREE redirects which directory git treats as the checkout's top
/// level.
#[test]
fn git_work_tree_cannot_change_resolution() {
    let decoy = repo();
    child_env().set("GIT_WORK_TREE", decoy.path()).run(|| {
        let repo = repo();
        let out = Git::at(repo.path())
            .args(["rev-parse", "--show-toplevel"])
            .output();
        assert_eq!(canonical(out.unwrap().trim()), canonical(repo.path()));
    });
}

/// GIT_INDEX_FILE redirects which staged state git reads and writes.
#[test]
fn git_index_file_cannot_change_resolution() {
    let decoy = repo();
    std::fs::write(decoy.path().join("decoy-only.txt"), "x").unwrap();
    Git::fixture(decoy.path())
        .args(["add", "decoy-only.txt"])
        .output()
        .unwrap();
    child_env()
        .set("GIT_INDEX_FILE", decoy.path().join(".git/index"))
        .run(|| {
            let repo = repo();
            let out = Git::at(repo.path()).args(["ls-files"]).output().unwrap();
            assert!(!out.contains("decoy-only.txt"), "{out}");
        });
}

/// GIT_CONFIG_COUNT, with a GIT_CONFIG_KEY_0/GIT_CONFIG_VALUE_0 pair, can set
/// an arbitrary config value. core.fsmonitor would turn into a command git
/// executes on every invocation were this honored.
#[test]
fn git_config_count_cannot_inject_config() {
    child_env()
        .set("GIT_CONFIG_COUNT", "1")
        .set("GIT_CONFIG_KEY_0", "core.fsmonitor")
        .set("GIT_CONFIG_VALUE_0", "touch injected-by-git-config-count")
        .run(|| {
            let repo = repo();
            let out = Git::fixture(repo.path())
                .args(["config", "core.fsmonitor"])
                .output();
            assert!(out.is_err(), "{out:?}");
        });
}

/// GIT_CEILING_DIRECTORIES can stop repository discovery before it reaches an
/// ancestor .git, turning a legitimate call into a false "not a git
/// repository". The ceiling has to be the repository itself, so the variable
/// names a repository the parent made, and the body runs against it.
#[test]
fn git_ceiling_directories_cannot_change_resolution() {
    let repo = repo();
    std::fs::create_dir(repo.path().join("sub")).unwrap();
    child_env()
        .set("GIT_CEILING_DIRECTORIES", repo.path())
        .run(|| {
            let repo = PathBuf::from(std::env::var_os("GIT_CEILING_DIRECTORIES").unwrap());
            let toplevel = Git::at(&repo.join("sub"))
                .args(["rev-parse", "--show-toplevel"])
                .output()
                .unwrap();
            assert_eq!(canonical(toplevel.trim()), canonical(&repo));
        });
}
