//! The `harness_log` doctor row.
//!
//! It exists because the runtime probe reads each `[harness]` key
//! independently, so a misspelled key changes nothing and reports nothing —
//! `HarnessSection` carries no `deny_unknown_fields`, and adding one would not
//! help, because nothing deserialises through the struct on that path. A row
//! printing what you are actually getting is what catches it, which is why it
//! reports the effective mode rather than the configured one.

use std::{
    path::Path,
    process::{Command, Output, Stdio},
};

struct Env {
    project: tempfile::TempDir,
    home: tempfile::TempDir,
}

/// A project and a private HOME. `global` is written to
/// `~/.config/devkit/config.toml` when given; `project` to `devkit.toml`.
fn env(global: Option<&str>, project_config: &str) -> Env {
    let project = tempfile::tempdir().unwrap();
    devkit_common::git::Git::fixture(project.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    std::fs::write(project.path().join("devkit.toml"), project_config).unwrap();

    let home = tempfile::tempdir().unwrap();
    if let Some(body) = global {
        let cfg = home.path().join(".config/devkit");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(cfg.join("config.toml"), body).unwrap();
    }
    Env { project, home }
}

fn doctor(e: &Env, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_devkit"))
        .arg("doctor")
        .args(args)
        .current_dir(e.project.path())
        .env("HOME", e.home.path())
        .env("XDG_STATE_HOME", e.home.path())
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_HARNESS_LOG")
        .env_remove("LINEAR_API_KEY")
        .env_remove("SLACK_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn devkit doctor")
}

/// `--json` emits an array of rows; this is the one named `harness_log`.
fn row(e: &Env) -> serde_json::Value {
    let out = doctor(e, &["--json"]);
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|err| {
        panic!(
            "doctor --json must parse: {err}\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    rows.as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "harness_log")
        .expect("doctor reports a harness_log row")
        .clone()
}

fn human(e: &Env) -> String {
    String::from_utf8_lossy(&doctor(e, &[]).stdout).into_owned()
}

fn enabled_global(dir: &Path, extra: &str) -> String {
    format!(
        "[harness.log]\nenabled = true\ndir = {:?}\n{extra}",
        dir.to_string_lossy()
    )
}

#[test]
fn the_row_reports_the_effective_mode_not_the_configured_one() {
    let logs = tempfile::tempdir().unwrap();
    let e = env(
        Some(&enabled_global(logs.path(), "command = \"full\"\n")),
        "[harness.log]\ncommand = \"hashed\"\n",
    );
    let row = row(&e);
    assert_eq!(
        row["command"], "hashed",
        "a project layer lowered it, so that is what is in force"
    );
    assert_eq!(row["enabled"], true);
}

#[test]
fn full_fidelity_is_marked_as_a_warning() {
    let logs = tempfile::tempdir().unwrap();
    let e = env(
        Some(&enabled_global(logs.path(), "command = \"full\"\n")),
        "[harness]\n",
    );
    assert_eq!(row(&e)["status"], "warn");
    let out = human(&e);
    let line = out
        .lines()
        .find(|l| l.contains("harness_log"))
        .expect("a harness_log row");
    assert!(line.contains("full"), "{line}");
    assert!(
        line.contains('\u{26a0}'),
        "full command text warrants a marker: {line}"
    );
}

#[test]
fn the_row_says_when_no_global_config_was_found() {
    let e = env(None, "[harness.log]\nenabled = true\ncommand = \"full\"\n");
    let row = row(&e);
    assert_eq!(row["global_config"], false);
    assert_eq!(
        row["enabled"], false,
        "a project layer cannot enable logging, and the row says what is in force"
    );
    assert_eq!(row["command"], "redacted", "nor raise the fidelity");
    assert_eq!(row["status"], "unset");
}

#[test]
fn the_row_names_the_resolved_directory_and_its_size() {
    let logs = tempfile::tempdir().unwrap();
    let day = logs.path().join("2020-01-01");
    std::fs::create_dir_all(&day).unwrap();
    std::fs::write(day.join("s1.jsonl"), "x".repeat(1234)).unwrap();
    let e = env(Some(&enabled_global(logs.path(), "")), "[harness]\n");
    let row = row(&e);
    assert_eq!(row["dir"], logs.path().to_string_lossy().as_ref());
    assert_eq!(row["bytes"], 1234);
    assert!(
        human(&e).contains(&logs.path().to_string_lossy().to_string()),
        "the human view names it too"
    );
}

/// The default modes, reported as the defaults rather than as absent: neither
/// unsafe mode is reached by accident, and the row is where you check.
#[test]
fn the_row_reports_the_defaults_when_nothing_lowers_them() {
    let logs = tempfile::tempdir().unwrap();
    let e = env(Some(&enabled_global(logs.path(), "")), "[harness]\n");
    let row = row(&e);
    assert_eq!(row["command"], "redacted");
    assert_eq!(row["prompt"], "off");
    assert_eq!(row["status"], "ok");
}
