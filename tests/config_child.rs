//! A devkit child resolves the config file its parent selected with
//! `--config`, unless it selects its own or runs in another checkout. Each
//! test drives the built `devkit` against real git repos with a private
//! HOME/XDG_STATE_HOME.

#[path = "common/testenv.rs"]
mod testenv;

use std::{
    path::Path,
    process::{Command, Output},
};

use serde_json::Value;

/// A git repo whose `devkit.toml` sets `[tracker] kind` to `kind`.
fn project(kind: &str) -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    devkit_git::Git::fixture(p.path())
        .args(["init", "-q"])
        .output()
        .unwrap();
    std::fs::write(
        p.path().join("devkit.toml"),
        format!("[tracker]\nkind = \"{kind}\"\n"),
    )
    .unwrap();
    p
}

/// Harness logging on, in the form only a global config may set it.
const LOGGING_ON: &str = "[harness.log]\nenabled = true\ncommand = \"hashed\"\n";

/// Write `ci.toml` into `p`: tracker `none`, and a `show` task whose step
/// runs in `apps/api` with `child_args` after `devkit`.
fn write_ci_config(p: &Path, child_args: &[&str]) {
    let exe = env!("CARGO_BIN_EXE_devkit");
    std::fs::create_dir_all(p.join("apps/api")).unwrap();
    let args: String = child_args.iter().map(|a| format!(", '{a}'")).collect();
    std::fs::write(
        p.join("ci.toml"),
        format!(
            "[tracker]\nkind = \"none\"\n\n\
             [apps.api]\nbase_port = 39180\npath = \"apps/api\"\nlaunch = [\"git\", \"version\"]\n\n\
             [tasks.show]\napp = \"api\"\nrun = ['{exe}'{args}]\n"
        ),
    )
    .unwrap();
}

/// `devkit run --config ci.toml task show` in `p` under a private HOME whose
/// home config is `home_config`, with each of `env` set. Returns the step's
/// stdout.
fn run_show(p: &Path, home_config: Option<&str>, env: &[(&str, &Path)]) -> String {
    let state = tempfile::tempdir().unwrap();
    if let Some(body) = home_config {
        let dir = state.path().join(".config/devkit");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), body).unwrap();
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.args(["run", "--config", "ci.toml", "task", "show"])
        .current_dir(p)
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .env("XDG_CONFIG_HOME", state.path().join("config"))
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("DEVKIT_CALLER", "human")
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_PARENT_CONFIG")
        .env_remove("DEVKIT_PARENT_CONFIG_CHECKOUT")
        .env_remove("DEVKIT_OPTIONS")
        .env_remove("DEVKIT_HARNESS_LOG")
        .envs(env.iter().copied());
    testenv::scrub_identity(&mut cmd);
    let out: Output = cmd.output().expect("spawn devkit");
    let shown = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "devkit failed: {}{shown}",
        String::from_utf8_lossy(&out.stderr)
    );
    shown.into_owned()
}

/// The tracker kind the step `devkit config show --json` reports, with
/// `child_args` after it.
fn child_tracker_kind(p: &Path, child_args: &[&str]) -> String {
    let mut args = vec!["config", "show", "--json"];
    args.extend(child_args);
    write_ci_config(p, &args);
    let shown = run_show(p, None, &[]);
    let json = &shown[shown.find('{').unwrap()..=shown.rfind('}').unwrap()];
    let cfg: serde_json::Value = serde_json::from_str(json).unwrap();
    cfg["tracker"]["kind"].as_str().unwrap().to_owned()
}

/// The `harness_log` row of the step `devkit doctor --json`.
fn child_harness_log(p: &Path, home_config: Option<&str>, env: &[(&str, &Path)]) -> Value {
    write_ci_config(p, &["doctor", "--json"]);
    let shown = run_show(p, home_config, env);
    let json = &shown[shown.find("[\n").unwrap()..=shown.rfind(']').unwrap()];
    let rows: Value = serde_json::from_str(json).unwrap_or_else(|e| panic!("{e}: {shown}"));
    rows.as_array()
        .and_then(|rows| rows.iter().find(|r| r["key"] == "harness_log"))
        .unwrap_or_else(|| panic!("no harness_log row: {shown}"))
        .clone()
}

#[test]
fn a_child_in_the_same_checkout_reads_the_parents_config_file() {
    let p = project("github");
    assert_eq!(child_tracker_kind(p.path(), &[]), "none");
}

#[test]
fn a_child_with_its_own_config_file_reads_that_file() {
    let p = project("github");
    let own = p.path().join("own.toml");
    std::fs::write(&own, "[tracker]\nkind = \"linear\"\n").unwrap();
    assert_eq!(
        child_tracker_kind(p.path(), &["--config", own.to_str().unwrap()]),
        "linear"
    );
}

#[test]
fn a_task_step_that_sets_devkit_config_reads_that_file() {
    let p = project("github");
    let own = p.path().join("own.toml");
    std::fs::write(&own, "[tracker]\nkind = \"linear\"\n").unwrap();
    write_ci_config(p.path(), &["config", "show", "--json"]);
    let mut ci = std::fs::read_to_string(p.path().join("ci.toml")).unwrap();
    ci.push_str(&format!(
        "env = {{ DEVKIT_CONFIG = '{}' }}\n",
        own.display()
    ));
    std::fs::write(p.path().join("ci.toml"), ci).unwrap();
    let shown = run_show(p.path(), None, &[]);
    let json = &shown[shown.find('{').unwrap()..=shown.rfind('}').unwrap()];
    let cfg: Value = serde_json::from_str(json).unwrap();
    assert_eq!(cfg["tracker"]["kind"], "linear", "{shown}");
}

#[test]
fn a_child_in_another_checkout_reads_that_checkouts_config() {
    let p = project("github");
    let other = project("linear");
    assert_eq!(
        child_tracker_kind(p.path(), &["-C", other.path().to_str().unwrap()]),
        "linear"
    );
}

#[test]
fn a_child_keeps_the_home_config_as_its_global_config() {
    let p = project("github");
    let row = child_harness_log(p.path(), Some(LOGGING_ON), &[]);
    assert_eq!(row["enabled"], true, "{row}");
    assert_eq!(row["command"], "hashed", "{row}");
}

#[test]
fn a_child_keeps_the_global_config_devkit_config_names() {
    let p = project("github");
    let global = tempfile::tempdir().unwrap();
    let file = global.path().join("global.toml");
    std::fs::write(&file, LOGGING_ON).unwrap();
    let row = child_harness_log(p.path(), None, &[("DEVKIT_CONFIG", &file)]);
    assert_eq!(row["enabled"], true, "{row}");
    assert_eq!(row["command"], "hashed", "{row}");
}
