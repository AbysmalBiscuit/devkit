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
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.args(args)
        .current_dir(project)
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("XDG_CONFIG_HOME", state.join("config"))
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("DEVKIT_CALLER", caller)
        .env_remove("DEVKIT_CONFIG");
    testenv::scrub_identity(&mut cmd);
    cmd.output().expect("spawn devkit")
}

fn human(project: &Path, args: &[&str]) -> Output {
    let state = tempfile::tempdir().unwrap();
    devkit(project, state.path(), "human", args)
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
