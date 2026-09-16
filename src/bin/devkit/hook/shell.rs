//! The shell path of `devkit hook pre-tool-use`.
//!
//! One analysis of the command feeds two stages. The command guard fails
//! open: its own failures allow the command. The write stage, active for
//! Claude Code and Codex when `enforce_writes` is on, fails closed: a write it
//! cannot evaluate, a registry it cannot reach, or a deadline it misses is a
//! denial.
//!
//! The payload arrives already read and parsed: the verb dispatch owns the
//! stdin read, because it is what decides between this path and the edit one.

use std::{io::Write, sync::OnceLock, time::Duration};

use anyhow::Result;
use devkit_command::{Context, Limits, PathStyle};
use devkit_common::{
    git::Checkout,
    harness::{self, Harness, ShellPayload},
    harness_log::{self, Decision, Kind, Record, ShellPre, Verdict},
};
use devkit_ports::guard::{self, Project};
use serde_json::Value;

use super::{HookEvent, dialect, record, writes};

// The fail-open contract below is `catch_unwind`, which catches nothing under
// an aborting panic strategy. Nothing else ties the compile profile to this
// file, so the dependency is stated where it is relied on.
#[cfg(panic = "abort")]
compile_error!(
    "`devkit hook pre-tool-use` fails open through catch_unwind; the release profile must unwind"
);

/// Far longer than a healthy registry takes, and below the manifest timeout,
/// which allows the call when it fires. Keep the manifest at this plus the
/// record deadline plus a second or two of process startup.
const WRITE_STAGE_DEADLINE: Duration = Duration::from_secs(2);
const UNUSABLE_SHELL_REASON: &str =
    "devkit write-harness: shell payload could not be evaluated (fail-closed)";

enum Response {
    Silent,
    Envelope(serde_json::Value),
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
    harness: Option<Harness>,
    checkout: Checkout,
    settings: harness_log::Settings,
}

/// The harness the manifest named, else the payload's own shape. A manifest
/// devkit wrote already knows which harness reads it, so passing that in beats
/// inferring it from which fields a vendor happens to send this release.
fn resolve_harness(declared: Option<Harness>, shell: &ShellPayload) -> Harness {
    declared.unwrap_or(shell.harness)
}

/// Guard a shell command about to run. Never returns an error: a panic allows
/// the command unless the write stage had started, in which case it denies.
pub fn guard(payload: &Value, declared: Option<Harness>) -> Result<()> {
    let write_stage: OnceLock<Harness> = OnceLock::new();
    let panic_ctx: OnceLock<PanicContext> = OnceLock::new();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        respond(payload, declared, &write_stage, &panic_ctx)
    }));
    match outcome {
        Ok(out) => {
            if let Response::Envelope(v) = &out.response {
                print_envelope(v);
            }
            finish(out.record.as_deref(), panic_ctx.get().map(|c| &c.settings));
        }
        Err(_) => {
            match write_stage.get() {
                Some(h) => print_envelope(&harness::deny_shell_json(
                    *h,
                    "devkit write-harness: internal failure while evaluating a shell write (fail-closed)",
                )),
                None => warn("command guard panicked; allowing the command"),
            }
            let ctx = panic_ctx.get();
            let panicked = ctx.map(|ctx| {
                undecided_record(
                    payload,
                    ctx.harness,
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

/// A payload devkit could not read at all: an unreadable pipe or text that is
/// not JSON. The signature of a harness format change, and an unevaluable write
/// fails closed.
///
/// A payload devkit could not read is precisely what the log exists for, so
/// this is a log-then-return rather than a bare return.
pub fn deny_unreadable_payload(declared: Option<Harness>) -> Result<()> {
    let cwd = current_cwd();
    let checkout = Checkout::at(&cwd);
    if harness::writes_enabled(&checkout, &cwd) {
        let envelope = match declared {
            Some(h) => harness::deny_shell_json(h, UNUSABLE_SHELL_REASON),
            None => harness::deny_json(UNUSABLE_SHELL_REASON),
        };
        print_envelope(&envelope);
    }
    let settings = harness_log::resolve_in(&checkout, &cwd);
    let rec = settings.enabled.then(|| {
        undecided_record(
            &Value::Null,
            declared,
            &checkout,
            &settings,
            "the hook payload could not be read as JSON",
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
    payload: &Value,
    declared: Option<Harness>,
    checkout: &Checkout,
    settings: &harness_log::Settings,
    reason: &str,
) -> Box<Record> {
    let command = payload
        .get("tool_input")
        .and_then(|ti| ti.get("command"))
        .or_else(|| payload.get("command"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let (command, truncated) = record::truncate(command);
    let (command, redacted) = harness_log::redact::apply(&command, settings.command);
    record::envelope(
        payload,
        HookEvent::PreToolUse,
        declared,
        checkout,
        Kind::ShellPre(Box::new(ShellPre {
            command,
            redacted,
            truncated,
            tool_name: payload
                .get("tool_name")
                .and_then(Value::as_str)
                .map(str::to_string),
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
        response: Response::Envelope(harness::deny_shell_json(which, &reasons.join("\n"))),
        record: None,
    }
}

fn current_cwd() -> std::path::PathBuf {
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

/// Recover a shell identity from a payload that parsed as JSON but not as a
/// shell payload. With `--harness` the identity half is already settled, so
/// this is left deciding only whether the event is about a shell command at
/// all, and where it would have run.
fn raw_shell_context(payload: &serde_json::Value, declared: Option<Harness>) -> Option<Harness> {
    payload
        .get("hook_event_name")
        .and_then(serde_json::Value::as_str)?;
    let tool = payload
        .get("tool_name")
        .and_then(serde_json::Value::as_str)?;
    if !harness::SHELL_TOOLS.contains(&tool) {
        return None;
    }
    Some(declared.unwrap_or_else(|| harness::infer_harness(payload)))
}

fn deny_unusable_shell(
    payload: &serde_json::Value,
    declared: Option<Harness>,
    checkout: &Checkout,
) -> Response {
    let Some(which) = raw_shell_context(payload, declared) else {
        return Response::Silent;
    };
    if harness::writes_enabled(checkout, checkout.dir()) {
        Response::Envelope(harness::deny_shell_json(which, UNUSABLE_SHELL_REASON))
    } else {
        Response::Silent
    }
}

fn respond(
    payload: &Value,
    declared: Option<Harness>,
    write_stage: &OnceLock<Harness>,
    panic_ctx: &OnceLock<PanicContext>,
) -> Outcome {
    let Some(shell) = harness::parse_shell_payload(payload) else {
        let checkout = Checkout::at(&record::payload_cwd(payload));
        let response = deny_unusable_shell(payload, declared, &checkout);
        let settings = harness_log::resolve_in(&checkout, checkout.dir());
        let rec = settings.enabled.then(|| {
            undecided_record(
                payload,
                declared,
                &checkout,
                &settings,
                "the payload did not parse as a shell command",
            )
        });
        let _ = panic_ctx.set(PanicContext {
            harness: declared,
            checkout,
            settings,
        });
        return Outcome {
            response,
            record: rec,
        };
    };
    let which = resolve_harness(declared, &shell);
    let Some(cwd) = shell.cwd.clone().or_else(|| std::env::current_dir().ok()) else {
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
        harness: Some(which),
        checkout: checkout.clone(),
        settings: settings.clone(),
    });

    let commands_on = harness::commands_enabled(&checkout, &cwd);
    let writes_on = which != Harness::Cursor && harness::writes_enabled(&checkout, &cwd);
    // With logging on and both gates off, the analysis runs anyway and this
    // early return is skipped: a record with an empty verdict is half a record.
    // That is the accepted trade, bounded by logging being off by default and
    // enablable only from the global config.
    if !commands_on && !writes_on && !settings.enabled {
        return Outcome::silent();
    }
    if writes_on {
        let _ = write_stage.set(which);
    }

    let (rules, warnings) = harness::resolve_rules_in(&checkout, &cwd);
    for w in &warnings {
        warn(w);
    }
    let dialect = dialect::resolve(
        rules.policy.shell,
        which,
        shell.tool_name.as_deref(),
        cfg!(windows),
    );
    let ctx = Context {
        dialect,
        cwd: shell.cwd.as_ref().map(|p| p.to_string_lossy().into_owned()),
        path_style: if cfg!(windows) {
            PathStyle::Windows
        } else {
            PathStyle::Unix
        },
        limits: Limits::default(),
    };
    let started = std::time::Instant::now();
    let analysis = devkit_command::analyze(&shell.command, &ctx);
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
                &shell,
                declared,
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
        let evaluation = writes::evaluate(&analysis, &rules.policy);
        blocks.extend(evaluation.blocks.iter().cloned());
        notes.extend(evaluation.warnings.iter().cloned());
        if blocks.is_empty() && evaluation.needs_registry() {
            let Some(session) = shell.session_id.clone() else {
                let reason =
                    "devkit write-harness: shell write payload carries no session_id (fail-closed)"
                        .to_string();
                let rec = shell_record(Decision::Deny, std::slice::from_ref(&reason), &notes);
                return deny(which, &[reason]).with(rec);
            };
            let holder =
                devkit_locks::hook::holder_from_fields(&session, shell.agent_id.as_deref());
            // The stage runs on its own thread, so it takes a clone of the
            // already-resolved checkout rather than a borrow.
            let checkout = checkout.clone();
            match writes::with_deadline(WRITE_STAGE_DEADLINE, move || {
                writes::enforce(&evaluation, &holder, checkout)
            }) {
                Ok(Ok(conflicts)) if conflicts.is_empty() => {}
                Ok(Ok(conflicts)) => blocks.push(writes::conflict_message(&conflicts)),
                Ok(Err(e)) => {
                    blocks.push(format!(
                        "devkit write-harness: registry error (fail-closed): {e:#}"
                    ));
                }
                Err(writes::StageError::Panicked) => blocks.push(
                    "devkit write-harness: internal failure while claiming shell write targets (fail-closed)"
                        .into(),
                ),
                Err(writes::StageError::TimedOut) => {
                    let reason = format!(
                        "devkit write-harness: the lock registry did not answer within {}s (fail-closed). Retry; if it persists, check `lockm status` and `devkit doctor`.",
                        WRITE_STAGE_DEADLINE.as_secs()
                    );
                    print_envelope(&harness::deny_shell_json(which, &reason));
                    // A deadline miss is one of the operational signals this
                    // log exists to collect, and the one path that would
                    // otherwise never reach it. Envelope, then record, then
                    // exit: the worker is still blocked on the registry, and
                    // exiting the process is what ends it.
                    let rec = shell_record(Decision::Deny, std::slice::from_ref(&reason), &notes);
                    finish(rec.as_deref(), Some(&settings));
                    std::process::exit(0);
                }
            }
        }
    }
    if !blocks.is_empty() {
        let rec = shell_record(Decision::Deny, &blocks, &notes);
        return deny(which, &blocks).with(rec);
    }
    let rec = shell_record(Decision::Allow, &[], &notes);
    if notes.is_empty() {
        return Outcome::silent().with(rec);
    }
    let response = harness::warn_shell_json(which, &notes.join("\n"))
        .map_or(Response::Silent, Response::Envelope);
    Outcome {
        response,
        record: rec,
    }
}

/// The `shell_pre` record for a call the guard actually evaluated.
#[allow(clippy::too_many_arguments)]
fn shell_pre_record(
    payload: &Value,
    shell: &ShellPayload,
    declared: Option<Harness>,
    checkout: &Checkout,
    settings: &harness_log::Settings,
    dialect: devkit_command::Dialect,
    analysis: Option<devkit_common::harness_log::AnalysisProjection>,
    verdict: Verdict,
) -> Box<Record> {
    let (command, truncated) = record::truncate(&shell.command);
    let (command, redacted) = harness_log::redact::apply(&command, settings.command);
    record::envelope(
        payload,
        HookEvent::PreToolUse,
        declared,
        checkout,
        Kind::ShellPre(Box::new(ShellPre {
            command,
            redacted,
            truncated,
            tool_name: shell.tool_name.clone(),
            dialect: Some(dialect.name().to_string()),
            analysis,
            verdict,
        })),
    )
}

/// Write a deny envelope to stdout. A closed pipe or a full disk on the
/// other end must not turn a denial into a crash, so the write error is
/// discarded rather than let the `print!` family's internal panic through.
fn print_envelope(envelope: &serde_json::Value) {
    let _ = writeln!(std::io::stdout(), "{envelope}");
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
