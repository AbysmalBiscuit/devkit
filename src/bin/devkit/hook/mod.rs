//! `devkit hook <event>`: every coding-agent hook event enters here.
//!
//! The cut is by caller rather than by subsystem. One harness event needs
//! several subsystems to answer it — `pre-tool-use` guards a command, claims
//! write targets and records the attempt — so the dispatch reads the payload's
//! own tool to pick the shell path or the edit path, which is where that
//! decision belongs.
//!
//! pabal parses the Claude Code and Codex payloads and writes their responses.
//! `payload` layers the Cursor spellings it does not model yet on top.
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
mod mcp;
mod payload;
pub mod record;
pub(crate) mod rules;
mod shell;
mod writes;

use std::io::Read;

use anyhow::Result;
use clap::{Args, ValueEnum};
use payload::{Harness, Payload};
use serde_json::Value;

#[derive(Args)]
pub struct HookCli {
    /// Which event the harness is reporting.
    pub event: HookEvent,
    /// Which harness sent it. Beats inferring from the payload's shape.
    #[arg(long)]
    pub harness: Option<Harness>,
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
    let harness = cli.harness;
    match cli.event {
        HookEvent::PreToolUse => pre_tool_use(harness),
        // The two verbs that release, which is the half with a correctness
        // consequence, so it runs before the record.
        HookEvent::SubagentStop => with_payload(harness, |p| {
            edit::release_subagent(p);
            record_only(p, cli.event);
            Ok(())
        }),
        // Release, then record, then sweep. Release first because it is the
        // one step with a correctness consequence; the sweep last because it is
        // the only one that can be skipped without loss.
        HookEvent::SessionEnd => with_payload(harness, |p| {
            edit::release_session(p);
            clear_issue_receipts(p);
            let settings = record_only(p, cli.event);
            if settings.auto_prune && settings.enabled {
                // A retention cap nothing enforces is not a promise. Fail-open:
                // the outcome is discarded, so a sweep failure never changes
                // this hook's exit.
                let _ = devkit_common::harness_log::prune::sweep(&settings);
            }
            Ok(())
        }),
        // Compaction is what drops the injected rules out of the agent's
        // context, so clearing the set is what lets them inject again.
        HookEvent::PostCompact => with_payload(harness, |p| {
            if let Some(session) = p.session_id() {
                rules::clear_for_holder(session);
            }
            record_only(p, cli.event);
            Ok(())
        }),
        // Record-only. Each reads stdin, builds one record and exits; nothing
        // reaches stdout, because `UserPromptSubmit` appends a hook's stdout to
        // the prompt and `Stop` and `PermissionRequest` honour a JSON decision.
        event => with_payload(harness, |p| {
            record_only(p, event);
            Ok(())
        }),
    }
}

/// Delete the ending session's `issue render` receipts in the payload's
/// checkout. Best-effort: a receipt left behind is swept by a later render.
fn clear_issue_receipts(payload: &Payload) {
    let cwd = record::payload_cwd(payload);
    if let (Some(session), Some(root)) = (
        payload.session_id(),
        devkit_common::git::Checkout::at(&cwd).root(),
    ) {
        let _ = crate::issue::receipt::clear_session(root, session);
    }
}

/// Write the record a verb beyond `pre-tool-use` carries, if any, and hand back
/// the settings so a caller that has more to do does not resolve them twice.
///
/// With logging off this is one global config read: no project layer, no git,
/// no tree-sitter.
fn record_only(payload: &Payload, event: HookEvent) -> devkit_common::harness_log::Settings {
    let cwd = record::payload_cwd(payload);
    let checkout = devkit_common::vcs::Checkout::at(&cwd);
    let settings = devkit_common::harness_log::resolve_in(&checkout, &cwd);
    if !settings.enabled {
        return settings;
    }
    let kind = record::record_only(payload, event, &settings);
    let rec = record::envelope(payload, event, &checkout, kind);
    devkit_common::harness_log::record(&settings, &rec);
    settings
}

/// Read the hook payload from stdin. `None` covers an unreadable pipe, text
/// that is not JSON, and JSON that is not an object: none is a payload to
/// judge, and the caller's fail-closed rule is the same for all three.
fn read_payload(harness: Option<Harness>) -> Option<Payload> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).ok()?;
    Payload::new(harness, serde_json::from_str::<Value>(&buf).ok()?)
}

/// Run a verb over the payload, or do nothing when there is none to read. A
/// verb reached here has no verdict to fail toward, so an unreadable payload
/// is silence rather than a denial.
///
/// An absent payload is read as an empty object rather than skipped, so a verb
/// a harness fires with no body still records that it fired.
fn with_payload(harness: Option<Harness>, f: impl FnOnce(&Payload) -> Result<()>) -> Result<()> {
    f(&read_payload(harness).unwrap_or_else(|| Payload::empty(harness)))
}

/// The retired `lockm hook <event>` spelling. Kept because an installed plugin
/// manifest can outlive the binary it was installed beside.
pub(crate) fn legacy_lock_event(event: &str) -> Result<()> {
    match event {
        "pretooluse" => pre_tool_use(None),
        "subagent-stop" => with_payload(None, |p| {
            edit::release_subagent(p);
            Ok(())
        }),
        "session-end" => with_payload(None, |p| {
            edit::release_session(p);
            Ok(())
        }),
        other => {
            anyhow::bail!("unknown lock hook event `{other}`");
        }
    }
}

/// The payload's own tool picks the path: an edit goes to the edit guard, an
/// MCP call to the issue guard, and everything else to the shell guard. The
/// decision belongs here, where the payload is, rather than in one matcher
/// block per subsystem in each manifest.
///
/// Dispatch happens before any config load or tree-sitter work, so an edit
/// or MCP payload pays nothing for the shell path.
pub(crate) fn pre_tool_use(harness: Option<Harness>) -> Result<()> {
    let Some(payload) = read_payload(harness) else {
        return shell::deny_unreadable_payload(harness);
    };
    if let Some(write) = edit::write(&payload) {
        return edit::guard(&payload, write);
    }
    match payload.tool() {
        Some(pabal::Tool::Mcp { .. }) => mcp::guard(&payload),
        _ => shell::guard(&payload),
    }
}
