//! `devkit rules` against an index passed by path, so no cache lookup is
//! involved and the test is hermetic.
//!
//! Isolated the same way `tests/locks.rs` is: a private `HOME`/`XDG_STATE_HOME`
//! and `DEVKIT_SKIP_AUTOLINK`, since the binary migrates legacy state and
//! links shims at startup regardless of the subcommand.

use std::{path::Path, process::Command};

fn devkit(state: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("DEVKIT_SKIP_AUTOLINK", "1");
    cmd
}

fn fixture_index(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("index.json");
    std::fs::copy("crates/devkit-rules/tests/fixtures/index.json", &path).unwrap();
    path
}

#[test]
fn query_filters_by_path_and_prints_json() {
    let dir = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let index = fixture_index(dir.path());
    let out = devkit(state.path())
        .args(["rules", "query", index.to_str().unwrap()])
        .args(["--path", "crates/foo/src/a.rs", "--format", "json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = String::from_utf8(out.stdout).unwrap();
    assert!(body.contains("r-foo-should"), "{body}");
    assert!(
        !body.contains("r-bad-severity"),
        "off-vocabulary rule: {body}"
    );
}

#[test]
fn stats_reports_counts_and_breakdowns() {
    let dir = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let index = fixture_index(dir.path());
    let out = devkit(state.path())
        .args(["rules", "stats", index.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = String::from_utf8(out.stdout).unwrap();
    assert!(body.contains("rules"), "{body}");
    assert!(body.contains("must"), "the severity breakdown: {body}");
}

#[test]
fn a_missing_index_exits_nonzero_with_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let out = devkit(state.path())
        .args([
            "rules",
            "stats",
            dir.path().join("absent.json").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no rules index"));
}
