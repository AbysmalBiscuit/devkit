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
use devkit_common::harness::{self, Harness, ShellPayload};
use devkit_ports::guard::{self, Project};
use serde_json::Value;

use super::{dialect, writes};

// The fail-open contract below is `catch_unwind`, which catches nothing under
// an aborting panic strategy. Nothing else ties the compile profile to this
// file, so the dependency is stated where it is relied on.
#[cfg(panic = "abort")]
compile_error!(
    "`devkit hook pre-tool-use` fails open through catch_unwind; the release profile must unwind"
);

/// Longer than a healthy registry ever takes and well inside the manifest's
/// 30-second timeout, which allows the call when it fires.
const WRITE_STAGE_DEADLINE: Duration = Duration::from_secs(5);
const UNUSABLE_SHELL_REASON: &str =
    "devkit write-harness: shell payload could not be evaluated (fail-closed)";

enum Response {
    Silent,
    Envelope(serde_json::Value),
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
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        respond(payload, declared, &write_stage)
    }));
    match outcome {
        Ok(Response::Envelope(v)) => print_envelope(&v),
        Ok(Response::Silent) => {}
        Err(_) => match write_stage.get() {
            Some(h) => print_envelope(&harness::deny_shell_json(
                *h,
                "devkit write-harness: internal failure while evaluating a shell write (fail-closed)",
            )),
            None => warn("command guard panicked; allowing the command"),
        },
    }
    Ok(())
}

/// A payload devkit could not read at all: an unreadable pipe or text that is
/// not JSON. The signature of a harness format change, and an unevaluable write
/// fails closed.
pub fn deny_unreadable_payload(declared: Option<Harness>) -> Result<()> {
    if harness::writes_enabled(&current_cwd()) {
        let envelope = match declared {
            Some(h) => harness::deny_shell_json(h, UNUSABLE_SHELL_REASON),
            None => harness::deny_json(UNUSABLE_SHELL_REASON),
        };
        print_envelope(&envelope);
    }
    Ok(())
}

fn deny(which: Harness, reasons: &[String]) -> Response {
    Response::Envelope(harness::deny_shell_json(which, &reasons.join("\n")))
}

fn current_cwd() -> std::path::PathBuf {
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

/// Recover a shell identity from a payload that parsed as JSON but not as a
/// shell payload. With `--harness` the identity half is already settled, so
/// this is left deciding only whether the event is about a shell command at
/// all, and where it would have run.
fn raw_shell_context(
    payload: &serde_json::Value,
    declared: Option<Harness>,
) -> Option<(Harness, Option<std::path::PathBuf>)> {
    payload
        .get("hook_event_name")
        .and_then(serde_json::Value::as_str)?;
    let tool = payload
        .get("tool_name")
        .and_then(serde_json::Value::as_str)?;
    if !harness::SHELL_TOOLS.contains(&tool) {
        return None;
    }
    let harness = declared.unwrap_or_else(|| harness::infer_harness(payload));
    let cwd = payload
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from);
    Some((harness, cwd))
}

fn deny_unusable_shell(payload: &serde_json::Value, declared: Option<Harness>) -> Response {
    let Some((which, cwd)) = raw_shell_context(payload, declared) else {
        return Response::Silent;
    };
    let cwd = cwd.unwrap_or_else(current_cwd);
    if harness::writes_enabled(&cwd) {
        Response::Envelope(harness::deny_shell_json(which, UNUSABLE_SHELL_REASON))
    } else {
        Response::Silent
    }
}

fn respond(
    payload: &Value,
    declared: Option<Harness>,
    write_stage: &OnceLock<Harness>,
) -> Response {
    let Some(shell) = harness::parse_shell_payload(payload) else {
        return deny_unusable_shell(payload, declared);
    };
    let which = resolve_harness(declared, &shell);
    let Some(cwd) = shell.cwd.clone().or_else(|| std::env::current_dir().ok()) else {
        return Response::Silent;
    };
    let commands_on = harness::commands_enabled(&cwd);
    let writes_on = which != Harness::Cursor && harness::writes_enabled(&cwd);
    if !commands_on && !writes_on {
        return Response::Silent;
    }
    if writes_on {
        let _ = write_stage.set(which);
    }

    let (rules, warnings) = harness::resolve_rules(&cwd);
    for w in &warnings {
        warn(w);
    }
    let ctx = Context {
        dialect: dialect::resolve(
            rules.policy.shell,
            which,
            shell.tool_name.as_deref(),
            cfg!(windows),
        ),
        cwd: shell.cwd.as_ref().map(|p| p.to_string_lossy().into_owned()),
        path_style: if cfg!(windows) {
            PathStyle::Windows
        } else {
            PathStyle::Unix
        },
        limits: Limits::default(),
    };
    let analysis = devkit_command::analyze(&shell.command, &ctx);

    let mut blocks: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    if commands_on {
        let project = load_project(&cwd, rules.app_match.clone());
        let verdict = guard::decide(&analysis, &rules.commands, project.as_ref());
        blocks.extend(verdict.blocks.into_iter().map(|f| f.message));
        notes.extend(verdict.warnings.into_iter().map(|f| f.message));
    }
    if writes_on {
        let evaluation = writes::evaluate(&analysis, &rules.policy);
        blocks.extend(evaluation.blocks.iter().cloned());
        notes.extend(evaluation.warnings.iter().cloned());
        if blocks.is_empty() && evaluation.needs_registry() {
            let Some(session) = shell.session_id.clone() else {
                return deny(which, &[
                    "devkit write-harness: shell write payload carries no session_id (fail-closed)"
                        .into(),
                ]);
            };
            let holder =
                devkit_locks::hook::holder_from_fields(&session, shell.agent_id.as_deref());
            match writes::with_deadline(WRITE_STAGE_DEADLINE, move || {
                writes::enforce(&evaluation, &holder)
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
                    print_envelope(&harness::deny_shell_json(
                        which,
                        &format!(
                            "devkit write-harness: the lock registry did not answer within {}s (fail-closed). Retry; if it persists, check `lockm status` and `devkit doctor`.",
                            WRITE_STAGE_DEADLINE.as_secs()
                        ),
                    ));
                    // The worker is still blocked on the registry; exiting the
                    // process is what ends it.
                    std::process::exit(0);
                }
            }
        }
    }
    if !blocks.is_empty() {
        return deny(which, &blocks);
    }
    if notes.is_empty() {
        return Response::Silent;
    }
    harness::warn_shell_json(which, &notes.join("\n")).map_or(Response::Silent, Response::Envelope)
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
fn load_project(cwd: &std::path::Path, app_match: devkit_config::AppMatch) -> Option<Project> {
    let loaded = match devkit_ports::load::load_quiet(None, cwd) {
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
    // `checkout_root`, not `main_checkout`: the latter is `None` when this *is*
    // the primary clone, and in a linked worktree it names a directory the cwd
    // is never under, so the relative path would never resolve anywhere.
    let cwd_rel = devkit_common::git::checkout_root(cwd)
        .ok()
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
