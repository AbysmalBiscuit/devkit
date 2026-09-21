//! `devkit rules` against an index passed by path, so no cache lookup is
//! involved and the test is hermetic.
//!
//! Isolated the same way `tests/locks.rs` is: a private `HOME`/`XDG_STATE_HOME`
//! and `DEVKIT_SKIP_AUTOLINK`, since the binary migrates legacy state and
//! links shims at startup regardless of the subcommand.

#[path = "common/testenv.rs"]
mod testenv;

use std::{
    io::Write,
    process::{Command, Stdio},
};

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

/// A project whose `[rules] index` names the fixture directly, with no
/// `--index`/positional path on the command line. `query` and `stats` must
/// resolve it the way `devkit rules context` and the hook already do.
#[test]
fn query_and_stats_honor_the_configured_index() {
    let (proj, state) = context_project();

    let mut cmd = devkit();
    cmd.args(["rules", "query", "--format", "json"])
        .current_dir(proj.path())
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG");
    testenv::scrub_identity(&mut cmd);
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "query should find the configured index: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = String::from_utf8(out.stdout).unwrap();
    assert!(body.contains("r-foo-should"), "{body}");

    let mut cmd = devkit();
    cmd.args(["rules", "stats"])
        .current_dir(proj.path())
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG");
    testenv::scrub_identity(&mut cmd);
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "stats should find the configured index: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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

/// Like `context_project`, but `max_event_bytes` is small enough that the
/// two matching fixture rules (`r-root-must`, `r-review-only`) cannot both
/// render into one event.
fn context_project_with_cap(max_event_bytes: usize) -> (tempfile::TempDir, tempfile::TempDir) {
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
        format!(
            "[rules]\nenabled = true\nmax_event_bytes = {max_event_bytes}\nindex = '{}'\n",
            index.display()
        ),
    )
    .unwrap();
    (p, state)
}

/// A piped call to `devkit rules context`, carrying `session_id`.
fn run_context_piped(
    project: &std::path::Path,
    state: &std::path::Path,
    session_id: &str,
) -> String {
    let mut cmd = devkit();
    cmd.args(["rules", "context"])
        .current_dir(project)
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    testenv::scrub_identity(&mut cmd);
    let mut child = cmd.spawn().expect("spawn devkit rules context");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            serde_json::json!({"session_id": session_id})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    let out = child
        .wait_with_output()
        .expect("devkit rules context output");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// A rule the byte cap cuts from the rendered block must not be stamped as
/// fired: the fired set must never claim to have shown content that never
/// fully rendered. Unlike the hook, `devkit rules context` does not consult
/// the fired set to decide what to show next time (every session-start
/// call broadcasts the same top rules), so the emitted-vs-stamped mismatch
/// has to be checked directly against the fired-set file.
#[test]
fn context_does_not_stamp_a_rule_the_byte_cap_cut() {
    let (proj, state) = context_project_with_cap(160);

    let body = run_context_piped(proj.path(), state.path(), "cap-session");
    assert!(body.contains("Root must"), "the rule that fits: {body}");
    assert!(
        !body.contains("Review only"),
        "the rule the cap cut is dropped whole, not truncated: {body}"
    );

    let fired_dir = state.path().join("devkit/rules");
    let file = std::fs::read_dir(&fired_dir)
        .unwrap()
        .next()
        .expect("a fired-set file for the session")
        .unwrap()
        .path();
    let stamped = std::fs::read_to_string(&file).unwrap();
    assert!(
        stamped.contains("r-root-must"),
        "the rule actually shown is stamped: {stamped}"
    );
    assert!(
        !stamped.contains("r-review-only"),
        "the rule the cap cut must not be marked fired, since it was never \
         actually shown: {stamped}"
    );
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
        body.contains("Review only"),
        "session start is the broad trigger: a code-review-only rule still \
         governs the repository and must appear here, unlike on the write \
         path: {body}"
    );
    assert!(
        body.contains("devkit rules query --path"),
        "the pointer: {body}"
    );
}

/// The stamp is what keeps the first allowed write from re-injecting
/// everything this block just showed. `session_id()` only reads a payload
/// off stdin, so the call has to be piped one to exercise it at all.
#[test]
fn context_stamps_the_ids_it_emitted_for_the_piped_session() {
    let (proj, state) = context_project();
    let body = run_context_piped(proj.path(), state.path(), "ctx-session");
    assert!(body.contains("Root must"), "{body}");

    let fired_dir = state.path().join("devkit/rules");
    let mut entries = std::fs::read_dir(&fired_dir)
        .unwrap_or_else(|e| panic!("no fired-set directory at {fired_dir:?}: {e}"));
    let file = entries
        .next()
        .expect("a fired-set file for the piped session")
        .unwrap()
        .path();
    assert!(entries.next().is_none(), "exactly one holder fired here");
    let stamped = std::fs::read_to_string(&file).unwrap();
    assert!(
        stamped.contains("r-root-must"),
        "the emitted rule's id is stamped: {stamped}"
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
