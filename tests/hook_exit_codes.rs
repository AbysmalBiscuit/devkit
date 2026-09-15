//! Exit 2 blocks the tool call on Claude Code `PreToolUse` and sets
//! `should_block` on Codex, so no verb in the `hook` family may reach it,
//! whatever the argument error. An unrecognised verb is still an error — the
//! message reaches stderr and the exit is non-zero — it just no longer reads
//! as a deny.

use std::process::{Command, Output, Stdio};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_devkit"))
        .args(args)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn devkit")
}

#[test]
fn an_unknown_verb_exits_one_with_a_message() {
    let out = run(&["hook", "pre-tool-yoose"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "exit 2 would block the tool call"
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).is_empty(),
        "an unrecognised verb is an error, not silence"
    );
}

#[test]
fn an_unknown_harness_exits_one() {
    let out = run(&["hook", "pre-tool-use", "--harness", "emacs"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!String::from_utf8_lossy(&out.stderr).is_empty());
}

#[test]
fn a_missing_verb_exits_one() {
    assert_eq!(run(&["hook"]).status.code(), Some(1));
}

#[test]
fn the_pretooluse_alias_still_resolves() {
    let out = run(&["hook", "pretooluse"]);
    assert_ne!(
        out.status.code(),
        Some(1),
        "the installed manifests spell it this way"
    );
}

#[test]
fn every_verb_the_family_carries_parses() {
    for verb in [
        "pre-tool-use",
        "post-tool-use",
        "post-tool-use-failure",
        "session-start",
        "session-end",
        "subagent-start",
        "subagent-stop",
        "permission-request",
        "permission-denied",
        "stop",
        "stop-failure",
        "pre-compact",
        "post-compact",
        "cwd-changed",
        "worktree-create",
        "worktree-remove",
        "user-prompt-submit",
    ] {
        let out = run(&["hook", verb, "--help"]);
        assert!(out.status.success(), "`devkit hook {verb}` does not parse");
    }
}

/// A usage error elsewhere keeps clap's own exit code. The rule is scoped to
/// the family whose exit code a harness reads, not applied to the whole binary.
#[test]
fn a_usage_error_outside_the_family_is_untouched() {
    assert_eq!(run(&["ports", "nonsense"]).status.code(), Some(2));
}
