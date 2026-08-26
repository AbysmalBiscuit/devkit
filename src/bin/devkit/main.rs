use anyhow::Result;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use devkit::completions::Shell;
use std::ffi::OsString;
use std::path::PathBuf;

mod auth;
mod brief;
mod docs;
mod doctor;
mod issue;
mod locks;
mod mcp;
mod ports;
mod run;
mod schema;
mod shim;

#[derive(Parser)]
#[command(
    name = "devkit",
    version,
    about = "Configure and diagnose the devkit toolkit",
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Validate and store a Linear or Slack credential, or report the GitHub
    /// identity devkit would use (GitHub stores nothing — `gh auth login` or
    /// `GH_TOKEN`/`GITHUB_TOKEN` already cover that credential).
    Auth {
        /// Credential to validate and store, or `github` to report identity.
        provider: Provider,
        /// Provide the token non-interactively instead of being prompted.
        /// Refused for `github`, which stores nothing.
        #[arg(long)]
        token: Option<String>,
    },
    /// Print a compact project brief (apps, tasks, live servers, library
    /// versions) for the current checkout; silent outside a devkit-managed
    /// project. Intended for coding-agent session hooks.
    Brief {
        /// Emit only the library-versions section — what a post-compaction
        /// re-injection wants, without respending the context compaction
        /// just reclaimed.
        #[arg(long)]
        pins_only: bool,
        /// Print nothing when this session already received the same brief.
        /// Reads `session_id` from the hook's stdin JSON.
        ///
        /// Rejected with `--pins-only`: the watermark records the whole brief,
        /// so suppressing on it after emitting only the library table would
        /// tell the session it had seen a brief it never got.
        #[arg(long, conflicts_with = "pins_only")]
        if_changed: bool,
        /// Wrap the brief in the JSON envelope Codex and Cursor read it from,
        /// instead of printing it bare. Claude Code injects a hook's plain
        /// stdout and needs no envelope.
        #[arg(long)]
        additional_context: bool,
    },
    /// Check configured credentials and report what is missing.
    Doctor {
        /// Emit the report as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Print the JSON Schema for `devkit.toml` to stdout. Editors that speak
    /// the TOML language server use it for completion and validation; see
    /// `docs/configuration.md`.
    Schema {
        #[command(subcommand)]
        cmd: Option<SchemaCmd>,
    },
    /// Print a shell-completion script (bash, zsh, fish, …) to stdout.
    Completions {
        /// Shell to emit the script for.
        shell: Shell,
    },
    /// Port registry for local dev servers.
    Ports(ports::PortsCli),
    /// Advisory file locks across sessions.
    Locks(locks::LocksCli),
    /// Version-correct local library docs and source checkouts.
    Docs(docs::DocsCli),
    /// Supervised dev servers and canned project tasks for an issue worktree,
    /// with optional baseline A/B.
    Run(run::RunCli),
    /// Issue lifecycle: setup, checkout-pr, status, info, end, sync-includes, prs, dashboard, review.
    Issue(issue::IssueCli),
    /// Serve the devkit MCP tools over stdio.
    Mcp(mcp::McpCli),
}

#[derive(Subcommand)]
enum SchemaCmd {
    /// Point a `devkit.toml` at the published schema, creating a starter one
    /// when it does not exist. Leaves a file that already names a schema alone.
    Init {
        /// The config to point at the schema.
        #[arg(default_value = "devkit.toml")]
        path: PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Provider {
    Linear,
    Slack,
    Github,
}

impl Provider {
    fn label(self) -> &'static str {
        match self {
            Provider::Linear => "Linear",
            Provider::Slack => "Slack",
            Provider::Github => "GitHub",
        }
    }
}

/// Build a tool's `Command` as a root command under `shim_name`, so an installed
/// hardlink of that name reports its own name and version instead of `devkit`'s.
fn shim_command(subcommand: &str, shim_name: &'static str) -> clap::Command {
    Cli::command()
        .find_subcommand(subcommand)
        .unwrap_or_else(|| panic!("no `{subcommand}` subcommand"))
        .clone()
        .name(shim_name)
        .bin_name(shim_name)
        .version(env!("CARGO_PKG_VERSION"))
}

/// Emit a completion script registered under the tool's shim name (e.g.
/// `portm`), so an installed hardlink of that name completes correctly.
fn emit_completions(shell: Shell, subcommand: &str, shim_name: &'static str) {
    let mut cmd = shim_command(subcommand, shim_name);
    clap_complete::generate(shell, &mut cmd, shim_name, &mut std::io::stdout());
}

fn dispatch_shim(s: &'static shim::Shim, args: Vec<OsString>) -> Result<()> {
    let matches = shim_command(s.subcommand, s.name).get_matches_from(args);
    match s.subcommand {
        "ports" => ports::run(ports::PortsCli::from_arg_matches(&matches)?),
        "locks" => locks::run(locks::LocksCli::from_arg_matches(&matches)?),
        "docs" => docs::run(docs::DocsCli::from_arg_matches(&matches)?),
        "run" => run::run(run::RunCli::from_arg_matches(&matches)?),
        "issue" => issue::run(issue::IssueCli::from_arg_matches(&matches)?),
        "mcp" => mcp::run(mcp::McpCli::from_arg_matches(&matches)?),
        other => unreachable!("shim `{}` selects unknown subcommand `{other}`", s.name),
    }
}

fn main() -> Result<()> {
    let args: Vec<OsString> = std::env::args_os().collect();
    let argv0 = args.first().map(|a| a.to_string_lossy().into_owned());
    if let Some(s) = argv0.as_deref().and_then(shim::resolve) {
        devkit_common::report::install_panic_hook(s.name);
        devkit_common::paths::migrate_legacy_state();
        return dispatch_shim(s, args);
    }
    devkit_common::report::install_panic_hook("devkit");
    devkit_common::paths::migrate_legacy_state();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Auth { provider, token } => auth::run(provider, token),
        Cmd::Brief {
            pins_only,
            if_changed,
            additional_context,
        } => brief::run(pins_only, if_changed, additional_context),
        Cmd::Schema { cmd } => match cmd {
            None => schema::run(),
            Some(SchemaCmd::Init { path }) => schema::init(&path),
        },
        Cmd::Doctor { json } => doctor::run(json),
        Cmd::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "devkit", &mut std::io::stdout());
            Ok(())
        }
        Cmd::Ports(c) => ports::run(c),
        Cmd::Locks(c) => locks::run(c),
        Cmd::Docs(c) => docs::run(c),
        Cmd::Run(c) => run::run(c),
        Cmd::Issue(c) => issue::run(c),
        Cmd::Mcp(c) => mcp::run(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `dispatch_shim` matches on a `&str` with `unreachable!` as the
    /// fallback, so a `SHIMS` entry with no dispatch arm is a runtime panic
    /// rather than a build error. This is the compile-time-adjacent guard for
    /// that: every shim's subcommand must actually exist on `Cli`.
    #[test]
    fn every_shim_selects_a_real_subcommand() {
        for s in shim::SHIMS {
            assert!(
                Cli::command().find_subcommand(s.subcommand).is_some(),
                "shim `{}` selects unknown subcommand `{}`",
                s.name,
                s.subcommand
            );
        }
    }
}
