//! `devkit harness shell`: the pre-execution command guard.
//!
//! Reads a harness's hook payload on stdin and either emits that harness's deny
//! envelope or says nothing. Every failure path exits 0 — a missed nudge costs
//! nothing, while a false denial would block legitimate work on every command.

use anyhow::Result;
use clap::{Args, Subcommand};
use devkit_common::harness::{self, ShellPayload};
use devkit_ports::guard::{self, Decision, Project};
use std::io::{Read, Write};

#[derive(Args)]
pub struct HarnessCli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Guard a shell command about to run, reading the payload on stdin.
    Shell,
}

pub fn run(cli: HarnessCli) -> Result<()> {
    match cli.cmd {
        Cmd::Shell => {
            guard_shell();
            Ok(())
        }
    }
}

/// Never returns an error and never panics out: a guard that fails a tool call
/// is worse than a guard that misses one.
fn guard_shell() {
    let outcome = std::panic::catch_unwind(|| {
        let mut buf = String::new();
        if std::io::stdin().read_to_string(&mut buf).is_err() {
            return None;
        }
        let payload: serde_json::Value = serde_json::from_str(&buf).ok()?;
        let ShellPayload {
            harness: which,
            command,
            cwd,
        } = harness::parse_shell_payload(&payload)?;

        let cwd = cwd.or_else(|| std::env::current_dir().ok())?;
        if !harness::commands_enabled(&cwd) {
            return None;
        }

        let (rules, warnings) = harness::resolve_rules(&cwd);
        for w in warnings {
            warn(&w);
        }
        let project = load_project(&cwd, rules.app_match.clone());
        match guard::decide_with(&command, &rules.commands, project.as_ref()) {
            Decision::Allow => None,
            Decision::Deny { reason } => Some(harness::deny_shell_json(which, &reason)),
        }
    });

    match outcome {
        Ok(Some(envelope)) => print_envelope(&envelope),
        Ok(None) => {}
        Err(_) => warn("command guard panicked; allowing the command"),
    }
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
