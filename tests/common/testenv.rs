//! One place to strip the identity a developer's own session would otherwise
//! leak into a test's child process. Without this a run started from inside a
//! coding agent resolves a different default holder than CI does.

use std::process::Command;

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
