//! `-o/--option KEY=VALUE` sets one config value for one command, over every
//! config layer. Each test drives the built `devkit` against a real git repo
//! with a private HOME/XDG_STATE_HOME.

#[path = "common/testenv.rs"]
mod testenv;

use std::{
    path::Path,
    process::{Command, Output},
};

/// A git repo whose `devkit.toml` sets `[tracker] kind = "github"`.
fn project() -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    devkit_git::Git::fixture(p.path())
        .args(["init", "-q"])
        .output()
        .unwrap();
    std::fs::write(
        p.path().join("devkit.toml"),
        "[defaults]\nbranch_prefix = \"x/\"\n\n[tracker]\nkind = \"github\"\n",
    )
    .unwrap();
    p
}

fn devkit(project: &Path, state: &Path, caller: &str, args: &[&str]) -> Output {
    devkit_with_options(project, state, caller, None, args)
}

/// `devkit` with `DEVKIT_OPTIONS` set to `options`, or unset for `None`.
fn devkit_with_options(
    project: &Path,
    state: &Path,
    caller: &str,
    options: Option<&str>,
    args: &[&str],
) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.args(args)
        .current_dir(project)
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("XDG_CONFIG_HOME", state.join("config"))
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("DEVKIT_CALLER", caller)
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_OPTIONS");
    if let Some(options) = options {
        cmd.env("DEVKIT_OPTIONS", options);
    }
    testenv::scrub_identity(&mut cmd);
    cmd.output().expect("spawn devkit")
}

fn human(project: &Path, args: &[&str]) -> Output {
    let state = tempfile::tempdir().unwrap();
    devkit(project, state.path(), "human", args)
}

/// `devkit` run by a person with `DEVKIT_OPTIONS` set to `options`.
fn human_with_options(project: &Path, options: &str, args: &[&str]) -> Output {
    let state = tempfile::tempdir().unwrap();
    devkit_with_options(project, state.path(), "human", Some(options), args)
}

fn stdout(out: &Output) -> String {
    assert!(
        out.status.success(),
        "devkit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    assert!(!out.status.success(), "devkit succeeded");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn an_option_overrides_the_value_a_config_file_sets() {
    let p = project();
    let shown = stdout(&human(p.path(), &[
        "config",
        "show",
        "-o",
        "tracker.kind=\"none\"",
    ]));
    assert!(shown.contains("kind = \"none\""), "{shown}");
    assert!(!shown.contains("kind = \"github\""), "{shown}");
}

#[test]
fn an_option_merges_over_the_file_config_names_and_the_last_one_wins() {
    let p = project();
    let other = p.path().join("other.toml");
    std::fs::write(
        &other,
        "[defaults]\nbranch_prefix = \"from-file/\"\n\n[tracker]\nkind = \"linear\"\n",
    )
    .unwrap();
    let shown = stdout(&human(p.path(), &[
        "config",
        "show",
        "--json",
        "--config",
        other.to_str().unwrap(),
        "--option",
        "tracker.kind=\"github\"",
        "--option",
        "tracker.kind=\"none\"",
    ]));
    let cfg: serde_json::Value = serde_json::from_str(&shown).unwrap();
    assert_eq!(cfg["tracker"]["kind"], "none", "{shown}");
    assert_eq!(cfg["defaults"]["branch_prefix"], "from-file/", "{shown}");
}

#[test]
fn origin_names_the_command_line_as_the_source_of_an_option() {
    let p = project();
    let shown = stdout(&human(p.path(), &[
        "config",
        "show",
        "--origin",
        "-o",
        "tracker.kind=\"none\"",
    ]));
    let line = shown
        .lines()
        .find(|l| l.starts_with("tracker.kind ="))
        .unwrap_or_else(|| panic!("no tracker.kind line: {shown}"));
    assert!(line.contains("# from <command line>"), "{line}");
    assert!(
        line.contains("devkit.toml"),
        "the shadowed file is named: {line}"
    );
    assert!(
        shown.contains("#   <command line>"),
        "the layer list names the command line: {shown}"
    );
}

#[test]
fn a_value_that_is_not_toml_fails_naming_the_key() {
    let p = project();
    let err = stderr(&human(p.path(), &[
        "config",
        "show",
        "-o",
        "tracker.kind=none",
    ]));
    assert!(err.contains("tracker.kind"), "{err}");
}

#[test]
fn a_key_the_schema_does_not_know_fails_naming_the_key() {
    let p = project();
    let err = stderr(&human(p.path(), &[
        "config",
        "show",
        "-o",
        "tracker.knd=\"none\"",
    ]));
    assert!(err.contains("`tracker.knd`"), "{err}");
}

#[test]
fn an_agent_caller_is_refused_the_flag() {
    let p = project();
    let state = tempfile::tempdir().unwrap();
    let out = devkit(p.path(), state.path(), "agent", &[
        "config",
        "show",
        "-o",
        "tracker.kind=\"none\"",
    ]);
    let err = stderr(&out);
    assert!(err.contains("--option"), "{err}");
    assert!(out.stdout.is_empty(), "nothing ran: {:?}", out.stdout);
}

#[test]
fn doctor_reports_an_option_of_the_wrong_type_as_a_broken_config() {
    let p = project();
    let out = human(p.path(), &[
        "doctor",
        "--json",
        "-o",
        "defaults.branch_prefix=1",
    ]);
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "doctor --json: {e}: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let config = rows
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["key"] == "config"))
        .unwrap_or_else(|| panic!("no config row: {rows}"));
    assert_eq!(config["status"], "invalid", "{config}");
    assert!(
        config["detail"]
            .as_str()
            .is_some_and(|d| d.contains("branch_prefix")),
        "{config}"
    );
}

#[test]
fn a_quoted_key_segment_may_hold_an_equals_sign() {
    let p = project();
    let shown = stdout(&human(p.path(), &[
        "config",
        "show",
        "--json",
        "-o",
        "people.\"a=b\".github=\"ann\"",
    ]));
    let cfg: serde_json::Value = serde_json::from_str(&shown).unwrap();
    assert_eq!(cfg["people"]["a=b"]["github"], "ann", "{shown}");
}

#[test]
fn options_stand_alone_without_a_config_file() {
    let p = tempfile::tempdir().unwrap();
    devkit_git::Git::fixture(p.path())
        .args(["init", "-q"])
        .output()
        .unwrap();
    let shown = stdout(&human(p.path(), &[
        "config",
        "show",
        "--json",
        "-o",
        "tracker.kind=\"none\"",
    ]));
    let cfg: serde_json::Value = serde_json::from_str(&shown).unwrap();
    assert_eq!(cfg["tracker"]["kind"], "none", "{shown}");
}

fn harness_log_row(out: &Output) -> serde_json::Value {
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "doctor --json: {e}: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    rows.as_array()
        .and_then(|rows| rows.iter().find(|r| r["key"] == "harness_log"))
        .unwrap_or_else(|| panic!("no harness_log row: {rows}"))
        .clone()
}

#[test]
fn an_option_sets_harness_logging_and_a_project_layer_still_disables_it() {
    let p = project();
    let on = [
        "doctor",
        "--json",
        "-o",
        "harness.log.enabled=true",
        "-o",
        "harness.log.command=\"hashed\"",
    ];
    let row = harness_log_row(&human(p.path(), &on));
    assert_eq!(row["enabled"], true, "{row}");
    assert_eq!(row["command"], "hashed", "{row}");

    std::fs::write(
        p.path().join("devkit.local.toml"),
        "[harness.log]\nenabled = false\n",
    )
    .unwrap();
    let row = harness_log_row(&human(p.path(), &on));
    assert_eq!(
        row["enabled"], false,
        "a project layer may still disable logging: {row}"
    );
}

#[test]
fn an_unknown_harness_key_is_refused_naming_it() {
    let p = project();
    let err = stderr(&human(p.path(), &[
        "config",
        "show",
        "-o",
        "harness.enforce_writs=true",
    ]));
    assert!(err.contains("`harness.enforce_writs`"), "{err}");
}

#[test]
fn a_harness_value_of_the_wrong_type_is_refused_naming_the_key() {
    let p = project();
    for (option, key) in [
        ("harness.enforce_commands=1", "harness.enforce_commands"),
        ("harness.log.max_age_days=0", "harness.log.max_age_days"),
        (
            "harness.commands.x.programs=\"node\"",
            "harness.commands.x.programs",
        ),
    ] {
        let err = stderr(&human(p.path(), &["config", "show", "-o", option]));
        assert!(err.contains(&format!("--option {key}")), "{option}: {err}");
    }
}

#[test]
fn a_harness_option_may_set_one_key_and_the_last_value_wins() {
    let p = project();
    let row = harness_log_row(&human(p.path(), &[
        "doctor",
        "--json",
        "-o",
        "harness.log.enabled=true",
        "-o",
        "harness.log.max_age_days=0",
        "-o",
        "harness.log.max_age_days=7",
        "-o",
        "harness.pr_tools.x.enabled=false",
    ]));
    assert_eq!(row["enabled"], true, "{row}");
}

#[test]
fn an_option_may_set_the_whole_harness_table() {
    let p = project();
    let row = harness_log_row(&human(p.path(), &[
        "doctor",
        "--json",
        "-o",
        "harness={ log = { enabled = true } }",
    ]));
    assert_eq!(row["enabled"], true, "{row}");

    for option in ["harness={ enforce_commands = 1 }", "harness=1"] {
        let err = stderr(&human(p.path(), &["config", "show", "-o", option]));
        assert!(err.contains("--option harness"), "{option}: {err}");
        assert!(!err.contains("panicked"), "{option}: {err}");
    }
}

#[test]
fn a_misspelled_key_inside_a_table_value_is_refused_naming_it() {
    let p = project();
    for (option, key) in [
        ("defaults={branch_prefx=\"x/\"}", "defaults.branch_prefx"),
        (
            "harness={enforce_commandz=true}",
            "harness.enforce_commandz",
        ),
    ] {
        let err = stderr(&human(p.path(), &["config", "show", "-o", option]));
        assert!(err.contains(&format!("`{key}`")), "{option}: {err}");
    }
}

#[test]
fn a_relative_option_path_anchors_to_the_start_directory() {
    let p = project();
    let elsewhere = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let shown = stdout(&devkit(elsewhere.path(), state.path(), "human", &[
        "config",
        "show",
        "--json",
        "-C",
        p.path().to_str().unwrap(),
        "-o",
        "defaults.worktree_root=\"wts\"",
    ]));
    let cfg: serde_json::Value = serde_json::from_str(&shown).unwrap();
    let root = Path::new(cfg["defaults"]["worktree_root"].as_str().unwrap());
    assert_eq!(root.file_name().unwrap(), "wts", "{shown}");
    assert_eq!(
        root.parent().unwrap().canonicalize().unwrap(),
        p.path().canonicalize().unwrap(),
        "{shown}"
    );
}

#[test]
fn a_relative_option_log_dir_anchors_to_the_hook_directory() {
    use std::io::Write;
    let p = project();
    let elsewhere = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "s1",
        "cwd": p.path(),
        "tool_name": "Bash",
        "tool_input": { "command": "ls" }
    });
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.args([
        "hook",
        "pre-tool-use",
        "-o",
        "harness.log.enabled=true",
        "-o",
        "harness.log.dir=\"logs\"",
    ])
    .current_dir(elsewhere.path())
    .env("HOME", state.path())
    .env("XDG_STATE_HOME", state.path())
    .env("XDG_CONFIG_HOME", state.path().join("config"))
    .env("DEVKIT_SKIP_AUTOLINK", "1")
    .env("DEVKIT_CALLER", "human")
    .env("CLAUDE_CODE_SESSION_ID", "")
    .env("CODEX_SESSION_ID", "")
    .env_remove("DEVKIT_CONFIG")
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped());
    testenv::scrub_identity(&mut cmd);
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let logs = p.path().join("logs");
    assert!(
        std::fs::read_dir(&logs).is_ok_and(|mut d| d.next().is_some()),
        "no record under {}; under the process directory: {:?}",
        logs.display(),
        std::fs::read_dir(elsewhere.path().join("logs")).map(|d| d.count())
    );
}

#[test]
fn the_environment_options_override_a_config_file_and_origin_names_the_variable() {
    let p = project();
    let options = "tracker.kind=\"none\"\ndefaults.branch_prefix = \"env/\"\n";
    let shown = stdout(&human_with_options(p.path(), options, &["config", "show"]));
    assert!(shown.contains("kind = \"none\""), "{shown}");
    assert!(shown.contains("branch_prefix = \"env/\""), "{shown}");

    let shown = stdout(&human_with_options(p.path(), options, &[
        "config", "show", "--origin",
    ]));
    let line = shown
        .lines()
        .find(|l| l.starts_with("tracker.kind ="))
        .unwrap_or_else(|| panic!("no tracker.kind line: {shown}"));
    assert!(line.contains("# from $DEVKIT_OPTIONS"), "{line}");
    assert!(
        shown.contains("#   $DEVKIT_OPTIONS"),
        "the layer list names the variable: {shown}"
    );
}

#[test]
fn an_environment_option_an_agent_inherits_applies() {
    let p = project();
    let state = tempfile::tempdir().unwrap();
    let shown = stdout(&devkit_with_options(
        p.path(),
        state.path(),
        "agent",
        Some("tracker.kind=\"none\""),
        &["config", "show"],
    ));
    assert!(shown.contains("kind = \"none\""), "{shown}");
}

#[test]
fn a_bad_environment_option_fails_naming_the_variable_and_the_key() {
    let p = project();
    let err = stderr(&human_with_options(p.path(), "tracker.knd=\"none\"", &[
        "config", "show",
    ]));
    assert!(err.contains("DEVKIT_OPTIONS"), "{err}");
    assert!(err.contains("`tracker.knd`"), "{err}");
}

#[test]
fn a_command_line_option_wins_over_the_same_key_in_the_environment() {
    let p = project();
    let shown = stdout(&human_with_options(
        p.path(),
        "tracker.kind=\"linear\"\ndefaults.branch_prefix=\"env/\"",
        &["config", "show", "--origin", "-o", "tracker.kind=\"none\""],
    ));
    let line = shown
        .lines()
        .find(|l| l.starts_with("tracker.kind ="))
        .unwrap_or_else(|| panic!("no tracker.kind line: {shown}"));
    assert!(line.contains("\"none\""), "{line}");
    assert!(line.contains("# from <command line>"), "{line}");
    assert!(
        line.contains("$DEVKIT_OPTIONS"),
        "the shadowed layer: {line}"
    );
    assert!(shown.contains("branch_prefix = \"env/\""), "{shown}");
}

#[test]
fn a_task_step_sees_the_options_of_the_devkit_that_runs_it() {
    let p = project();
    let exe = env!("CARGO_BIN_EXE_devkit");
    std::fs::write(
        p.path().join("devkit.local.toml"),
        format!("[tasks.show]\nrun = ['{exe}', 'config', 'show', '--origin']\n"),
    )
    .unwrap();
    let shown = stdout(&human_with_options(
        p.path(),
        "tracker.kind=\"linear\"\ndefaults.branch_prefix=\"env/\"",
        &[
            "-o",
            "tracker.kind=\"none\"",
            "-o",
            "templates.variables.note.default=\"\"\"two\nlines\"\"\"",
            "run",
            "task",
            "show",
        ],
    ));
    let kind = shown
        .lines()
        .find(|l| l.starts_with("tracker.kind ="))
        .unwrap_or_else(|| panic!("no tracker.kind line: {shown}"));
    assert!(kind.contains("\"none\""), "{kind}");
    assert!(shown.contains("branch_prefix = \"env/\""), "{shown}");
    assert!(
        shown.contains("two") && shown.contains("lines"),
        "a value that spans lines reaches the step: {shown}"
    );
}

#[test]
fn a_task_step_in_an_app_directory_anchors_option_paths_as_its_parent_does() {
    let p = project();
    let exe = env!("CARGO_BIN_EXE_devkit");
    std::fs::create_dir_all(p.path().join("apps/api")).unwrap();
    std::fs::write(
        p.path().join("devkit.local.toml"),
        format!(
            "[apps.api]\nbase_port = 39170\npath = \"apps/api\"\nlaunch = [\"git\", \"version\"]\n\n\
             [tasks.show]\napp = \"api\"\nrun = ['{exe}', 'config', 'show', '--json']\n"
        ),
    )
    .unwrap();
    let shown = stdout(&human(p.path(), &[
        "-o",
        "defaults.worktree_root=\"wts\"",
        "-o",
        "harness.log.dir=\"logs\"",
        "run",
        "task",
        "show",
    ]));
    let json = &shown[shown.find('{').unwrap()..=shown.rfind('}').unwrap()];
    let cfg: serde_json::Value = serde_json::from_str(json).unwrap();
    let root = Path::new(cfg["defaults"]["worktree_root"].as_str().unwrap());
    assert_eq!(
        root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
        p.path().canonicalize().unwrap().join("wts"),
        "{shown}"
    );
}

#[test]
fn a_child_in_another_checkout_resolves_a_repo_relative_option_against_its_own() {
    let p = project();
    let other = project();
    let exe = env!("CARGO_BIN_EXE_devkit");
    std::fs::write(
        p.path().join("devkit.local.toml"),
        format!(
            "[tasks.show]\nrun = ['{exe}', 'config', 'show', '--json', '-C', '{}']\n",
            other.path().display()
        ),
    )
    .unwrap();
    let shown = stdout(&human(p.path(), &[
        "-o",
        "defaults.doppler_yaml=\"doppler.yaml\"",
        "run",
        "task",
        "show",
    ]));
    let json = &shown[shown.find('{').unwrap()..=shown.rfind('}').unwrap()];
    let cfg: serde_json::Value = serde_json::from_str(json).unwrap();
    let doppler = Path::new(cfg["defaults"]["doppler_yaml"].as_str().unwrap());
    assert_eq!(
        doppler.parent().unwrap().canonicalize().unwrap(),
        other.path().canonicalize().unwrap(),
        "{shown}"
    );
}
