//! `devkit hook <event>`: every coding-agent hook event enters here.
//!
//! The cut is by caller rather than by subsystem. One harness event needs
//! several subsystems to answer it — `pre-tool-use` guards a command, claims
//! write targets and records the attempt — so the dispatch reads the payload's
//! own `tool_name` to pick the shell path or the edit path, which is where that
//! decision belongs.
//!
//! Exit codes are a control channel. Exit 2 blocks the tool call on Claude Code
//! `PreToolUse` and on Codex, so a usage error, an unknown verb and a panic all
//! exit 1 instead; `main` owns that wrapper, because the parse it covers
//! happens before this module is reached.
//!
//! Only `pre-tool-use` writes to stdout. Every other verb is silent, because
//! `UserPromptSubmit` appends a hook's stdout to the prompt and `Stop` and
//! `PermissionRequest` honour a JSON decision, so a stray `println!` would
//! change what the agent does.

use anyhow::Result;
use clap::{Args, ValueEnum};
use devkit_common::harness::Harness;

#[derive(Args)]
pub struct HookCli {
    /// Which event the harness is reporting.
    pub event: HookEvent,
    /// Which harness sent it. Beats inferring from the payload's shape.
    #[arg(long)]
    pub harness: Option<HarnessArg>,
}

/// The harness a manifest names. Separate from `Harness` because that type
/// lives in `devkit-common`, the one crate worth keeping free of clap.
#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum HarnessArg {
    ClaudeCode,
    Codex,
    Cursor,
}

impl From<HarnessArg> for Harness {
    fn from(a: HarnessArg) -> Self {
        match a {
            HarnessArg::ClaudeCode => Harness::ClaudeCode,
            HarnessArg::Codex => Harness::Codex,
            HarnessArg::Cursor => Harness::Cursor,
        }
    }
}

/// devkit's own hook vocabulary, deliberately not a model of any payload's
/// `hook_event_name`. The two differ: Codex maps both `Stop` and `Interrupt`
/// onto `stop`, and Cursor spells the same events in camelCase. One verb per
/// vendor event is what keeps each manifest a translation table with no logic
/// in it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum HookEvent {
    /// The one-word spelling is permanent: it is how every installed
    /// `lockm hook pretooluse` reads, and those manifests outlive the binary.
    #[value(alias = "pretooluse")]
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    SessionStart,
    SessionEnd,
    SubagentStart,
    SubagentStop,
    PermissionRequest,
    PermissionDenied,
    Stop,
    StopFailure,
    PreCompact,
    PostCompact,
    CwdChanged,
    WorktreeCreate,
    WorktreeRemove,
    UserPromptSubmit,
}

pub fn run(cli: HookCli) -> Result<()> {
    let _harness = cli.harness.map(Harness::from);
    match cli.event {
        HookEvent::PreToolUse => {
            crate::harness::guard_shell();
            Ok(())
        }
        HookEvent::SubagentStop => {
            crate::locks::run_hook("subagent-stop");
            Ok(())
        }
        HookEvent::SessionEnd => {
            crate::locks::run_hook("session-end");
            Ok(())
        }
        // Record-only verbs. They take no action, and their records arrive
        // with the logging subsystem.
        _ => Ok(()),
    }
}
