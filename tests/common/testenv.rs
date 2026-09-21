//! One place to keep a developer's own session out of a test's child process:
//! its identity and its home directories. Without this a run started from
//! inside a coding agent resolves a different default holder than CI does, and
//! a run anywhere reads and writes the developer's real devkit state.
//!
//! Compile-time unused helpers are expected: different test binaries include
//! this module via `#[path]` and use different subsets of it.
#![allow(dead_code)]

use std::{ffi::OsStr, process::Command};

/// Variables that would let an ambient session own a test's locks.
const IDENTITY_VARS: [&str; 4] = [
    "DEVKIT_SESSION",
    "TMUX_PANE",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_SESSION_ID",
];

pub fn scrub_identity(cmd: &mut Command) -> &mut Command {
    for var in IDENTITY_VARS {
        cmd.env_remove(var);
    }
    cmd
}

/// A `Command` for `exe` whose `HOME` and `XDG_STATE_HOME` point at a fresh
/// temp dir. Bind the guard for as long as the command runs.
///
/// `DEVKIT_SKIP_AUTOLINK` alone is not isolation: every invocation runs
/// `migrate_legacy_state`, which reads and writes under both homes.
pub fn isolated(exe: impl AsRef<OsStr>) -> (tempfile::TempDir, Command) {
    let home = tempfile::tempdir().expect("temp home");
    let mut cmd = Command::new(exe);
    cmd.env("HOME", home.path())
        .env("XDG_STATE_HOME", home.path())
        .env("DEVKIT_SKIP_AUTOLINK", "1");
    (home, cmd)
}
