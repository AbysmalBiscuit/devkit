//! A relative `--arg-file` path names a file in the directory `-C` selects,
//! not in the process directory, for every command that takes both.

#[path = "common/testenv.rs"]
mod testenv;

use std::{
    path::Path,
    process::{Command, Output},
};

#[cfg(windows)]
const NULL_DEVICE: &str = "NUL";
#[cfg(not(windows))]
const NULL_DEVICE: &str = "/dev/null";

const CONFIG: &str = r#"
[templates]
commit_message = "{{ subject }}: {{ note }}"
issue_body = "{{ input }}: {{ note }}"

[templates.variables]
note = { default = "none" }

[tasks.echo]
run = ["git", "--note={{ note }}", "version"]
"#;

/// A project with `note.md`, and a process directory with a `note.md` of
/// its own.
struct Dirs {
    project: tempfile::TempDir,
    elsewhere: tempfile::TempDir,
}

fn dirs() -> Dirs {
    let project = tempfile::tempdir().unwrap();
    git(project.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(project.path().join("devkit.toml"), CONFIG).unwrap();
    std::fs::write(project.path().join("note.md"), "from the project").unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::write(elsewhere.path().join("note.md"), "from elsewhere").unwrap();
    Dirs { project, elsewhere }
}

fn git(dir: &Path, args: &[&str]) -> String {
    devkit_git::Git::fixture(dir)
        .args(args.iter().copied())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e:#}"))
}

/// `devkit ARGS` from `cwd`, isolated from the developer's git config and
/// devkit state.
fn devkit(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.current_dir(cwd)
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("LOCALAPPDATA", home.join("state"))
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("DEVKIT_CALLER", "human")
        .env_remove("DEVKIT_CONFIG")
        .env("GIT_CONFIG_GLOBAL", NULL_DEVICE)
        .env("GIT_CONFIG_SYSTEM", NULL_DEVICE)
        .env("GIT_AUTHOR_NAME", "devkit test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "devkit test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid");
    for var in devkit_common::caller::HARNESS_SESSION_VARS {
        cmd.env_remove(var);
    }
    testenv::scrub_identity(&mut cmd);
    cmd.output().expect("spawn devkit")
}

fn stdout(out: &Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn run(d: &Dirs, args: &[&str]) -> Output {
    let home = tempfile::tempdir().unwrap();
    devkit(d.elsewhere.path(), home.path(), args)
}

fn project(d: &Dirs) -> &str {
    d.project.path().to_str().unwrap()
}

#[test]
fn ticket_render_reads_from_the_start_directory() {
    let d = dirs();
    let out = stdout(&run(&d, &[
        "ticket",
        "-C",
        project(&d),
        "render",
        "--title",
        "T",
        "--body",
        "B",
        "--arg-file",
        "note=note.md",
    ]));
    assert!(out.contains("B: from the project"), "{out}");
}

#[test]
fn template_render_reads_from_the_start_directory() {
    let d = dirs();
    let out = stdout(&run(&d, &[
        "template",
        "-C",
        project(&d),
        "render",
        "commit_message",
        "--subject",
        "S",
        "--arg-file",
        "note=note.md",
    ]));
    assert!(out.contains("S: from the project"), "{out}");
}

#[test]
fn commit_reads_from_the_start_directory() {
    let d = dirs();
    git(d.project.path(), &["add", "devkit.toml"]);
    git(d.project.path(), &["commit", "-qm", "init"]);
    std::fs::write(d.project.path().join("a.txt"), "a").unwrap();

    stdout(&run(&d, &[
        "commit",
        "-C",
        project(&d),
        "--files",
        "a.txt",
        "--subject",
        "S",
        "--arg-file",
        "note=note.md",
    ]));

    let message = git(d.project.path(), &["log", "-1", "--format=%s"]);
    assert_eq!(message.trim(), "S: from the project");
}

#[test]
fn devrun_task_reads_from_the_start_directory() {
    let d = dirs();
    let out = stdout(&run(&d, &[
        "run",
        "-C",
        project(&d),
        "task",
        "echo",
        "--arg-file",
        "note=note.md",
        "--dry-run",
    ]));
    assert!(out.contains("--note=from the project"), "{out}");
}

#[test]
fn an_absolute_path_is_kept() {
    let d = dirs();
    let absolute = d.elsewhere.path().join("note.md");
    let out = stdout(&run(&d, &[
        "template",
        "-C",
        project(&d),
        "render",
        "commit_message",
        "--subject",
        "S",
        "--arg-file",
        &format!("note={}", absolute.display()),
    ]));
    assert!(out.contains("S: from elsewhere"), "{out}");
}
