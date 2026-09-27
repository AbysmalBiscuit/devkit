//! `devkit run task` end-to-end: listing, dry-run rendering with a registry-
//! allocated port, execution, and exit-code propagation. Drives `devkit run`
//! directly (not the `devrun` shim). Uses an isolated HOME/XDG_STATE_HOME so
//! the port registry never touches the real one.

mod common;

use std::{path::Path, process::Command};

/// A temp dir that is a git repo (cmd_task resolves the worktree root) with a
/// devkit.toml defining one app and three tasks.
fn setup() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let git = |args: &[&str]| {
        devkit_git::Git::fixture(root)
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
scope = { default = "devkit", description = "area of the codebase the commit touches" }
msg = { description = "imperative summary of the change" }

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

[tasks.pinned-commit]
description = "commit that will not take the default scope"
run = ["git", "--msg={{ scope }}: {{ msg }}", "version"]
required_args = { scope = "agents" }

[tasks.typo-required]
run = ["git", "--msg={{ msg }}", "version"]
required_args = { nope = "always" }
"#,
    )
    .expect("write devkit.toml");
    dir
}

fn run_in(dir: &Path, args: &[&str]) -> std::process::Output {
    devkit_run_in(dir)
        .args(args)
        .output()
        .expect("run devkit run")
}

/// Whether a real run executed command step `step`, judged by the exec marker
/// `devrun task` writes to stderr right before it spawns one.
fn ran(stderr: &str, step: &str) -> bool {
    stderr.contains(&format!("-> {step}: "))
}

fn config_in(dir: &Path, args: &[&str]) -> std::process::Output {
    devkit_in(dir)
        .arg("config")
        .args(args)
        .output()
        .expect("run devkit config")
}

fn devkit_run_in(dir: &Path) -> Command {
    let mut cmd = devkit_in(dir);
    cmd.arg("run");
    cmd
}

/// `devkit` sandboxed to `dir`, with no caller override inherited from the
/// developer's shell.
fn devkit_in(dir: &Path) -> Command {
    let state = dir.join("state");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.current_dir(dir)
        .env("HOME", dir)
        .env("XDG_STATE_HOME", &state)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("LOCALAPPDATA", &state) // Windows: keep the registry off the real one
        .env("USERPROFILE", dir) // Windows: keep config resolution off the real home
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CALLER");
    cmd
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

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !ran(&stderr, "hello"),
        "dry-run must not execute steps: {stderr}"
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
        !ran(&stderr, "hello"),
        "the first step ran before the missing arg was reported: {stderr}"
    );
}

#[test]
fn a_missing_arg_error_describes_what_to_pass() {
    let dir = setup();
    let out = run_in(dir.path(), &["task", "commit"]);
    assert!(!out.status.success(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("msg: imperative summary of the change"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("area of the codebase"),
        "only the missing args are described: {stderr}"
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
        devkit_git::Git::fixture(dir.path())
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
fn an_agents_marking_binds_a_run_with_no_terminal() {
    let dir = setup();
    // The test harness has no TTY, so the run classifies as an agent.
    let out = run_in(dir.path(), &[
        "task",
        "pinned-commit",
        "--arg",
        "msg=fix",
        "--dry-run",
    ]);
    assert!(!out.status.success(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--arg scope=..."), "{stderr}");
    assert!(
        stderr.contains("required for agents"),
        "the audience is named: {stderr}"
    );
}

#[test]
fn the_same_run_as_a_human_takes_the_default() {
    let dir = setup();
    let out = devkit_run_in(dir.path())
        .args(["task", "pinned-commit", "--arg", "msg=fix", "--dry-run"])
        .env("DEVKIT_CALLER", "human")
        .output()
        .expect("run devkit run");
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("argv: git --msg=devkit: fix version"),
        "{out:?}"
    );
}

#[test]
fn a_blank_value_does_not_supply_a_required_arg() {
    let dir = setup();
    let out = run_in(dir.path(), &[
        "task",
        "pinned-commit",
        "--arg",
        "msg=fix",
        "--arg",
        "scope=",
        "--dry-run",
    ]);
    assert!(!out.status.success(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--arg scope=... (required for agents)"),
        "{stderr}"
    );
}

#[test]
fn a_whitespace_value_does_not_supply_an_arg_with_no_default() {
    let dir = setup();
    let out = devkit_run_in(dir.path())
        .args(["task", "commit", "--arg", "msg=  ", "--dry-run"])
        .env("DEVKIT_CALLER", "human")
        .output()
        .expect("run devkit run");
    assert!(!out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--arg msg=..."),
        "{out:?}"
    );
}

#[test]
fn a_blank_value_still_overrides_the_default_of_an_optional_arg() {
    let dir = setup();
    let out = run_in(dir.path(), &[
        "task",
        "commit",
        "--arg",
        "msg=fix",
        "--arg",
        "scope=",
        "--dry-run",
    ]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("argv: git --msg=: fix version"),
        "{out:?}"
    );
}

#[test]
fn the_listing_is_caller_relative() {
    let dir = setup();
    let agent = run_in(dir.path(), &["task"]);
    let stdout = String::from_utf8_lossy(&agent.stdout);
    let row = stdout
        .lines()
        .find(|l| l.contains("will not take the default scope"))
        .unwrap_or_else(|| panic!("row missing: {stdout}"));
    assert!(
        row.contains("scope") && !row.contains("[scope]"),
        "agent sees it required: {row}"
    );
}

#[test]
fn a_required_args_name_the_task_never_reads_is_invalid() {
    let dir = setup();
    let out = run_in(dir.path(), &["task", "typo-required", "--arg", "msg=x"]);
    assert!(!out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("nope"),
        "{out:?}"
    );

    let listing = run_in(dir.path(), &["task"]);
    let stdout = String::from_utf8_lossy(&listing.stdout);
    let row = stdout
        .lines()
        .find(|l| l.contains("typo-required"))
        .unwrap_or_else(|| panic!("row missing: {stdout}"));
    assert!(row.contains("invalid"), "{row}");
}

/// Tasks whose shape `devrun task <name>` refuses, each with a fragment of
/// the refusal.
const MALFORMED: [(&str, &str); 3] = [
    ("seq-with-app", "may only set"),
    ("empty-split", "split with an empty `on`"),
    ("unknown-step", "unknown task `nope`"),
];

fn malformed_setup() -> tempfile::TempDir {
    let dir = setup();
    std::fs::write(
        dir.path().join("devkit.toml"),
        r#"[apps.api]
base_port = 39140
path = "."
launch = ["git", "version"]
[tasks.hello]
run = ["git", "version"]
[tasks.seq-with-app]
app = "api"
steps = [{ task = "hello" }]
[tasks.empty-split]
run = ["git", "add", { split = "{{ files }}", on = "" }]
[tasks.unknown-step]
steps = [{ task = "nope" }]
"#,
    )
    .unwrap();
    dir
}

#[test]
fn every_listing_marks_a_malformed_task_invalid_with_the_reason_resolve_gives() {
    let dir = malformed_setup();
    let listing = run_in(dir.path(), &["task"]);
    assert!(listing.status.success(), "{listing:?}");
    let listing = String::from_utf8_lossy(&listing.stdout);
    let json = config_in(dir.path(), &["tasks", "--json"]);
    assert!(json.status.success(), "{json:?}");
    let json: serde_json::Value = serde_json::from_slice(&json.stdout).expect("json");

    for (name, reason) in MALFORMED {
        let resolved = run_in(dir.path(), &["task", name, "--arg", "files=a", "--dry-run"]);
        assert!(!resolved.status.success(), "{name}: {resolved:?}");
        assert!(
            String::from_utf8_lossy(&resolved.stderr).contains(reason),
            "{name}: {resolved:?}"
        );

        assert_eq!(row(&listing, name)[1], "invalid", "{name}: {listing}");

        let entry = json
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing: {json}"));
        assert_eq!(entry["kind"], "invalid", "{entry}");
        assert!(
            entry["error"].as_str().is_some_and(|e| e.contains(reason)),
            "{entry}"
        );
    }
    assert_eq!(row(&listing, "hello")[1], "command", "{listing}");
}

#[test]
fn doctor_names_each_malformed_task() {
    let dir = malformed_setup();
    let out = devkit_in(dir.path())
        .args(["doctor", "--json"])
        .env_remove("DEVKIT_CONFIG")
        .env_remove("LINEAR_API_KEY")
        .env_remove("SLACK_TOKEN")
        .output()
        .expect("run devkit doctor");
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let tasks = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "tasks")
        .unwrap_or_else(|| panic!("no tasks row: {rows}"));
    assert_eq!(tasks["status"], "invalid", "{tasks}");
    let detail = tasks["detail"].as_str().unwrap_or_default();
    for (name, _) in MALFORMED {
        assert!(detail.contains(name), "{name}: {tasks}");
    }
    assert!(!detail.contains("hello"), "{tasks}");
}

/// A sequence gated only through its step: the brief and `devkit config
/// tasks` both name the server it needs.
#[test]
fn the_brief_and_the_description_agree_on_a_sequences_live_needs() {
    let dir = setup();
    std::fs::write(
        dir.path().join("devkit.toml"),
        r#"[apps.api]
base_port = 39140
path = "."
launch = ["git", "version"]
[tasks.e2e]
run = ["git", "--url=http://localhost:{{ ports['api'] }}", "version"]
require_live = ["api"]
[tasks.check]
description = "end to end"
steps = [{ task = "e2e" }]
[tasks.ship]
steps = [{ up = "api" }, { task = "e2e" }]
"#,
    )
    .unwrap();

    let described = config_in(dir.path(), &["tasks", "check"]);
    assert!(described.status.success(), "{described:?}");
    let described = String::from_utf8_lossy(&described.stdout);
    assert_eq!(
        row(&described, "needs"),
        ["needs", "live", "api", "(devrun", "up", "api)"],
        "{described}"
    );

    let brief = devkit_in(dir.path())
        .arg("brief")
        .env_remove("DEVKIT_CONFIG")
        .output()
        .expect("run devkit brief");
    assert!(brief.status.success(), "{brief:?}");
    let brief = String::from_utf8_lossy(&brief.stdout);
    let check = brief
        .lines()
        .find(|l| l.contains("check: end to end"))
        .unwrap_or_else(|| panic!("check line missing: {brief}"));
    assert!(check.contains("`devrun up api`"), "{brief}");

    // A sequence that brings the server up itself needs nothing beforehand.
    let ship = config_in(dir.path(), &["tasks", "ship", "--json"]);
    let ship: serde_json::Value = serde_json::from_slice(&ship.stdout).expect("json");
    assert_eq!(ship["require_live"], serde_json::json!([]), "{ship}");
    let ship_line = brief
        .lines()
        .find(|l| l.contains("ship"))
        .unwrap_or_else(|| panic!("ship line missing: {brief}"));
    assert!(!ship_line.contains("needs"), "{brief}");
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

/// Command tasks that each append a value to `probe.step` in the `observed`
/// file when they run, one of them gated on a live `api`, and sequences over
/// them. `api` launches a real TCP server on its allocated port.
fn recording_setup() -> tempfile::TempDir {
    let dir = setup();
    let launch = serde_json::to_string(&common::tcp_server_argv("{{ port }}")).unwrap();
    let config = r#"[apps.api]
base_port = @PORT@
path = "."
launch = @LAUNCH@
[tasks.first]
run = ["git", "config", "--file", "observed", "--add", "probe.step", "first"]
[tasks.second]
run = ["git", "config", "--file", "observed", "--add", "probe.step", "second"]
[tasks.gated]
run = ["git", "config", "--file", "observed", "--add", "probe.step", "gated:{{ ports['api'] }}"]
require_live = ["api"]
[tasks.reversed]
steps = [{ task = "second" }, { task = "first" }]
[tasks.check]
steps = [{ task = "first" }, { task = "gated" }, { task = "second" }]
[tasks.ship]
steps = [{ up = "api" }, { task = "gated" }]
"#
    .replace("@PORT@", &common::free_port().to_string())
    .replace("@LAUNCH@", &launch);
    std::fs::write(dir.path().join("devkit.toml"), config).unwrap();
    dir
}

/// The values the run steps appended to `observed`, in the order they ran.
fn recorded(dir: &Path) -> Vec<String> {
    if !dir.join("observed").exists() {
        return Vec::new();
    }
    devkit_git::Git::fixture(dir)
        .args(["config", "--file", "observed", "--get-all", "probe.step"])
        .output()
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn a_sequence_runs_its_steps_in_the_order_it_lists_them() {
    let dir = recording_setup();
    let out = run_in(dir.path(), &["task", "reversed"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(recorded(dir.path()), ["second", "first"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(ran(&stderr, "second") && ran(&stderr, "first"), "{stderr}");
}

#[test]
fn a_gated_task_with_no_live_server_runs_nothing() {
    let dir = recording_setup();
    let out = run_in(dir.path(), &["task", "gated"]);
    assert!(!out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no live server"),
        "{out:?}"
    );
    assert_eq!(recorded(dir.path()), Vec::<String>::new());
}

#[test]
fn a_gated_step_with_no_live_server_stops_the_sequence_where_it_stands() {
    let dir = recording_setup();
    let out = run_in(dir.path(), &["task", "check"]);
    assert!(!out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no live server"),
        "{out:?}"
    );
    assert_eq!(recorded(dir.path()), ["first"]);
}

/// Stops the servers a test brought up, including when the test fails.
#[cfg(unix)]
struct StopServers<'a>(&'a Path);

#[cfg(unix)]
impl Drop for StopServers<'_> {
    fn drop(&mut self) {
        let _ = run_in(self.0, &["down"]);
    }
}

/// The port the registry records a live `app` server on.
#[cfg(unix)]
fn live_port(dir: &Path, app: &str) -> String {
    let body = std::fs::read_to_string(dir.join("state/devkit/ports.json")).unwrap();
    let data: serde_json::Value = serde_json::from_str(&body).unwrap();
    data["entries"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, e)| e["app"] == app && e["pid"].is_u64())
        .unwrap_or_else(|| panic!("no live `{app}` row: {body}"))
        .0
        .clone()
}

// The fixture server runs on `python3`, which only Unix runners are assumed to
// have, as in `supervision.rs`.
#[cfg(unix)]
#[test]
fn a_step_after_up_passes_its_gate_and_renders_the_port_up_bound() {
    let dir = recording_setup();
    let _stop = StopServers(dir.path());
    let out = run_in(dir.path(), &["task", "ship"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(recorded(dir.path()), [format!(
        "gated:{}",
        live_port(dir.path(), "api")
    )]);
}

#[test]
fn task_split_passes_selected_paths_as_separate_arguments() {
    let dir = setup();
    std::fs::write(
        dir.path().join("devkit.toml"),
        r#"[tasks.stage]
run = ["git", "add", "--", { split = "{{ files }}", on = ";" }]
"#,
    )
    .unwrap();
    for name in ["new file.txt", "$(touch injected).txt", "unselected.txt"] {
        std::fs::write(dir.path().join(name), name).unwrap();
    }

    let out = run_in(dir.path(), &[
        "task",
        "stage",
        "--arg",
        "files=new file.txt;$(touch injected).txt",
    ]);
    assert!(out.status.success(), "{out:?}");
    let staged = devkit_git::Git::fixture(dir.path())
        .args(["diff", "--cached", "--name-only", "-z"])
        .output()
        .unwrap();
    assert_eq!(staged, "$(touch injected).txt\0new file.txt\0");
    assert!(!dir.path().join("injected").exists());
}

#[test]
fn task_split_of_an_empty_value_adds_no_arguments() {
    let dir = setup();
    std::fs::write(
        dir.path().join("devkit.toml"),
        r#"[templates.variables]
extra = ""

[tasks.show]
run = ["git", "config", "--file", "observed", "probe.value", "set", { split = "{{ extra }}", on = ";" }]
"#,
    )
    .unwrap();
    let out = run_in(dir.path(), &["task", "show", "--arg", "extra="]);
    assert!(out.status.success(), "{out:?}");
    let recorded = devkit_git::Git::fixture(dir.path())
        .args(["config", "--file", "observed", "--get", "probe.value"])
        .output()
        .unwrap();
    assert_eq!(recorded, "set\n");
}

#[test]
fn task_split_shape_rules_are_enforced() {
    for (run, expected) in [
        (
            r#"[{ split = "{{ program }}", on = ";" }, "version"]"#,
            "program must be a plain string",
        ),
        (
            r#"["git", "version", { split = "{{ program }}", on = "" }]"#,
            "split with an empty `on`",
        ),
    ] {
        let dir = setup();
        std::fs::write(
            dir.path().join("devkit.toml"),
            format!("[tasks.invalid]\nrun = {run}\n"),
        )
        .unwrap();
        let out = run_in(dir.path(), &["task", "invalid", "--arg", "program=git"]);
        assert!(!out.status.success(), "{run}: {out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(expected),
            "{run}: {out:?}"
        );
    }
}

#[test]
fn task_split_template_is_scanned_for_args_and_ports() {
    let dir = setup();
    std::fs::write(
        dir.path().join("devkit.toml"),
        r#"[apps.api]
base_port = 39140
path = "."
launch = ["git", "version"]
[tasks.show]
run = ["git", "config", "--file", "observed", "probe.value", { split = "{{ prefix }}{{ ports['api'] }}", on = ";" }]
require_live = ["api"]
"#,
    )
    .unwrap();
    let listing = run_in(dir.path(), &["task"]);
    assert!(listing.status.success(), "{listing:?}");
    assert!(String::from_utf8_lossy(&listing.stdout).contains("prefix"));

    let dry = run_in(dir.path(), &[
        "task",
        "show",
        "--arg",
        "prefix=port-",
        "--dry-run",
    ]);
    assert!(dry.status.success(), "{dry:?}");
    assert!(
        String::from_utf8_lossy(&dry.stdout).contains("probe.value port-391"),
        "{dry:?}"
    );
    assert!(!dir.path().join("observed").exists());

    let gated = run_in(dir.path(), &["task", "show", "--arg", "prefix=port-"]);
    assert!(!gated.status.success(), "{gated:?}");
    assert!(
        String::from_utf8_lossy(&gated.stderr).contains("no live server"),
        "{gated:?}"
    );
}

/// The whitespace-split columns of the table row whose first column is `name`.
fn row<'a>(stdout: &'a str, name: &str) -> Vec<&'a str> {
    stdout
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .find(|cols| cols.first() == Some(&name))
        .unwrap_or_else(|| panic!("no `{name}` row: {stdout}"))
}

#[test]
fn describing_a_task_shows_its_command_and_each_arg() {
    let dir = setup();
    let out = config_in(dir.path(), &["tasks", "commit"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("commit with a scope"), "{stdout}");
    assert!(
        stdout.contains("git --msg={{ scope }}: {{ msg }} version"),
        "{stdout}"
    );
    assert_eq!(
        row(&stdout, "usage"),
        ["usage", "devrun", "task", "commit", "--arg", "msg=..."],
        "{stdout}"
    );
    let msg = row(&stdout, "msg");
    assert_eq!(msg[1..3], ["always", "none"], "{stdout}");
    assert!(
        msg.join(" ").ends_with("imperative summary of the change"),
        "{stdout}"
    );
    let scope = row(&stdout, "scope");
    assert_eq!(scope[1..3], ["no", "devkit"], "{stdout}");
    assert!(
        scope
            .join(" ")
            .ends_with("area of the codebase the commit touches"),
        "{stdout}"
    );
}

/// A task gated on a live server, and a sequence reaching it as a step.
fn gated_setup() -> tempfile::TempDir {
    let dir = setup();
    std::fs::write(
        dir.path().join("devkit.toml"),
        r#"[apps.api]
base_port = 39140
path = "."
launch = ["git", "version"]
[templates.variables]
targets = "linux,macos"
[tasks.build]
app = "api"
run = ["git", "--url=http://localhost:{{ ports['api'] }}", { split = "{{ targets }}", on = "," }]
require_live = ["api"]
[tasks.ship]
steps = [{ up = "api" }, { task = "build" }]
"#,
    )
    .unwrap();
    dir
}

#[test]
fn describing_a_gated_task_names_the_server_it_needs_and_how_to_start_it() {
    let dir = gated_setup();
    let out = config_in(dir.path(), &["tasks", "build"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        row(&stdout, "needs"),
        ["needs", "live", "api", "(devrun", "up", "api)"],
        "{stdout}"
    );
    assert!(
        stdout.contains(r#"{{ targets }}(split on "," into separate args)"#),
        "{stdout}"
    );
    assert_eq!(
        row(&stdout, "usage"),
        ["usage", "devrun", "task", "build"],
        "{stdout}"
    );

    let seq = config_in(dir.path(), &["tasks", "ship"]);
    assert!(seq.status.success(), "{seq:?}");
    let seq = String::from_utf8_lossy(&seq.stdout);
    assert_eq!(row(&seq, "steps"), ["steps", "up", "api"], "{seq}");
    assert!(
        seq.lines().any(|l| {
            let l = l.trim();
            l.starts_with("task build: git --url=") && l.ends_with("(needs live api)")
        }),
        "{seq}"
    );
}

#[test]
fn describing_a_gated_task_as_json_carries_the_gate_and_each_step() {
    let dir = gated_setup();
    let out = config_in(dir.path(), &["tasks", "build", "--json"]);
    assert!(out.status.success(), "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(v["app"], "api");
    assert_eq!(
        v["require_live"],
        serde_json::json!([{ "app": "api", "start": "devrun up api" }])
    );

    let out = config_in(dir.path(), &["tasks", "ship", "--json"]);
    assert!(out.status.success(), "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(v["app"], serde_json::Value::Null);
    assert_eq!(v["steps"][0], serde_json::json!({ "up": "api" }));
    assert_eq!(v["steps"][1]["task"], "build");
    assert_eq!(v["steps"][1]["run"][0], "git");
    assert_eq!(
        v["steps"][1]["require_live"],
        serde_json::json!([{ "app": "api", "start": "devrun up api" }])
    );
}

#[test]
fn describing_a_task_as_json_carries_its_templates_and_args() {
    let dir = setup();
    let out = config_in(dir.path(), &["tasks", "commit", "--json"]);
    assert!(out.status.success(), "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(v["name"], "commit");
    assert_eq!(v["kind"], "command");
    assert_eq!(
        v["run"],
        serde_json::json!(["git", "--msg={{ scope }}: {{ msg }}", "version"])
    );
    assert_eq!(v["app"], serde_json::Value::Null);
    assert_eq!(v["usage"], "devrun task commit --arg msg=...");
    assert_eq!(v["require_live"], serde_json::json!([]));
    assert_eq!(
        v["args"],
        serde_json::json!([
            {
                "name": "msg",
                "required": "always",
                "default": null,
                "description": "imperative summary of the change",
            },
            {
                "name": "scope",
                "required": "never",
                "default": "devkit",
                "description": "area of the codebase the commit touches",
            },
        ])
    );
}

#[test]
fn describing_a_sequence_lists_its_steps_and_their_args() {
    let dir = setup();
    let out = config_in(dir.path(), &["tasks", "seq-commit"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        row(&stdout, "steps"),
        ["steps", "task", "hello:", "git", "version"],
        "{stdout}"
    );
    assert!(
        stdout.contains("task commit: git --msg={{ scope }}: {{ msg }} version"),
        "{stdout}"
    );
    assert_eq!(row(&stdout, "msg")[1], "always", "{stdout}");
}

#[test]
fn describing_a_task_names_who_must_pass_an_arg_and_what_this_caller_passes() {
    let dir = setup();
    let agent = config_in(dir.path(), &["tasks", "pinned-commit"]);
    let human = devkit_in(dir.path())
        .args(["config", "tasks", "pinned-commit"])
        .env("DEVKIT_CALLER", "human")
        .output()
        .expect("run devkit config");
    let agent = String::from_utf8_lossy(&agent.stdout);
    let human = String::from_utf8_lossy(&human.stdout);
    assert_eq!(row(&agent, "scope")[1], "agents", "{agent}");
    assert_eq!(row(&human, "scope")[1], "agents", "{human}");
    assert!(row(&agent, "usage").contains(&"scope=..."), "{agent}");
    assert!(!row(&human, "usage").contains(&"scope=..."), "{human}");
}

#[test]
fn describing_a_malformed_or_unknown_task_says_what_is_wrong() {
    let dir = setup();
    let both = config_in(dir.path(), &["tasks", "both"]);
    assert!(!both.status.success(), "{both:?}");
    assert!(
        String::from_utf8_lossy(&both.stderr).contains("sets both"),
        "{both:?}"
    );

    let unknown = config_in(dir.path(), &["tasks", "nope"]);
    assert!(!unknown.status.success(), "{unknown:?}");
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("unknown task `nope`"),
        "{unknown:?}"
    );
}

#[test]
fn config_variables_lists_each_declared_variable() {
    let dir = setup();
    let out = config_in(dir.path(), &["variables"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let msg = row(&stdout, "msg");
    assert_eq!(msg[1..3], ["always", "none"], "{stdout}");
    assert!(
        msg.join(" ").ends_with("imperative summary of the change"),
        "{stdout}"
    );
    assert_eq!(row(&stdout, "scope")[1..3], ["no", "devkit"], "{stdout}");

    let json = config_in(dir.path(), &["variables", "--json"]);
    assert!(json.status.success(), "{json:?}");
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).expect("json");
    assert_eq!(
        v[1],
        serde_json::json!({
            "name": "scope",
            "required": "never",
            "default": "devkit",
            "description": "area of the codebase the commit touches",
        })
    );
}

#[test]
fn arg_file_passes_a_multi_line_value_from_a_file() {
    let dir = setup();
    std::fs::write(dir.path().join("body.md"), "line one\nline two\n").unwrap();
    let out = run_in(dir.path(), &[
        "task",
        "commit",
        "--arg-file",
        "msg=body.md",
        "--dry-run",
    ]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--msg=devkit: line one\nline two\n version"),
        "the file's contents, trailing newline included: {stdout}"
    );
}

#[test]
fn arg_file_dash_reads_the_value_from_stdin() {
    use std::io::Write;

    let dir = setup();
    let mut child = devkit_run_in(dir.path())
        .args(["task", "commit", "--arg-file", "msg=-", "--dry-run"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn devkit run");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"from\nstdin\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--msg=devkit: from\nstdin\n version"),
        "{stdout}"
    );
}

#[test]
fn naming_one_key_through_arg_and_arg_file_is_refused() {
    let dir = setup();
    std::fs::write(dir.path().join("body.md"), "x").unwrap();
    let out = run_in(dir.path(), &[
        "task",
        "commit",
        "--arg",
        "msg=inline",
        "--arg-file",
        "msg=body.md",
        "--dry-run",
    ]);
    assert!(!out.status.success(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("`msg`"), "{stderr}");
}
