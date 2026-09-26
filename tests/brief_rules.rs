//! The brief's rules section: present exactly when the pre-tool-use hook has
//! an index to inject from, and true to what that hook does for the host
//! reading it.

#[path = "common/testenv.rs"]
mod testenv;

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

/// A git checkout whose `devkit.toml` is `[rules]` plus `extra`, with the
/// fixture index beside it when `with_index`.
fn project(extra: &str, with_index: bool) -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    devkit_common::git::Git::fixture(p.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    let index = p.path().join("index.json");
    if with_index {
        std::fs::copy("crates/devkit-rules/tests/fixtures/index.json", &index).unwrap();
    }
    std::fs::write(
        p.path().join("devkit.toml"),
        // A literal string: a Windows path's backslashes are not escapes.
        format!("[rules]\nindex = '{}'\n{extra}", index.display()),
    )
    .unwrap();
    p
}

fn brief(project: &Path, state: &Path, args: &[&str], env: &[(&str, &str)], stdin: &str) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.arg("brief")
        .args(args)
        .current_dir(project)
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("XDG_CONFIG_HOME", state.join("config"))
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG")
        .env_remove("CURSOR_PROJECT_DIR")
        .env_remove("CURSOR_PLUGIN_ROOT")
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    testenv::scrub_identity(&mut cmd);
    let mut child = cmd.spawn().expect("spawn devkit brief");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("devkit brief output");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// The output with its line breaks folded to spaces: the brief wraps prose to
/// the terminal width, so a phrase can straddle a break.
fn text(out: &Output) -> String {
    unwrapped(&String::from_utf8(out.stdout.clone()).unwrap())
}

fn unwrapped(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn an_index_gets_a_rules_section_naming_what_the_hook_injects() {
    let proj = project("", true);
    let state = tempfile::tempdir().unwrap();
    let body = text(&brief(proj.path(), state.path(), &[], &[], ""));
    assert!(body.contains("### Rules"), "{body}");
    assert!(
        body.contains("`must` and `should` rules"),
        "the default floor is `should`: {body}"
    );
    assert!(
        body.contains("shell writes"),
        "the hook injects on edit tools only: {body}"
    );
    assert!(body.contains("devkit rules query --path <file>"), "{body}");
}

#[test]
fn the_rules_section_follows_the_configured_floor() {
    let proj = project("min_severity = \"must\"\n", true);
    let state = tempfile::tempdir().unwrap();
    let body = text(&brief(proj.path(), state.path(), &[], &[], ""));
    assert!(body.contains("that file's `must` rules"), "{body}");
}

#[test]
fn cursor_is_told_that_nothing_injects_rules_for_it() {
    let proj = project("", true);
    let state = tempfile::tempdir().unwrap();
    let out = brief(
        proj.path(),
        state.path(),
        &["--additional-context"],
        &[("CURSOR_PROJECT_DIR", "/x")],
        "",
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let body = unwrapped(
        v["additional_context"]
            .as_str()
            .expect("additional_context"),
    );
    assert!(body.contains("### Rules"), "{body}");
    assert!(!body.contains("shell writes"), "{body}");
    assert!(body.contains("no rules added automatically"), "{body}");
}

#[test]
fn no_rules_section_without_an_enabled_index_or_with_the_switch_off() {
    let state = tempfile::tempdir().unwrap();
    for (extra, with_index) in [
        ("", false),
        ("enabled = false\n", true),
        ("[brief]\nrules = false\n", true),
    ] {
        let proj = project(extra, with_index);
        let body = text(&brief(proj.path(), state.path(), &[], &[], ""));
        assert!(
            !body.contains("### Rules"),
            "{extra:?} with_index={with_index}: {body}"
        );
    }
}

#[test]
fn if_changed_re_emits_when_an_index_appears() {
    let proj = project("", false);
    let state = tempfile::tempdir().unwrap();
    let session = r#"{"session_id":"rules-appear"}"#;
    brief(proj.path(), state.path(), &["--if-changed"], &[], session);
    let quiet = text(&brief(
        proj.path(),
        state.path(),
        &["--if-changed"],
        &[],
        session,
    ));
    assert!(quiet.is_empty(), "nothing changed yet: {quiet}");

    std::fs::copy(
        "crates/devkit-rules/tests/fixtures/index.json",
        proj.path().join("index.json"),
    )
    .unwrap();
    let body = text(&brief(
        proj.path(),
        state.path(),
        &["--if-changed"],
        &[],
        session,
    ));
    assert!(body.contains("### Rules"), "{body}");
}
