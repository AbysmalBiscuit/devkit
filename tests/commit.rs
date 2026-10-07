//! `devkit commit` end to end: a selection committed with a rendered message
//! while changes another session staged stay staged.

use std::{
    path::Path,
    process::{Command, Output},
};

const NULL_DEVICE: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

fn git(dir: &Path, args: &[&str]) -> String {
    devkit_git::Git::fixture(dir)
        .args(args.iter().copied())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e:#}"))
}

/// A repository with `a.txt` and `b.txt` committed, five lines each.
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    for name in ["a.txt", "b.txt"] {
        std::fs::write(dir.path().join(name), lines(name, &[])).unwrap();
    }
    git(dir.path(), &["add", "a.txt", "b.txt"]);
    git(dir.path(), &["commit", "-qm", "init"]);
    dir
}

/// Five numbered lines of `name`, with the lines in `changed` marked.
fn lines(name: &str, changed: &[usize]) -> String {
    (1..=5)
        .map(|i| {
            let mark = if changed.contains(&i) { " changed" } else { "" };
            format!("{name} {i}{mark}\n")
        })
        .collect()
}

/// `devkit commit` in `dir`, run by a human.
fn commit(dir: &Path, args: &[&str]) -> Output {
    devkit_commit(dir).args(args).output().expect("run devkit")
}

/// `devkit commit` in `dir`, isolated from the developer's own git config and
/// devkit state.
fn devkit_commit(dir: &Path) -> Command {
    let home = dir.join(".git/test-home");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.current_dir(dir)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("LOCALAPPDATA", home.join("state"))
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("DEVKIT_CALLER", "human")
        .env("GIT_CONFIG_GLOBAL", NULL_DEVICE)
        .env("GIT_CONFIG_SYSTEM", NULL_DEVICE)
        .env("GIT_AUTHOR_NAME", "devkit test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "devkit test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .arg("commit");
    cmd
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn head(dir: &Path) -> String {
    git(dir, &["rev-parse", "HEAD"]).trim().to_string()
}

fn message(dir: &Path) -> String {
    git(dir, &["log", "-1", "--format=%B"])
}

fn changed_in_head(dir: &Path) -> String {
    git(dir, &["show", "--name-status", "--format=", "HEAD"])
}

fn staged(dir: &Path) -> String {
    git(dir, &["diff", "--cached"])
}

#[test]
fn files_commits_only_the_named_paths_and_keeps_other_staging() {
    let repo = repo();
    let dir = repo.path();
    std::fs::write(dir.join("a.txt"), lines("a.txt", &[1])).unwrap();
    std::fs::write(dir.join("new.txt"), "new\n").unwrap();
    std::fs::write(dir.join("b.txt"), lines("b.txt", &[2])).unwrap();
    git(dir, &["add", "b.txt"]);

    let out = commit(dir, &[
        "--files",
        "a.txt",
        "new.txt",
        "--subject",
        "feat: change a",
        "--coauthor",
        "Claude Opus 5.5 <noreply@anthropic.com>",
        "--coauthor",
        "Codex <noreply@openai.com>",
    ]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        message(dir),
        "feat: change a\n\n\
         Co-authored-by: Claude Opus 5.5 <noreply@anthropic.com>\n\
         Co-authored-by: Codex <noreply@openai.com>\n\n"
    );
    assert_eq!(changed_in_head(dir), "M\ta.txt\nA\tnew.txt\n");
    assert_eq!(
        git(dir, &["diff", "--cached", "--name-only"]),
        "b.txt\n",
        "b.txt, staged by someone else, is still staged"
    );
}

#[test]
fn files_commits_a_deleted_path() {
    let repo = repo();
    let dir = repo.path();
    std::fs::remove_file(dir.join("a.txt")).unwrap();

    let out = commit(dir, &["--files", "a.txt", "--subject", "chore: drop a"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(changed_in_head(dir), "D\ta.txt\n");
    assert_eq!(staged(dir), "");
}

#[test]
fn a_rejected_files_commit_leaves_the_index_as_it_was() {
    let repo = repo();
    let dir = repo.path();
    std::fs::write(dir.join("new.txt"), "new\n").unwrap();
    let hook = dir.join(".git/hooks/pre-commit");
    std::fs::write(&hook, "#!/bin/sh\necho no >&2\nexit 1\n").unwrap();
    make_executable(&hook);
    let before = head(dir);

    let out = commit(dir, &["--files", "new.txt", "--subject", "feat: new"]);

    assert!(!out.status.success());
    assert_eq!(head(dir), before);
    assert_eq!(git(dir, &["status", "--porcelain"]), "?? new.txt\n");
}

#[test]
fn files_naming_a_directory_skips_its_ignored_files() {
    let repo = repo();
    let dir = repo.path();
    std::fs::write(dir.join(".gitignore"), "ign\n").unwrap();
    git(dir, &["add", ".gitignore"]);
    git(dir, &["commit", "-qm", "ignore"]);
    std::fs::create_dir(dir.join("d")).unwrap();
    std::fs::write(dir.join("d/new.txt"), "new\n").unwrap();
    std::fs::write(dir.join("d/ign"), "ignored\n").unwrap();

    let out = commit(dir, &["--files", "d", "--subject", "feat: d"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(changed_in_head(dir), "A\td/new.txt\n");
    assert_eq!(git(dir, &["status", "--porcelain"]), "");
}

#[test]
fn a_refused_files_commit_forgets_the_paths_it_recorded() {
    let repo = repo();
    let dir = repo.path();
    std::fs::write(dir.join(".gitignore"), "ign\n").unwrap();
    git(dir, &["add", ".gitignore"]);
    git(dir, &["commit", "-qm", "ignore"]);
    std::fs::write(dir.join("new.txt"), "new\n").unwrap();
    std::fs::write(dir.join("ign"), "ignored\n").unwrap();
    let before = head(dir);

    let out = commit(dir, &[
        "--files",
        "new.txt",
        "ign",
        "--subject",
        "feat: new",
    ]);

    assert!(!out.status.success());
    assert_eq!(head(dir), before);
    assert_eq!(git(dir, &["status", "--porcelain"]), "?? new.txt\n");
}

#[test]
fn the_project_commit_message_template_renders_the_message() {
    let repo = repo();
    let dir = repo.path();
    std::fs::write(
        dir.join("devkit.toml"),
        "[templates]\ncommit_message = \"{{ subject }} [{{ ticket }}]\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("a.txt"), lines("a.txt", &[1])).unwrap();

    let out = commit(dir, &[
        "--files",
        "a.txt",
        "--subject",
        "fix: a",
        "--arg",
        "ticket=ENG-1",
    ]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(message(dir), "fix: a [ENG-1]\n\n");
}

#[test]
fn an_agent_must_name_its_coauthors_when_the_project_says_so() {
    let repo = repo();
    let dir = repo.path();
    std::fs::write(
        dir.join("devkit.toml"),
        "[templates.variables]\ncoauthors = { default = \"\", required = \"agents\" }\n",
    )
    .unwrap();
    std::fs::write(dir.join("a.txt"), lines("a.txt", &[1])).unwrap();
    let before = head(dir);

    let out = devkit_commit(dir)
        .env("DEVKIT_CALLER", "agent")
        .args(["--files", "a.txt", "--subject", "fix: a"])
        .output()
        .unwrap();

    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("devkit commit needs --coauthor=... (required for agents)"),
        "{}",
        stderr(&out)
    );
    assert_eq!(head(dir), before);
}

#[test]
fn amend_rewords_the_last_commit_and_keeps_staged_changes() {
    let repo = repo();
    let dir = repo.path();
    let tree = git(dir, &["rev-parse", "HEAD^{tree}"]);
    std::fs::write(dir.join("b.txt"), lines("b.txt", &[2])).unwrap();
    git(dir, &["add", "b.txt"]);
    let staged_before = staged(dir);

    let out = commit(dir, &[
        "--amend",
        "--subject",
        "feat: start",
        "--body",
        "Why it starts.",
    ]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(message(dir), "feat: start\n\nWhy it starts.\n\n");
    assert_eq!(git(dir, &["rev-parse", "HEAD^{tree}"]), tree);
    assert_eq!(git(dir, &["rev-list", "--count", "HEAD"]), "1\n");
    assert_eq!(staged(dir), staged_before);
}

/// A patch against HEAD changing line 1 of `a.txt`.
fn patch_a_line_1(dir: &Path) -> std::path::PathBuf {
    let path = dir.join(".git/a.patch");
    std::fs::write(dir.join("a.txt"), lines("a.txt", &[1])).unwrap();
    std::fs::write(&path, git(dir, &["diff", "--", "a.txt"])).unwrap();
    git(dir, &["checkout", "--", "a.txt"]);
    path
}

#[test]
fn patch_commits_only_its_hunks_and_keeps_unrelated_staging() {
    let repo = repo();
    let dir = repo.path();
    let patch = patch_a_line_1(dir);
    // Someone else staged line 5 of the same file and all of b.txt, and has
    // unstaged work on line 3.
    std::fs::write(dir.join("a.txt"), lines("a.txt", &[5])).unwrap();
    std::fs::write(dir.join("b.txt"), lines("b.txt", &[2])).unwrap();
    git(dir, &["add", "a.txt", "b.txt"]);
    std::fs::write(dir.join("a.txt"), lines("a.txt", &[3, 5])).unwrap();

    let out = commit(dir, &[
        "--patch",
        patch.to_str().unwrap(),
        "--subject",
        "fix: line 1",
    ]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(message(dir), "fix: line 1\n\n");
    assert_eq!(git(dir, &["show", "HEAD:a.txt"]), lines("a.txt", &[1]));
    assert_eq!(changed_in_head(dir), "M\ta.txt\n");
    assert_eq!(git(dir, &["show", ":a.txt"]), lines("a.txt", &[1, 5]));
    assert_eq!(git(dir, &["show", ":b.txt"]), lines("b.txt", &[2]));
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        lines("a.txt", &[3, 5]),
        "the working tree is untouched"
    );
}

#[test]
fn a_relative_patch_is_read_from_the_directory_devkit_runs_in() {
    let repo = repo();
    let dir = repo.path();
    let patch = patch_a_line_1(dir);
    let sub = dir.join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::rename(&patch, sub.join("a.patch")).unwrap();

    let out = commit(&sub, &["--patch", "a.patch", "--subject", "fix: line 1"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(git(dir, &["show", "HEAD:a.txt"]), lines("a.txt", &[1]));
}

#[test]
fn a_patch_overlapping_staged_hunks_is_refused_with_head_and_index_unchanged() {
    let repo = repo();
    let dir = repo.path();
    let patch = patch_a_line_1(dir);
    let mut other = lines("a.txt", &[]);
    other = other.replacen("a.txt 1\n", "a.txt 1 elsewhere\n", 1);
    std::fs::write(dir.join("a.txt"), &other).unwrap();
    git(dir, &["add", "a.txt"]);
    let before = head(dir);
    let index = std::fs::read(dir.join(".git/index")).unwrap();

    let out = commit(dir, &[
        "--patch",
        patch.to_str().unwrap(),
        "--subject",
        "fix: line 1",
    ]);

    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("overlaps staged changes"),
        "{}",
        stderr(&out)
    );
    assert_eq!(head(dir), before);
    assert_eq!(std::fs::read(dir.join(".git/index")).unwrap(), index);
    assert!(!dir.join(".git/index.lock").exists());
}

#[test]
fn a_patch_sharing_a_path_with_staging_under_a_custom_merge_driver_is_refused() {
    let repo = repo();
    let dir = repo.path();
    std::fs::write(dir.join(".gitattributes"), "a.txt merge=mine\n").unwrap();
    git(dir, &["config", "merge.mine.driver", "true"]);
    let patch = patch_a_line_1(dir);
    std::fs::write(dir.join("a.txt"), lines("a.txt", &[5])).unwrap();
    git(dir, &["add", "a.txt"]);
    let before = head(dir);

    let out = commit(dir, &[
        "--patch",
        patch.to_str().unwrap(),
        "--subject",
        "fix: line 1",
    ]);

    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("merge driver `mine`"),
        "{}",
        stderr(&out)
    );
    assert_eq!(head(dir), before);
    assert_eq!(git(dir, &["show", ":a.txt"]), lines("a.txt", &[5]));
}

#[cfg(unix)]
#[test]
fn a_hook_that_changes_the_committed_tree_makes_patch_refuse() {
    let repo = repo();
    let dir = repo.path();
    let patch = patch_a_line_1(dir);
    std::fs::write(dir.join("b.txt"), lines("b.txt", &[4])).unwrap();
    let hook = dir.join(".git/hooks/pre-commit");
    std::fs::write(&hook, "#!/bin/sh\ngit add b.txt\n").unwrap();
    make_executable(&hook);
    let before = head(dir);

    let out = commit(dir, &[
        "--patch",
        patch.to_str().unwrap(),
        "--subject",
        "fix: line 1",
    ]);

    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("changed the committed tree"),
        "{}",
        stderr(&out)
    );
    assert_eq!(head(dir), before);
    assert_eq!(staged(dir), "");
    assert!(!dir.join(".git/index.lock").exists());
}

#[cfg(unix)]
#[test]
fn patch_runs_the_repository_hooks() {
    let repo = repo();
    let dir = repo.path();
    let patch = patch_a_line_1(dir);
    let hook = dir.join(".git/hooks/commit-msg");
    std::fs::write(&hook, "#!/bin/sh\necho 'Signed-off-by: hook' >> \"$1\"\n").unwrap();
    make_executable(&hook);

    let out = commit(dir, &[
        "--patch",
        patch.to_str().unwrap(),
        "--subject",
        "fix: line 1",
    ]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(message(dir), "fix: line 1\nSigned-off-by: hook\n\n");
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(not(unix))]
fn make_executable(_: &Path) {}
