//! `devkit run task` end-to-end: listing, dry-run rendering with a registry-
//! allocated port, execution, and exit-code propagation. Drives `devkit run`
//! directly (not the `devrun` shim). Uses an isolated HOME/XDG_STATE_HOME so
//! the port registry never touches the real one.

use std::{path::Path, process::Command};

fn devkit_run() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.arg("run");
    cmd
}

/// A temp dir that is a git repo (cmd_task resolves the worktree root) with a
/// devkit.toml defining one app and three tasks.
fn setup() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let git = |args: &[&str]| {
        devkit_common::git::Git::fixture(root)
            .args(args.iter().copied())
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
    };
    git(&["init", "-q"]);
    std::fs::write(
        root.join("devkit.toml"),
        r#"
[defaults]
worktree_root = "wts"
branch_prefix = "x/"
baseline_ref = "origin/main"

[apps.api]
base_port = 39140
path = "."
launch = ["git", "version"]
static_env = { FROM_APP = "static" }

[tasks.hello]
description = "prints git version"
run = ["git", "version"]

[tasks.show-port]
run = ["git", "--url=http://localhost:{{ ports['api'] }}", "version"]

[tasks.fail]
run = ["git", "definitely-not-a-subcommand"]

[tasks.seq]
steps = [{ up = "api" }, { task = "hello" }]

[templates.variables]
scope = "devkit"

[tasks.commit]
description = "commit with a scope"
run = ["git", "--msg={{ scope }}: {{ msg }}", "version"]

[tasks.seq-commit]
steps = [{ task = "hello" }, { task = "commit" }]

[tasks.issue-only]
run = ["git", "--issue={{ issue }}", "version"]

[tasks.both]
run = ["git", "--msg={{ msg }}"]
steps = [{ up = "api" }]

[tasks.broken]
run = ["git", "--msg={{ msg "]

[tasks.tagged]
run = ["git", "--tag={% if issue is defined %}{{ issue }}/{{ slug }}@{{ branch }}{% else %}none{% endif %}", "version"]
"#,
    )
    .expect("write devkit.toml");
    dir
}

fn run_in(dir: &Path, args: &[&str]) -> std::process::Output {
    let state = dir.join("state");
    devkit_run()
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_STATE_HOME", &state)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("LOCALAPPDATA", &state) // Windows: keep the registry off the real one
        .env("USERPROFILE", dir) // Windows: keep config resolution off the real home
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .output()
        .expect("run devkit run")
}

#[test]
fn task_lists_names_and_descriptions() {
    let dir = setup();
    let out = run_in(dir.path(), &["task"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("hello"),
        "listing missing task name: {stdout}"
    );
    assert!(
        stdout.contains("prints git version"),
        "listing missing description: {stdout}"
    );
}

#[test]
fn task_dry_run_renders_allocated_port() {
    let dir = setup();
    let out = run_in(dir.path(), &["task", "show-port", "--dry-run"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("http://localhost:391"),
        "dry-run must show a rendered port at/above base 39140: {stdout}"
    );
    assert!(
        !stdout.contains("{{"),
        "no unrendered templates in dry-run: {stdout}"
    );
}

#[test]
fn task_seq_dry_run_renders_up_step_plan() {
    let dir = setup();
    let out = run_in(dir.path(), &["task", "seq", "--dry-run"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("up api\n") && !stdout.trim_end().ends_with("up api"),
        "up step must render its full plan, not a bare `up api` line: {stdout}"
    );
    // `cmd_up`'s dry-run block is the only place that prints a `log:` line
    // (`run_task_step`'s dry-run branch prints only cwd/argv/env), so its
    // presence proves the up step went through the same rendering as
    // `up --dry-run` rather than the old one-line summary.
    assert!(
        stdout.contains("[issue] api :") && stdout.contains("log:"),
        "up step must render the same [role] app :port / cwd / argv / env / log \
         plan that `up --dry-run` prints: {stdout}"
    );
    assert!(
        stdout.contains("argv: git version"),
        "up step's rendered argv missing: {stdout}"
    );
    assert!(
        !stdout.contains("{{"),
        "no unrendered templates in dry-run: {stdout}"
    );

    // Sanity: dry-run never executes; `run_task_step`'s real-run branch is the
    // only place that prints an exec-marker line.
    assert!(
        !stdout.contains("→ "),
        "dry-run must not execute steps: {stdout}"
    );
}

#[test]
fn task_seq_env_overrides_up_step_static_env() {
    let dir = setup();
    let out = run_in(dir.path(), &[
        "task",
        "seq",
        "--env",
        "FROM_APP=user",
        "--dry-run",
    ]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("FROM_APP=user"),
        "--env override missing from rendered env: {stdout}"
    );
    assert!(
        !stdout.contains("FROM_APP=static"),
        "up step fell back to static_env instead of the --env override: {stdout}"
    );
}

#[test]
fn task_env_file_feeds_steps_and_env_wins() {
    let dir = setup();
    let envfile = dir.path().join("task.env");
    std::fs::write(&envfile, "# comment\nFROM_APP=filed\nEXTRA=1\n").expect("write env file");
    let envfile = envfile.to_string_lossy().into_owned();

    let out = run_in(dir.path(), &[
        "task",
        "seq",
        "--env-file",
        &envfile,
        "--dry-run",
    ]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("FROM_APP=filed") && stdout.contains("EXTRA=1"),
        "--env-file vars missing from rendered env: {stdout}"
    );
    assert!(
        !stdout.contains("FROM_APP=static"),
        "up step fell back to static_env instead of the --env-file value: {stdout}"
    );

    let out = run_in(dir.path(), &[
        "task",
        "seq",
        "--env-file",
        &envfile,
        "--env",
        "FROM_APP=cli",
        "--dry-run",
    ]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("FROM_APP=cli") && !stdout.contains("FROM_APP=filed"),
        "--env must win over --env-file, matching `up`: {stdout}"
    );
}

#[test]
fn task_arg_fills_a_variable_the_task_reads() {
    let dir = setup();
    let out = run_in(dir.path(), &[
        "task",
        "commit",
        "--arg",
        "msg=fix it",
        "--dry-run",
    ]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("argv: git --msg=devkit: fix it version"),
        "{stdout}"
    );
}

#[test]
fn task_arg_overrides_a_shared_variable() {
    let dir = setup();
    let out = run_in(dir.path(), &[
        "task",
        "commit",
        "--arg",
        "scope=web",
        "--arg",
        "msg=fix it",
        "--dry-run",
    ]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("argv: git --msg=web: fix it version"),
        "{stdout}"
    );
}

#[test]
fn a_missing_required_arg_fails_the_sequence_before_any_step_runs() {
    let dir = setup();
    let out = run_in(dir.path(), &["task", "seq-commit"]);
    assert!(!out.status.success(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--arg msg="), "{stderr}");
    assert!(
        !stderr.contains("\u{2192} hello"),
        "the first step ran before the missing arg was reported: {stderr}"
    );
}

#[test]
fn an_arg_no_task_template_reads_is_rejected() {
    let dir = setup();
    let out = run_in(dir.path(), &[
        "task",
        "hello",
        "--arg",
        "nope=1",
        "--dry-run",
    ]);
    assert!(!out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("`nope`"),
        "{out:?}"
    );
}

#[test]
fn issue_fields_render_from_the_record_and_are_undefined_without_one() {
    let dir = setup();
    let out = run_in(dir.path(), &["task", "tagged", "--dry-run"]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("--tag=none"),
        "{out:?}"
    );

    let git = |args: &[&str]| {
        devkit_common::git::Git::fixture(dir.path())
            .args(args.iter().copied())
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
    };
    git(&["commit", "--allow-empty", "-qm", "init"]);
    git(&["switch", "-qc", "feat"]);
    std::fs::create_dir_all(dir.path().join(".devkit")).unwrap();
    std::fs::write(
        dir.path().join(".devkit/issue.toml"),
        "issue = \"ENG-42\"\nslug = \"fix-it\"\napps = []\n",
    )
    .unwrap();

    let out = run_in(dir.path(), &["task", "tagged", "--dry-run"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--tag=ENG-42/fix-it@feat"), "{stdout}");
}

#[test]
fn an_arg_overrides_an_issue_field() {
    let dir = setup();
    let out = run_in(dir.path(), &[
        "task",
        "issue-only",
        "--arg",
        "issue=ENG-9",
        "--dry-run",
    ]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("--issue=ENG-9"),
        "{out:?}"
    );
}

#[test]
fn a_malformed_task_reports_its_shape_before_its_args() {
    let dir = setup();
    let out = run_in(dir.path(), &["task", "both", "--dry-run"]);
    assert!(!out.status.success(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("sets both"), "{stderr}");
}

#[test]
fn the_listing_marks_a_task_whose_template_does_not_compile_invalid() {
    let dir = setup();
    let out = run_in(dir.path(), &["task"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let broken = stdout
        .lines()
        .find(|l| l.contains("broken"))
        .unwrap_or_else(|| panic!("broken row missing: {stdout}"));
    assert!(broken.contains("invalid"), "{broken}");
}

#[test]
fn task_listing_shows_the_args_a_task_reads() {
    let dir = setup();
    let out = run_in(dir.path(), &["task"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let commit = stdout
        .lines()
        .find(|l| l.contains("commit with a scope"))
        .unwrap_or_else(|| panic!("commit row missing: {stdout}"));
    assert!(commit.contains("msg"), "{commit}");
}

#[test]
fn task_runs_and_propagates_exit_codes() {
    let dir = setup();
    let ok = run_in(dir.path(), &["task", "hello"]);
    assert!(ok.status.success(), "{ok:?}");

    let bad = run_in(dir.path(), &["task", "fail"]);
    assert!(!bad.status.success(), "failing task must exit non-zero");

    let missing = run_in(dir.path(), &["task", "nope"]);
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("unknown task"),
        "{missing:?}"
    );
}
