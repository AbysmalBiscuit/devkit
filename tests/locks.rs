//! End-to-end coverage of the `lockm` binary: conflict detection, JSON output,
//! release, and exit codes. Each test is isolated via a private temp project
//! (a real git repository) and a private `XDG_STATE_HOME`.

#[path = "common/shimtest.rs"]
mod shimtest;
#[path = "common/testenv.rs"]
mod testenv;
use std::path::Path;
use std::process::{Command, Output};

fn project() -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    devkit_common::git::Git::fixture(p.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    p
}

fn run(exe: &Path, project: &Path, state: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .current_dir(project)
        .env("XDG_STATE_HOME", state)
        // Override HOME too: the binary runs migrate_legacy_state() at startup, which
        // reads $HOME/.claude/state/devkit. Pointing HOME at the throwaway temp dir
        // keeps the test from ever touching the developer's real state home.
        .env("HOME", state)
        .env("DEVKIT_SKIP_AUTOLINK", "1");
    testenv::scrub_identity(&mut cmd);
    cmd.output().expect("spawn lockm")
}

#[test]
fn second_holder_conflicts_with_overlap() {
    let (_dir, link) = shimtest::linked("lockm");
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    let a = run(
        &link,
        proj.path(),
        state.path(),
        &["acquire", "scenes", "--as", "alice"],
    );
    assert!(a.status.success(), "alice should acquire");

    let b = run(
        &link,
        proj.path(),
        state.path(),
        &["acquire", "scenes/player.tscn", "--as", "bob"],
    );
    assert_eq!(
        b.status.code(),
        Some(1),
        "bob conflicts on an overlapping path"
    );
    let text = String::from_utf8_lossy(&b.stderr);
    assert!(text.contains("alice"), "conflict names the holder: {text}");
}

#[test]
fn json_conflict_shape() {
    let (_dir, link) = shimtest::linked("lockm");
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    run(
        &link,
        proj.path(),
        state.path(),
        &["acquire", "scenes", "--as", "alice"],
    );

    let b = run(
        &link,
        proj.path(),
        state.path(),
        &["check", "scenes/x", "--as", "bob", "--json"],
    );
    assert_eq!(b.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&b.stdout).expect("json on stdout");
    assert_eq!(v["ok"], serde_json::json!(false));
    assert_eq!(v["conflicts"][0]["held_by"], serde_json::json!("alice"));
}

#[test]
fn release_frees_for_other_holder() {
    let (_dir, link) = shimtest::linked("lockm");
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    run(
        &link,
        proj.path(),
        state.path(),
        &["acquire", "scenes", "--as", "alice"],
    );
    let r = run(
        &link,
        proj.path(),
        state.path(),
        &["release", "scenes", "--as", "alice"],
    );
    assert!(r.status.success());

    let b = run(
        &link,
        proj.path(),
        state.path(),
        &["acquire", "scenes", "--as", "bob"],
    );
    assert!(b.status.success(), "bob can acquire after alice releases");
}

#[test]
fn same_holder_reacquire_is_ok() {
    let (_dir, link) = shimtest::linked("lockm");
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    assert!(
        run(
            &link,
            proj.path(),
            state.path(),
            &["acquire", "scenes", "--as", "alice"]
        )
        .status
        .success()
    );
    assert!(
        run(
            &link,
            proj.path(),
            state.path(),
            &["acquire", "scenes", "--as", "alice"]
        )
        .status
        .success()
    );
}

/// Feeds a `pretooluse` hook payload to `lockm hook` over stdin, isolated the
/// same way `run` is: a private `XDG_STATE_HOME`/`HOME` so the harness's
/// global-config read (`$HOME/.config/devkit/config.toml`) finds nothing, and
/// `DEVKIT_ENFORCE_WRITES` stripped so an inherited override cannot decide
/// the result instead of the layer stack.
fn run_hook(exe: &Path, cwd: &Path, state: &Path, holder: &str, target: &Path) -> Output {
    run_hook_as(exe, cwd, state, holder, None, target)
}

/// `run_hook` with an optional `agent_id`, which the harness sends for a
/// sub-agent's write and which the hook folds into a `session/agent` holder.
fn run_hook_as(
    exe: &Path,
    cwd: &Path,
    state: &Path,
    session: &str,
    agent: Option<&str>,
    target: &Path,
) -> Output {
    use std::io::Write;
    let mut payload = serde_json::json!({
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Write",
        "tool_input": { "file_path": target.to_string_lossy() },
    });
    if let Some(a) = agent {
        payload["agent_id"] = serde_json::json!(a);
    }
    let mut cmd = Command::new(exe);
    cmd.args(["hook", "pretooluse"])
        .env("XDG_STATE_HOME", state)
        .env("HOME", state)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_ENFORCE_WRITES")
        .env_remove("DEVKIT_CONFIG");
    testenv::scrub_identity(&mut cmd);
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn lockm hook");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    child.wait_with_output().expect("lockm hook output")
}

/// Whether a hook invocation's stdout carries a deny decision. Panics with
/// the exit status and stderr if the process did not exit successfully: a
/// crashed hook must be diagnosed, not misread as "allowed".
fn is_deny(label: &str, out: &Output) -> bool {
    assert!(
        out.status.success(),
        "{label}: lockm hook exited with {:?}; stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stdout = stdout.trim();
    if stdout.is_empty() {
        return false;
    }
    let v: serde_json::Value = serde_json::from_str(stdout).expect("hook stdout is JSON");
    v["hookSpecificOutput"]["permissionDecision"] == "deny"
}

/// A harness declaration below the checkout root must be seen by the deployed
/// hook binary, not only by the library function it calls: the enforcement
/// gate reads `cwd`, not a pre-resolved checkout root, so a directory between
/// the root and the write is part of the answer.
#[test]
fn hook_honors_a_harness_declaration_in_a_nested_directory() {
    let (_dir, link) = shimtest::linked("lockm");
    let proj = project();
    std::fs::write(proj.path().join("devkit.toml"), "").unwrap();
    let nested = proj.path().join("packages/thing");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(
        nested.join("devkit.local.toml"),
        "[harness]\nenforce_writes = true\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();

    // Enforcement is on at the nested directory: the first write claims the
    // file, the second is denied.
    let target = nested.join("a.rs");
    let first = run_hook(&link, &nested, state.path(), "alice", &target);
    assert!(
        !is_deny("first write (nested)", &first),
        "first write should be allowed: {first:?}"
    );
    let second = run_hook(&link, &nested, state.path(), "bob", &target);
    assert!(
        is_deny("second write (nested)", &second),
        "second write should be denied: {second:?}"
    );

    // Enforcement is off at the checkout root: neither write is denied.
    let other = proj.path().join("b.rs");
    let third = run_hook(&link, proj.path(), state.path(), "carol", &other);
    assert!(
        !is_deny("first write (root)", &third),
        "root has no opt-in: {third:?}"
    );
    let fourth = run_hook(&link, proj.path(), state.path(), "dave", &other);
    assert!(
        !is_deny("second write (root)", &fourth),
        "root has no opt-in: {fourth:?}"
    );
}

#[test]
fn session_end_releases_even_when_enforcement_is_off() {
    let (_dir, link) = shimtest::linked("lockm");
    let proj = project();
    let state = tempfile::tempdir().unwrap();

    let a = run(
        &link,
        proj.path(),
        state.path(),
        &["acquire", "src/a.rs", "--as", "S"],
    );
    assert!(a.status.success(), "S should acquire");

    let payload = r#"{"session_id":"S","hook_event_name":"SessionEnd"}"#;
    let out = Command::new(&link)
        .args(["hook", "session-end"])
        .current_dir(proj.path())
        .env("XDG_STATE_HOME", state.path())
        .env("HOME", state.path())
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("DEVKIT_ENFORCE_WRITES", "0")
        .env_remove("DEVKIT_SESSION")
        .env_remove("TMUX_PANE")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin.as_mut().unwrap().write_all(payload.as_bytes())?;
            c.wait_with_output()
        })
        .expect("run session-end hook");
    assert!(out.status.success(), "hook exits 0");

    let s = run(&link, proj.path(), state.path(), &["status", "--json"]);
    let text = String::from_utf8_lossy(&s.stdout);
    assert!(
        !text.contains("\"S\""),
        "S's locks are released even with enforcement off: {text}"
    );
}

/// Runs `lockm` as a harness session would: the vendor variable set, and every
/// other identity source stripped so the resolved holder can only have come
/// from detection.
fn run_as_session(
    exe: &Path,
    project: &Path,
    state: &Path,
    session: &str,
    args: &[&str],
) -> Output {
    Command::new(exe)
        .args(args)
        .current_dir(project)
        .env("XDG_STATE_HOME", state)
        .env("HOME", state)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("CLAUDE_CODE_SESSION_ID", session)
        .env_remove("CODEX_SESSION_ID")
        .env_remove("DEVKIT_SESSION")
        .env_remove("TMUX_PANE")
        .output()
        .expect("spawn lockm")
}

/// The enforced project both directions of this test need.
fn enforced_project() -> (tempfile::TempDir, std::path::PathBuf) {
    let proj = project();
    std::fs::write(
        proj.path().join("devkit.toml"),
        "[harness]\nenforce_writes = true\n",
    )
    .unwrap();
    let target = proj.path().join("src/a.rs");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    (proj, target)
}

#[test]
fn a_hook_held_lock_is_not_a_conflict_for_its_own_session() {
    let (_dir, link) = shimtest::linked("lockm");
    let state = tempfile::tempdir().unwrap();
    let (proj, target) = enforced_project();

    let h = run_hook(&link, proj.path(), state.path(), "sess-e2e", &target);
    assert!(
        !is_deny("hook claim", &h),
        "the hook's own first write is allowed"
    );

    let a = run_as_session(
        &link,
        proj.path(),
        state.path(),
        "sess-e2e",
        &["acquire", "src/a.rs"],
    );
    assert!(
        a.status.success(),
        "a session must not conflict with the lock its own write hook took; stdout: {} stderr: {}",
        String::from_utf8_lossy(&a.stdout),
        String::from_utf8_lossy(&a.stderr)
    );
}

#[test]
fn a_cli_held_lock_does_not_deny_its_own_sessions_write() {
    let (_dir, link) = shimtest::linked("lockm");
    let state = tempfile::tempdir().unwrap();
    let (proj, target) = enforced_project();

    let a = run_as_session(
        &link,
        proj.path(),
        state.path(),
        "sess-e2e",
        &["acquire", "src/a.rs"],
    );
    assert!(
        a.status.success(),
        "claim succeeds: {}",
        String::from_utf8_lossy(&a.stderr)
    );

    let h = run_hook(&link, proj.path(), state.path(), "sess-e2e", &target);
    assert!(
        !is_deny("own write", &h),
        "a session must not be denied a write to the path it claimed by hand"
    );
}

/// A sub-agent's hook row (`S/a1`) and a hand claim by the session that spawned
/// it (`S`) are one session line: the claim must be permitted, and it reports
/// the lease already in force rather than the one it asked for.
#[test]
fn a_subagent_hook_row_does_not_block_its_sessions_claim() {
    let (_dir, link) = shimtest::linked("lockm");
    let state = tempfile::tempdir().unwrap();
    let (proj, target) = enforced_project();

    let h = run_hook_as(
        &link,
        proj.path(),
        state.path(),
        "sess-fanout",
        Some("a1"),
        &target,
    );
    assert!(
        !is_deny("subagent write", &h),
        "the sub-agent's own first write is allowed"
    );

    let a = run_as_session(
        &link,
        proj.path(),
        state.path(),
        "sess-fanout",
        &["acquire", "src/a.rs", "--ttl", "60"],
    );
    assert!(
        a.status.success(),
        "a session must not conflict with the row its own sub-agent's write hook took; stderr: {}",
        String::from_utf8_lossy(&a.stderr)
    );
    let stdout = String::from_utf8_lossy(&a.stdout);
    assert!(
        stdout.contains("already held on this session line") && stdout.contains("ttl 1800s"),
        "the row's own lease is reported, not the requested 60s: {stdout}"
    );
}

/// The same-line row a session may acquire over, it may not release: the row is
/// its sub-agent's and the lifecycle hook frees it. The refusal must say that
/// rather than blame another session and offer `--force`.
#[test]
fn releasing_a_subagent_row_by_hand_is_refused_as_a_same_line_row() {
    let (_dir, link) = shimtest::linked("lockm");
    let state = tempfile::tempdir().unwrap();
    let (proj, target) = enforced_project();

    let h = run_hook_as(
        &link,
        proj.path(),
        state.path(),
        "sess-release",
        Some("a1"),
        &target,
    );
    assert!(!is_deny("subagent write", &h));

    let r = run_as_session(
        &link,
        proj.path(),
        state.path(),
        "sess-release",
        &["release", "src/a.rs"],
    );
    assert_eq!(r.status.code(), Some(1), "the release is still refused");
    let text = String::from_utf8_lossy(&r.stderr);
    assert!(
        text.contains("this session line") && text.contains("sess-release/a1"),
        "the refusal names the session line and the holder: {text}"
    );
    assert!(
        !text.contains("--force"),
        "forcing past your own sub-agent is not the remedy: {text}"
    );
}

/// A payload the hook cannot parse is the signature of a harness format change,
/// so an enforced write is denied rather than let through. The release events
/// carry no permission decision, so they stay silent.
#[test]
fn an_unparsable_write_payload_is_denied_under_enforcement() {
    let (_dir, link) = shimtest::linked("lockm");
    let state = tempfile::tempdir().unwrap();
    let (proj, _target) = enforced_project();

    let feed = |event: &str| -> Output {
        use std::io::Write;
        let mut cmd = Command::new(&link);
        cmd.args(["hook", event])
            .current_dir(proj.path())
            .env("XDG_STATE_HOME", state.path())
            .env("HOME", state.path())
            .env("DEVKIT_SKIP_AUTOLINK", "1")
            .env_remove("DEVKIT_ENFORCE_WRITES")
            .env_remove("DEVKIT_CONFIG");
        testenv::scrub_identity(&mut cmd);
        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn lockm hook");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"{not json at all")
            .unwrap();
        child.wait_with_output().expect("lockm hook output")
    };

    let write = feed("pretooluse");
    assert!(
        is_deny("unparsable write payload", &write),
        "an unevaluable write must not be allowed: {write:?}"
    );

    let end = feed("session-end");
    assert!(end.status.success(), "the release hook still exits 0");
    assert!(
        end.stdout.is_empty(),
        "a release event has no decision to emit: {:?}",
        String::from_utf8_lossy(&end.stdout)
    );
}

#[test]
fn nested_harness_refuses_to_guess_on_acquire_and_release() {
    let (_dir, link) = shimtest::linked("lockm");
    let proj = project();
    let state = tempfile::tempdir().unwrap();

    let nested = |args: &[&str]| {
        let mut cmd = Command::new(&link);
        cmd.args(args)
            .current_dir(proj.path())
            .env("XDG_STATE_HOME", state.path())
            .env("HOME", state.path())
            .env("DEVKIT_SKIP_AUTOLINK", "1");
        // Scrub first, then put back exactly the two variables under test.
        testenv::scrub_identity(&mut cmd);
        cmd.env("CLAUDE_CODE_SESSION_ID", "outer-session")
            .env("CODEX_SESSION_ID", "inner-session");
        cmd.output().expect("spawn lockm")
    };

    let a = nested(&["acquire", "src/a.rs"]);
    assert_eq!(a.status.code(), Some(2), "acquire refuses to guess");
    let text = String::from_utf8_lossy(&a.stderr);
    assert!(
        text.contains("CLAUDE_CODE_SESSION_ID=outer-session"),
        "names the variable and its value: {text}"
    );
    assert!(
        text.contains("CODEX_SESSION_ID=inner-session"),
        "names the variable and its value: {text}"
    );

    let r = nested(&["release", "--all"]);
    assert_eq!(r.status.code(), Some(2), "release refuses to guess");

    let s = nested(&["status"]);
    assert!(s.status.success(), "status is read-only and proceeds");

    let ok = nested(&["acquire", "src/a.rs", "--as", "inner-session"]);
    assert!(ok.status.success(), "--as resolves it");
}
