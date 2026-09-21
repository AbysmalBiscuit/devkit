//! `devkit rules` against an index passed by path, so no cache lookup is
//! involved and the test is hermetic.
//!
//! Isolated the same way `tests/locks.rs` is: a private `HOME`/`XDG_STATE_HOME`
//! and `DEVKIT_SKIP_AUTOLINK`, since the binary migrates legacy state and
//! links shims at startup regardless of the subcommand.

#[path = "common/testenv.rs"]
mod testenv;

use std::process::Command;

fn devkit() -> Command {
    Command::new(env!("CARGO_BIN_EXE_devkit"))
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
    let out = devkit()
        .args(["rules", "query", index.to_str().unwrap()])
        .args(["--path", "crates/foo/src/a.rs", "--format", "json"])
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .env("DEVKIT_SKIP_AUTOLINK", "1")
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
    let out = devkit()
        .args(["rules", "stats", index.to_str().unwrap()])
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .env("DEVKIT_SKIP_AUTOLINK", "1")
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
    let out = devkit()
        .args([
            "rules",
            "stats",
            dir.path().join("absent.json").to_str().unwrap(),
        ])
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no rules index"));
}

fn context_project() -> (tempfile::TempDir, tempfile::TempDir) {
    let state = tempfile::tempdir().unwrap();
    let p = tempfile::tempdir().unwrap();
    devkit_common::git::Git::fixture(p.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    let index = p.path().join("index.json");
    std::fs::copy("crates/devkit-rules/tests/fixtures/index.json", &index).unwrap();
    std::fs::write(
        p.path().join("devkit.toml"),
        // A literal string: a Windows path's backslashes are not escapes.
        format!("[rules]\nenabled = true\nindex = '{}'\n", index.display()),
    )
    .unwrap();
    (p, state)
}

fn run_context(
    project: &std::path::Path,
    state: &std::path::Path,
    args: &[&str],
) -> std::process::Output {
    let mut cmd = devkit();
    cmd.args(["rules", "context"])
        .args(args)
        .current_dir(project)
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG");
    testenv::scrub_identity(&mut cmd);
    cmd.output().unwrap()
}

#[test]
fn context_emits_repo_scope_must_rules_and_the_query_pointer() {
    let (proj, state) = context_project();
    let out = run_context(proj.path(), state.path(), &[]);
    assert!(out.status.success());
    let body = String::from_utf8(out.stdout).unwrap();
    assert!(
        body.contains("Root must"),
        "the repo-scope must rule: {body}"
    );
    assert!(
        !body.contains("Foo should"),
        "not the directory rule: {body}"
    );
    assert!(
        !body.contains("Review only"),
        "scope repo, severity must only: {body}"
    );
    assert!(
        body.contains("devkit rules query --path"),
        "the pointer: {body}"
    );
}

#[test]
fn context_outside_a_devkit_project_prints_nothing_and_exits_zero() {
    let state = tempfile::tempdir().unwrap();
    let empty = tempfile::tempdir().unwrap();
    let out = run_context(empty.path(), state.path(), &[]);
    assert!(
        out.status.success(),
        "a session hook calls this unconditionally"
    );
    assert!(
        out.stdout.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn additional_context_wraps_the_block_in_the_json_envelope() {
    let (proj, state) = context_project();
    let out = run_context(proj.path(), state.path(), &["--additional-context"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let text = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext");
    assert!(text.contains("Root must"), "{text}");
}
