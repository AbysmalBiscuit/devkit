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

mod dialect;
mod edit;
pub mod record;
mod shell;
mod writes;

use std::io::Read;

use anyhow::Result;
use clap::{Args, ValueEnum};
use devkit_common::harness::Harness;
use serde_json::Value;

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
    let harness = cli.harness.map(Harness::from);
    match cli.event {
        HookEvent::PreToolUse => pre_tool_use(harness),
        // The two verbs that release, which is the half with a correctness
        // consequence, so it runs before the record.
        HookEvent::SubagentStop => with_payload(|p| {
            edit::release_subagent(p)?;
            record_only(p, cli.event, harness);
            Ok(())
        }),
        // Release, then record, then sweep. Release first because it is the
        // one step with a correctness consequence; the sweep last because it is
        // the only one that can be skipped without loss.
        HookEvent::SessionEnd => with_payload(|p| {
            edit::release_session(p)?;
            let settings = record_only(p, cli.event, harness);
            if settings.auto_prune && settings.enabled {
                // A retention cap nothing enforces is not a promise. Fail-open:
                // the outcome is discarded, so a sweep failure never changes
                // this hook's exit.
                let _ = devkit_common::harness_log::prune::sweep(&settings);
            }
            Ok(())
        }),
        // Record-only. Each reads stdin, builds one record and exits; nothing
        // reaches stdout, because `UserPromptSubmit` appends a hook's stdout to
        // the prompt and `Stop` and `PermissionRequest` honour a JSON decision.
        event => with_payload(|p| {
            record_only(p, event, harness);
            Ok(())
        }),
    }
}

/// Write the record a verb beyond `pre-tool-use` carries, if any, and hand back
/// the settings so a caller that has more to do does not resolve them twice.
///
/// With logging off this is one global config read: no project layer, no git,
/// no tree-sitter.
fn record_only(
    payload: &Value,
    event: HookEvent,
    harness: Option<Harness>,
) -> devkit_common::harness_log::Settings {
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let settings = devkit_common::harness_log::resolve(&cwd);
    if !settings.enabled {
        return settings;
    }
    let kind = record::record_only(payload, event, &settings);
    let rec = record::envelope(payload, event, harness, kind);
    devkit_common::harness_log::record(&settings, &rec);
    settings
}

/// Read the hook payload from stdin. `None` covers both an unreadable pipe and
/// text that is not JSON — neither is a payload to judge, and the caller's
/// fail-closed rule is the same for both.
fn read_payload() -> Option<Value> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).ok()?;
    serde_json::from_str(&buf).ok()
}

/// Run a verb over the payload, or do nothing when there is none to read. A
/// verb reached here has no verdict to fail toward, so an unreadable payload
/// is silence rather than a denial.
///
/// An absent payload is read as an empty object rather than skipped, so a verb
/// a harness fires with no body still records that it fired.
fn with_payload(f: impl FnOnce(&Value) -> Result<()>) -> Result<()> {
    f(&read_payload().unwrap_or_else(|| Value::Object(serde_json::Map::new())))
}

/// The retired `lockm hook <event>` spelling. Kept because an installed plugin
/// manifest can outlive the binary it was installed beside.
pub(crate) fn legacy_lock_event(event: &str) -> Result<()> {
    match event {
        "pretooluse" => pre_tool_use(None),
        "subagent-stop" => with_payload(edit::release_subagent),
        "session-end" => with_payload(edit::release_session),
        other => {
            anyhow::bail!("unknown lock hook event `{other}`");
        }
    }
}

/// The payload's own `tool_name` picks the path. The two `PreToolUse` matcher
/// blocks each manifest used to carry existed only because two subsystems
/// answered one event; the decision belongs here, where the payload is.
///
/// Dispatch happens before any config load or tree-sitter work, so an edit
/// payload pays nothing for the shell path.
pub(crate) fn pre_tool_use(harness: Option<Harness>) -> Result<()> {
    let Some(payload) = read_payload() else {
        return shell::deny_unreadable_payload(harness);
    };
    match payload.get("tool_name").and_then(Value::as_str) {
        Some(t) if devkit_locks::hook::is_write_tool(t) => edit::guard(&payload),
        _ => shell::guard(&payload, harness),
    }
}
