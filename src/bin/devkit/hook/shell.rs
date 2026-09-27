//! The shell path of `devkit hook pre-tool-use`.
//!
//! One analysis of the command feeds two stages. The command guard fails
//! open: its own failures allow the command. The write stage fails closed: a
//! write it cannot evaluate is a denial, and the claims it can evaluate go
//! through the write gate, which denies on the registry's failures too.
//!
//! The payload arrives already read and parsed: the verb dispatch owns the
//! stdin read, because it is what decides between this path and the edit one.

use std::{io::Write, path::Path, sync::OnceLock};

use anyhow::Result;
use devkit_command::{Analysis, Context, Dialect, Limits, PathStyle};
use devkit_common::{
    harness::{self, HarnessPolicy},
    harness_log::{self, Decision, Kind, Record, ShellPre, Verdict},
    vcs::Checkout,
};
use devkit_ports::guard::{self, Project};
use pabal::Tool;
use serde_json::Value;

use super::{
    HookEvent, dialect,
    gate::{self, Armed, WriteGate, WriteVerdict},
    payload::{Harness, MissingSession, Payload},
    print_envelope, record, writes,
};

const UNUSABLE_SHELL_REASON: &str =
    "devkit write-harness: shell payload could not be evaluated (fail-closed)";
/// Tool names that run shell commands, `Shell` being Cursor's. One that
/// arrives without a readable command is a harness format change.
const SHELL_TOOLS: [&str; 3] = ["Bash", "PowerShell", "Shell"];

enum Response {
    Silent,
    Envelope(String),
}

/// What the guard decided, and the record of it. The record is `None` when
/// logging is off, which is the default.
struct Outcome {
    response: Response,
    record: Option<Box<Record>>,
}

impl Outcome {
    fn silent() -> Self {
        Outcome {
            response: Response::Silent,
            record: None,
        }
    }

    fn with(mut self, record: Option<Box<Record>>) -> Self {
        self.record = record;
        self
    }
}

/// What `respond` resolved, carried out of the `catch_unwind` closure: the
/// panic arm's record names the call, and every record lands against settings
/// resolved once.
#[derive(Clone)]
struct PanicContext {
    checkout: Checkout,
    settings: harness_log::Settings,
}

/// Guard a shell command about to run. Never returns an error: a panic allows
/// the command unless the write stage had started, in which case it denies.
pub fn guard(payload: &Payload) -> Result<()> {
    let panic_ctx: OnceLock<PanicContext> = OnceLock::new();
    let gate = WriteGate::live();
    match gate::guarded(|armed| respond(payload, &gate, armed, &panic_ctx)) {
        Ok(out) => {
            if let Response::Envelope(v) = &out.response {
                print_envelope(v);
            }
            finish(out.record.as_deref(), panic_ctx.get().map(|c| &c.settings));
        }
        Err(panicked) => {
            if !panicked.denied {
                warn("command guard panicked; allowing the command");
            }
            let ctx = panic_ctx.get();
            let panicked = ctx.map(|ctx| {
                undecided_record(
                    payload,
                    &ctx.checkout,
                    &ctx.settings,
                    "the command guard panicked while evaluating this command",
                )
            });
            finish(panicked.as_deref(), ctx.map(|c| &c.settings));
        }
    }
    Ok(())
}

/// Flush the envelope, then write the record.
///
/// The order is the contract. `harness_log::record` catches its own panics but
/// cannot be made block-safe, and the log directory is configurable to a
/// network home, so `create_dir_all` and the first append can stall. A stall
/// before the envelope exists runs into the manifest's timeout, and a
/// harness timeout *allows* the call — which would turn a denial the write
/// stage had already decided into an allow. Printing first costs nothing: a
/// closed stdout pipe fails the write immediately rather than blocking, and the
/// record still lands afterwards.
fn finish(rec: Option<&Record>, settings: Option<&harness_log::Settings>) {
    let _ = std::io::stdout().flush();
    if let (Some(rec), Some(settings)) = (rec, settings) {
        harness_log::record(settings, rec);
    }
}

/// A payload devkit could not read at all: an unreadable pipe, text that is
/// not JSON, or JSON that is not an object. The signature of a harness format
/// change, and an unevaluable write fails closed.
///
/// A payload devkit could not read is precisely what the log exists for, so
/// this is a log-then-return rather than a bare return.
pub fn deny_unreadable_payload(declared: Option<Harness>) -> Result<()> {
    let cwd = current_cwd();
    let checkout = Checkout::at(&cwd);
    let payload = Payload::empty(declared);
    if harness::writes_enabled(&checkout, &cwd) {
        print_envelope(&payload.harness().deny(UNUSABLE_SHELL_REASON));
    }
    let settings = harness_log::resolve_in(&checkout, &cwd);
    let rec = settings.enabled.then(|| {
        undecided_record(
            &payload,
            &checkout,
            &settings,
            "the hook payload could not be read as a JSON object",
        )
    });
    finish(rec.as_deref(), Some(&settings));
    Ok(())
}

/// A record for a call the guard could not decide: an unreadable payload, a
/// panic, or a write stage that missed its deadline. Each of these is one of
/// the operational signals the log exists to collect, and each would otherwise
/// leave at most one stderr line that scrolls away.
fn undecided_record(
    payload: &Payload,
    checkout: &Checkout,
    settings: &harness_log::Settings,
    reason: &str,
) -> Box<Record> {
    let raw = payload.raw();
    let command = raw
        .get("tool_input")
        .and_then(|ti| ti.get("command"))
        .or_else(|| raw.get("command"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let (command, truncated) = record::truncate(command);
    let (command, redacted) = harness_log::redact::apply(&command, settings.command);
    record::envelope(
        payload,
        HookEvent::PreToolUse,
        checkout,
        Kind::ShellPre(Box::new(ShellPre {
            command,
            redacted,
            truncated,
            tool_name: payload.tool_name().map(str::to_string),
            dialect: None,
            analysis: None,
            verdict: Verdict {
                decision: Decision::Undecided,
                blocks: vec![reason.to_string()],
                warnings: Vec::new(),
            },
        })),
    )
}

fn deny(which: Harness, reasons: &[String]) -> Outcome {
    Outcome {
        response: Response::Envelope(which.deny(&reasons.join("\n"))),
        record: None,
    }
}

fn current_cwd() -> std::path::PathBuf {
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

/// A shell-tool event whose command could not be read, as opposed to an event
/// about some other tool, which this path has no business judging.
fn deny_unusable_shell(payload: &Payload, checkout: &Checkout) -> Response {
    let shell_event = payload.event_name().is_some()
        && payload
            .tool_name()
            .is_some_and(|tool| SHELL_TOOLS.contains(&tool));
    if shell_event && harness::writes_enabled(checkout, checkout.dir()) {
        Response::Envelope(payload.harness().deny(UNUSABLE_SHELL_REASON))
    } else {
        Response::Silent
    }
}

fn respond(
    payload: &Payload,
    gate: &WriteGate,
    armed: &Armed,
    panic_ctx: &OnceLock<PanicContext>,
) -> Outcome {
    let Some(Tool::Shell {
        command,
        cwd: shell_cwd,
        shell,
    }) = payload.tool()
    else {
        let checkout = Checkout::at(&record::payload_cwd(payload));
        let response = deny_unusable_shell(payload, &checkout);
        let settings = harness_log::resolve_in(&checkout, checkout.dir());
        let rec = settings.enabled.then(|| {
            undecided_record(
                payload,
                &checkout,
                &settings,
                "the payload did not parse as a shell command",
            )
        });
        let _ = panic_ctx.set(PanicContext { checkout, settings });
        return Outcome {
            response,
            record: rec,
        };
    };
    let which = payload.harness();
    let Some(cwd) = shell_cwd
        .map(std::path::Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
    else {
        // There is no directory to resolve config against, so there is nothing
        // to log to either: `resolve` needs one to find the project layers.
        return Outcome::silent();
    };
    // One `git worktree list` for the whole invocation. The log settings, the
    // two enforcement gates, the rule layers, the config load and the write
    // stage's lock scoping all read this checkout instead of asking git for
    // themselves. Resolution is lazy, so a harness switched off by environment
    // still spawns nothing.
    let checkout = Checkout::at(&cwd);
    let settings = harness_log::resolve_in(&checkout, &cwd);
    let _ = panic_ctx.set(PanicContext {
        checkout: checkout.clone(),
        settings: settings.clone(),
    });

    let commands_on = harness::commands_enabled(&checkout, &cwd);
    let writes_on = gate::enabled(which, &checkout, &cwd);
    // With logging on and both gates off, the analysis runs anyway and this
    // early return is skipped: a record with an empty verdict is half a record.
    // That is the accepted trade, bounded by logging being off by default and
    // enablable only from the global config.
    if !commands_on && !writes_on && !settings.enabled {
        return Outcome::silent();
    }
    if writes_on {
        armed.arm(which);
    }

    let (rules, warnings) = harness::resolve_rules_in(&checkout, &cwd);
    for w in &warnings {
        warn(w);
    }
    let dialect = dialect::resolve(rules.policy.shell, which, shell.as_ref(), cfg!(windows));
    let started = std::time::Instant::now();
    let analysis = devkit_command::analyze(command, &context(dialect, shell_cwd));
    let analyze_micros = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;

    let mut blocks: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    if commands_on {
        let project = load_project(&checkout, &cwd, rules.app_match.clone());
        let verdict = guard::decide(&analysis, &rules.commands, project.as_ref());
        blocks.extend(verdict.blocks.into_iter().map(|f| f.message));
        notes.extend(verdict.warnings.into_iter().map(|f| f.message));
    }
    // Everything the record needs that the write stage may consume.
    let shell_record = |decision, blocks: &[String], notes: &[String]| {
        settings.enabled.then(|| {
            shell_pre_record(
                payload,
                command,
                &checkout,
                &settings,
                dialect,
                Some(record::projection(&analysis, analyze_micros)),
                Verdict {
                    decision,
                    blocks: blocks.to_vec(),
                    warnings: notes.to_vec(),
                },
            )
        })
    };
    if writes_on {
        let holder = payload.holder();
        let verdict = write_stage(
            &analysis,
            &rules.policy,
            !blocks.is_empty(),
            holder.as_deref().map_err(|&missing| missing),
            gate,
            &checkout,
            &cwd,
        );
        blocks.extend(verdict.blocks);
        notes.extend(verdict.warnings);
    }
    if !blocks.is_empty() {
        let rec = shell_record(Decision::Deny, &blocks, &notes);
        return deny(which, &blocks).with(rec);
    }
    let rec = shell_record(Decision::Allow, &[], &notes);
    if notes.is_empty() {
        return Outcome::silent().with(rec);
    }
    let response = payload
        .pre_tool_use_context(&notes.join("\n"))
        .map_or(Response::Silent, Response::Envelope);
    Outcome {
        response,
        record: rec,
    }
}

fn context(dialect: Dialect, cwd: Option<&Path>) -> Context {
    Context {
        dialect,
        cwd: cwd.map(|p| p.to_string_lossy().into_owned()),
        path_style: if cfg!(windows) {
            PathStyle::Windows
        } else {
            PathStyle::Unix
        },
        limits: Limits::default(),
    }
}

/// The write stage: the evaluation's own findings, then, when nothing has
/// blocked the command yet, the gate's.
fn write_stage<R>(
    analysis: &Analysis,
    policy: &HarnessPolicy,
    blocked: bool,
    holder: Result<&str, MissingSession>,
    gate: &WriteGate<R>,
    checkout: &Checkout,
    cwd: &Path,
) -> WriteVerdict
where
    R: devkit_locks::Registry + Send + Sync + 'static,
{
    let evaluation = writes::evaluate(analysis, policy);
    let mut verdict = WriteVerdict {
        blocks: evaluation.blocks,
        warnings: evaluation.warnings,
    };
    if blocked || !verdict.blocks.is_empty() || evaluation.claims.is_empty() {
        return verdict;
    }
    let holder = match holder {
        Ok(holder) => holder,
        Err(missing) => {
            verdict
                .blocks
                .push(format!("{} {missing} (fail-closed)", gate::PREFIX));
            return verdict;
        }
    };
    let found = gate.decide(
        &evaluation.claims,
        holder,
        checkout,
        cwd,
        policy.unresolved_writes,
    );
    verdict.blocks.extend(found.blocks);
    verdict.warnings.extend(found.warnings);
    verdict
}

/// The `shell_pre` record for a call the guard actually evaluated.
fn shell_pre_record(
    payload: &Payload,
    command: &str,
    checkout: &Checkout,
    settings: &harness_log::Settings,
    dialect: devkit_command::Dialect,
    analysis: Option<devkit_common::harness_log::AnalysisProjection>,
    verdict: Verdict,
) -> Box<Record> {
    let (command, truncated) = record::truncate(command);
    let (command, redacted) = harness_log::redact::apply(&command, settings.command);
    record::envelope(
        payload,
        HookEvent::PreToolUse,
        checkout,
        Kind::ShellPre(Box::new(ShellPre {
            command,
            redacted,
            truncated,
            tool_name: payload.tool_name().map(str::to_string),
            dialect: Some(dialect.name().to_string()),
            analysis,
            verdict,
        })),
    )
}

/// Write a diagnostic to stderr, ignoring a write failure for the same reason
/// `print_envelope` does.
fn warn(msg: &str) {
    let _ = writeln!(std::io::stderr(), "devkit: {msg}");
}

/// The resolved config and app catalog, or `None` when this is not a devkit
/// project or the config will not load. Read through `load_quiet`: an
/// unresolvable app is not this command's business to report.
///
/// A config that does not exist anywhere the search looks is silence — most
/// directories are not devkit projects. A config that exists and fails to
/// parse or deserialize is different: it silently drops the task- and
/// app-aware guard sources while leaving `[harness.commands]` rules working,
/// so that case is worth a line on stderr naming what broke.
fn load_project(
    checkout: &Checkout,
    cwd: &std::path::Path,
    app_match: devkit_config::AppMatch,
) -> Option<Project> {
    let loaded = match devkit_ports::load::load_quiet_in(checkout, None, cwd) {
        Ok(loaded) => loaded,
        Err(e) if e.downcast_ref::<devkit_config::NoConfig>().is_some() => return None,
        Err(e) => {
            let chain = e
                .chain()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(": ");
            warn(&format!(
                "devkit.toml failed to load ({chain}); guarding with [harness.commands] rules only"
            ));
            return None;
        }
    };
    // `root`, not `main_checkout`: the latter is `None` when this *is* the
    // primary clone, and in a linked worktree it names a directory the cwd is
    // never under, so the relative path would never resolve anywhere.
    let cwd_rel = checkout
        .root()
        .and_then(|r| cwd.strip_prefix(r).ok().map(|p| p.to_path_buf()))
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .filter(|s| !s.is_empty());
    Some(Project {
        config: loaded.config,
        catalog: loaded.catalog,
        cwd_rel,
        app_match,
    })
}

#[cfg(test)]
mod tests;
