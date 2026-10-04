//! `devkit hook <event>`: every coding-agent hook event enters here.
//!
//! The cut is by caller rather than by subsystem. One harness event needs
//! several subsystems to answer it — `pre-tool-use` guards a command, claims
//! write targets and records the attempt — so the dispatch reads the payload's
//! own tool to pick the shell path or the edit path, which is where that
//! decision belongs.
//!
//! pabal parses each harness's payload and writes its responses.
//!
//! Exit codes are a control channel. Exit 2 blocks the tool call on Claude Code
//! `PreToolUse` and on Codex, so a usage error, an unknown verb and a panic all
//! exit 1 instead; `main` owns that wrapper, because the parse it covers
//! happens before this module is reached.
//!
//! Only `pre-tool-use`, `stop` and `subagent-stop` write to stdout, and the
//! last two only to refuse a stop while the agent has open todos. Every other
//! verb is silent, because `UserPromptSubmit` appends a hook's stdout to the
//! prompt and `PermissionRequest` honours a JSON decision, so a stray
//! `println!` would change what the agent does.

mod activity;
mod dialect;
mod edit;
mod gate;
mod issue_event;
mod mcp;
pub(crate) mod payload;
pub mod record;
pub(crate) mod rules;
mod shell;
pub(crate) mod todo;
mod writes;

use std::{
    io::{Read, Write},
    path::Path,
};

use anyhow::Result;
use clap::{Args, ValueEnum};
use devkit_common::vcs::Checkout;
use pabal::AnyHarness;
use payload::Payload;
use serde_json::Value;

#[derive(Args)]
pub struct HookCli {
    /// Which event the harness is reporting.
    pub event: HookEvent,
    /// Which harness sent it. Beats inferring from the payload's shape.
    #[arg(long)]
    pub harness: Option<AnyHarness>,
}

/// devkit's own hook vocabulary, deliberately not a model of any payload's
/// `hook_event_name`. The two differ: Codex maps both `Stop` and `Interrupt`
/// onto `stop`, and Cursor spells the same events in camelCase. One verb per
/// vendor event is what keeps each manifest a translation table with no logic
/// in it.
///
/// A variant's own name is the event pabal is told for a harness that leaves
/// it out of the payload.
#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum, strum::IntoStaticStr)]
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
        // consequence, so it runs before the record. A sub-agent held to its
        // open todos is not stopping, so it keeps its claims, its locks and
        // its run.
        HookEvent::SubagentStop => with_payload_held(harness, cli.event, |p, checkout, cwd| {
            let holder = p.subagent_holder();
            let answer = holder
                .as_ref()
                .and_then(|h| todo::hold(p, h, checkout, cwd));
            let held = match answer {
                Some(answer) => {
                    print_envelope(&answer);
                    true
                }
                None => {
                    if let Some(h) = &holder {
                        todo::rearm(h);
                    }
                    todo::release(holder, checkout, cwd);
                    edit::release_subagent(p);
                    false
                }
            };
            record_in(p, cli.event, checkout, cwd);
            held
        }),
        // Release, then record, then sweep. Release first because it is the
        // one step with a correctness consequence; the sweep last because it is
        // the only one that can be skipped without loss.
        HookEvent::SessionEnd => with_payload(harness, cli.event, |p, checkout, cwd| {
            if let Some(session) = p.session_holder() {
                todo::forget_holds(&session);
            }
            todo::release(p.session_holder(), checkout, cwd);
            edit::release_session(p);
            clear_issue_receipts(p, checkout);
            let settings = record_in(p, cli.event, checkout, cwd);
            if settings.auto_prune && settings.enabled {
                // A retention cap nothing enforces is not a promise. Fail-open:
                // the outcome is discarded, so a sweep failure never changes
                // this hook's exit.
                let _ = devkit_common::harness_log::prune::sweep(&settings);
            }
            Ok(())
        }),
        HookEvent::PostToolUse => with_payload(harness, cli.event, |p, checkout, cwd| {
            todo::capture(p, checkout);
            record_in(p, cli.event, checkout, cwd);
            Ok(())
        }),
        HookEvent::SessionStart => with_payload(harness, cli.event, |p, checkout, cwd| {
            issue_event::on_session_start(checkout);
            record_in(p, cli.event, checkout, cwd);
            Ok(())
        }),
        // The verdict comes first and recording after, so a record can never
        // change it.
        HookEvent::Stop => with_payload(harness, cli.event, |p, checkout, cwd| {
            if let Some(session) = p.session_holder()
                && let Some(answer) = todo::hold(p, &session, checkout, cwd)
            {
                print_envelope(&answer);
            }
            record_in(p, cli.event, checkout, cwd);
            Ok(())
        }),
        // A new prompt changes the agent's context under a refused stop, so
        // its next stop with the same open todos is refused again. Nothing
        // reaches stdout, which the harness appends to the prompt.
        HookEvent::UserPromptSubmit => with_payload(harness, cli.event, |p, checkout, cwd| {
            if let Ok(holder) = p.holder() {
                todo::rearm(&holder);
            }
            record_in(p, cli.event, checkout, cwd);
            Ok(())
        }),
        // Compaction is what drops the injected rules out of the agent's
        // context, so clearing the set is what lets them inject again. It
        // drops the reminder of open todos too, so the hold re-arms.
        HookEvent::PostCompact => with_payload(harness, cli.event, |p, checkout, cwd| {
            if let Ok(holder) = p.holder() {
                todo::rearm(&holder);
                rules::clear_for_holder(&holder);
            }
            record_in(p, cli.event, checkout, cwd);
            Ok(())
        }),
        // Record-only. Each reads stdin, builds one record and exits; nothing
        // reaches stdout, because `PermissionRequest` honours a JSON decision.
        event => with_payload(harness, event, |p, checkout, cwd| {
            record_in(p, event, checkout, cwd);
            Ok(())
        }),
    }
}

/// Delete the ending session's `issue render` receipts in the payload's
/// checkout. Best-effort: a receipt left behind is swept by a later render.
fn clear_issue_receipts(payload: &Payload, checkout: &Checkout) {
    if let (Some(session), Some(root)) = (
        payload.session_id(),
        crate::issue::receipt::store_root(checkout),
    ) {
        let _ = crate::issue::receipt::clear_session(&root, session);
    }
}

/// Write the record a verb beyond `pre-tool-use` carries, if any, and hand back
/// the settings so a caller that has more to do does not resolve them twice.
///
/// With logging off this is one global config read: no project layer, no git,
/// no tree-sitter.
fn record_in(
    payload: &Payload,
    event: HookEvent,
    checkout: &devkit_common::vcs::Checkout,
    cwd: &std::path::Path,
) -> devkit_common::harness_log::Settings {
    let settings = devkit_common::harness_log::resolve_in(checkout, cwd);
    if !settings.enabled {
        return settings;
    }
    let kind = record::record_only(payload, event, &settings);
    let rec = record::envelope(payload, event, checkout, kind);
    devkit_common::harness_log::record(&settings, &rec);
    settings
}

/// Read the hook payload from stdin. `None` covers an unreadable pipe, text
/// that is not JSON, and JSON that is not an object: none is a payload to
/// judge, and the caller's fail-closed rule is the same for all three.
pub(crate) fn read_payload(harness: Option<AnyHarness>, event: HookEvent) -> Option<Payload> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).ok()?;
    Payload::new(harness, event, serde_json::from_str::<Value>(&buf).ok()?)
}

/// Run a verb over the payload, or do nothing when there is none to read. A
/// verb reached here has no verdict to fail toward, so an unreadable payload
/// is silence rather than a denial.
///
/// An absent payload is read as an empty object rather than skipped, so a verb
/// a harness fires with no body still records that it fired. The activity log
/// is written after the verb, whose releases come first. Both read the one
/// checkout the payload's directory resolves to. A session's end gets one
/// budget for all of its todo database work, since the harness gives that
/// hook little time in all; every other hook bounds each call on its own.
fn with_payload(
    harness: Option<AnyHarness>,
    event: HookEvent,
    f: impl FnOnce(&Payload, &Checkout, &Path) -> Result<()>,
) -> Result<()> {
    if event == HookEvent::SessionEnd {
        crate::todo::store::end_session_within(crate::todo::store::SESSION_END_DATABASE_BUDGET);
    }
    let payload = read_payload(harness, event).unwrap_or_else(|| Payload::empty(harness, event));
    let cwd = record::payload_cwd(&payload);
    let checkout = Checkout::at(&cwd);
    let out = f(&payload, &checkout, &cwd);
    activity::observe_within(&payload, event, &checkout, &cwd);
    out
}

/// [`with_payload`] for a stop that can be refused: `f` returns whether it
/// held the agent, whose run then stays open instead of ending.
fn with_payload_held(
    harness: Option<AnyHarness>,
    event: HookEvent,
    f: impl FnOnce(&Payload, &Checkout, &Path) -> bool,
) -> Result<()> {
    let payload = read_payload(harness, event).unwrap_or_else(|| Payload::empty(harness, event));
    let cwd = record::payload_cwd(&payload);
    let checkout = Checkout::at(&cwd);
    if f(&payload, &checkout, &cwd) {
        activity::seen_within(&payload, &checkout, &cwd);
    } else {
        activity::observe_within(&payload, event, &checkout, &cwd);
    }
    Ok(())
}

/// The retired `lockm hook <event>` spelling. Kept because an installed plugin
/// manifest can outlive the binary it was installed beside.
pub(crate) fn legacy_lock_event(event: &str) -> Result<()> {
    match event {
        "pretooluse" => pre_tool_use(None),
        "subagent-stop" => with_payload(None, HookEvent::SubagentStop, |p, _, _| {
            edit::release_subagent(p);
            Ok(())
        }),
        "session-end" => with_payload(None, HookEvent::SessionEnd, |p, _, _| {
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
pub(crate) fn pre_tool_use(harness: Option<AnyHarness>) -> Result<()> {
    let Some(payload) = read_payload(harness, HookEvent::PreToolUse) else {
        return shell::deny_unreadable_payload(harness);
    };
    let cwd = record::payload_cwd(&payload);
    let checkout = Checkout::at(&cwd);
    let verdict = match (edit::write(&payload), payload.tool()) {
        (Some(write), _) => edit::guard(&payload, write, &checkout, &cwd),
        (None, Some(pabal::Tool::Mcp { .. })) => mcp::guard(&payload, &checkout, &cwd),
        (None, _) => shell::guard(&payload, &checkout),
    };
    // After the verdict is out, so a slow activity log never delays it.
    activity::observe_within(&payload, HookEvent::PreToolUse, &checkout, &cwd);
    verdict
}

/// Write a hook's answer to stdout. A closed pipe or a full disk on the other
/// end must not turn a denial into a crash, so the write error is discarded
/// rather than let the `print!` family's internal panic through.
fn print_envelope(envelope: &str) {
    let _ = writeln!(std::io::stdout(), "{envelope}");
}
