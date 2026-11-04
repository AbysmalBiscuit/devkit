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

/// Write `ci.toml` into `p`: tracker `none`, and a `show` task whose step
/// runs in `apps/api` with `child_args` after `devkit config show --json`.
fn write_ci_config(p: &Path, child_args: &[&str]) {
    let exe = env!("CARGO_BIN_EXE_devkit");
    std::fs::create_dir_all(p.join("apps/api")).unwrap();
    let extra: String = child_args.iter().map(|a| format!(", '{a}'")).collect();
    std::fs::write(
        p.join("ci.toml"),
        format!(
            "[tracker]\nkind = \"none\"\n\n\
             [apps.api]\nbase_port = 39180\npath = \"apps/api\"\nlaunch = [\"git\", \"version\"]\n\n\
             [tasks.show]\napp = \"api\"\nrun = ['{exe}', 'config', 'show', '--json'{extra}]\n"
        ),
    )
    .unwrap();
}

/// `devkit run --config ci.toml task show` in `p`, the step's tracker kind.
fn child_tracker_kind(p: &Path) -> String {
    let state = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.args(["run", "--config", "ci.toml", "task", "show"])
        .current_dir(p)
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .env("XDG_CONFIG_HOME", state.path().join("config"))
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("DEVKIT_CALLER", "human")
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_CONFIG_CHECKOUT")
        .env_remove("DEVKIT_OPTIONS");
    testenv::scrub_identity(&mut cmd);
    let out: Output = cmd.output().expect("spawn devkit");
    let shown = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "devkit failed: {}{shown}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = &shown[shown.find('{').unwrap()..=shown.rfind('}').unwrap()];
    let cfg: serde_json::Value = serde_json::from_str(json).unwrap();
    cfg["tracker"]["kind"].as_str().unwrap().to_owned()
}

#[test]
fn a_child_in_the_same_checkout_reads_the_parents_config_file() {
    let p = project("github");
    write_ci_config(p.path(), &[]);
    assert_eq!(child_tracker_kind(p.path()), "none");
}

#[test]
fn a_child_with_its_own_config_file_reads_that_file() {
    let p = project("github");
    std::fs::write(p.path().join("own.toml"), "[tracker]\nkind = \"linear\"\n").unwrap();
    let own = p.path().join("own.toml");
    write_ci_config(p.path(), &["--config", own.to_str().unwrap()]);
    assert_eq!(child_tracker_kind(p.path()), "linear");
}

#[test]
fn a_child_in_another_checkout_reads_that_checkouts_config() {
    let p = project("github");
    let other = project("linear");
    write_ci_config(p.path(), &["-C", other.path().to_str().unwrap()]);
    assert_eq!(child_tracker_kind(p.path()), "linear");
}
