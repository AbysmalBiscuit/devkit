use anyhow::Result;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use devkit::completions::{self, Shell};
use std::ffi::OsString;
use std::path::PathBuf;

mod auth;
mod brief;
mod docs;
mod doctor;
mod issue;
mod links;
mod locks;
mod mcp;
mod ports;
mod run;
mod schema;
mod shim;

const SHIM_HELP: &str = "\
Also installed under their own names:
  issue       = devkit issue
  devrun      = devkit run
  portm       = devkit ports
  lockm       = devkit locks
  docm        = devkit docs
  devkit-mcp  = devkit mcp

Run `devkit install-links` if any of them are missing.";

#[derive(Parser)]
#[command(
    name = "devkit",
    version,
    about = "Configure and diagnose the devkit toolkit",
    propagate_version = true,
    after_help = SHIM_HELP
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Validate and store a Linear or Slack credential, or report the GitHub
    /// identity devkit would use (GitHub stores nothing, since `gh auth login`
    /// or `GH_TOKEN`/`GITHUB_TOKEN` already cover that credential).
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
        /// Emit only the library-versions section, which is what a
        /// post-compaction re-injection wants, without respending the context
        /// compaction just reclaimed.
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
    /// Print a shell-completion script (bash, zsh, fish, ...) to stdout.
    Completions {
        /// Shell to emit the script for.
        shell: Shell,
        /// Emit one script for `devkit` and one for every old name,
        /// concatenated, for installing them all from a single file.
        #[arg(long)]
        all: bool,
    },
    /// Port registry for local dev servers.
    #[command(display_name = "devkit ports")]
    Ports(ports::PortsCli),
    /// Advisory file locks across sessions.
    #[command(display_name = "devkit locks")]
    Locks(locks::LocksCli),
    /// Version-correct local library docs and source checkouts.
    #[command(display_name = "devkit docs")]
    Docs(docs::DocsCli),
    /// Supervised dev servers and canned project tasks for an issue worktree,
    /// with optional baseline A/B.
    #[command(display_name = "devkit run")]
    Run(run::RunCli),
    /// Issue lifecycle: setup, checkout-pr, status, info, end, sync-includes, prs, dashboard, review.
    #[command(display_name = "devkit issue")]
    Issue(issue::IssueCli),
    /// Serve the devkit MCP tools over stdio.
    #[command(display_name = "devkit mcp")]
    Mcp(mcp::McpCli),
    /// Install the old command names (`issue`, `devrun`, ...) as hardlinks
    /// beside this executable.
    InstallLinks(links::InstallLinksArgs),
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
pub(crate) fn shim_command(subcommand: &str, shim_name: &'static str) -> clap::Command {
    Cli::command()
        .find_subcommand(subcommand)
        .unwrap_or_else(|| panic!("no `{subcommand}` subcommand"))
        .clone()
        .name(shim_name)
        // Overrides the `devkit <sub>` spelling the subcommand carries for its
        // own version line: under this name it *is* the root command.
        .display_name(shim_name)
        .bin_name(shim_name)
        .version(env!("CARGO_PKG_VERSION"))
}

/// Emit a completion script registered under the tool's shim name (e.g.
/// `portm`), so an installed hardlink of that name completes correctly.
fn emit_completions(shell: Shell, subcommand: &str, shim_name: &'static str) -> Result<()> {
    let cmd = shim_command(subcommand, shim_name);
    Ok(completions::emit(shell, [(cmd, shim_name)])?)
}

/// Every command name that has a `completions` subcommand of its own, paired
/// with the script to emit for it: `devkit` first, then the old names in the
/// order `install-links` creates them.
///
/// Read off the command tree rather than listed, so a name whose subcommand
/// gains or loses `completions` is picked up without a second list to update.
/// `devkit-mcp` is absent today because `devkit mcp` takes no subcommands.
fn every_completion_script() -> Vec<(clap::Command, &'static str)> {
    let mut scripts = vec![(Cli::command(), "devkit")];
    scripts.extend(shim::SHIMS.iter().filter_map(|s| {
        let cmd = shim_command(s.sub.name(), s.name);
        cmd.find_subcommand("completions")?;
        Some((cmd, s.name))
    }));
    scripts
}

fn dispatch_shim(s: &'static shim::Shim, args: Vec<OsString>) -> Result<()> {
    let matches = shim_command(s.sub.name(), s.name).get_matches_from(args);
    match s.sub {
        shim::Sub::Ports => ports::run(ports::PortsCli::from_arg_matches(&matches)?),
        shim::Sub::Locks => locks::run(locks::LocksCli::from_arg_matches(&matches)?),
        shim::Sub::Docs => docs::run(docs::DocsCli::from_arg_matches(&matches)?),
        shim::Sub::Run => run::run(run::RunCli::from_arg_matches(&matches)?),
        shim::Sub::Issue => issue::run(issue::IssueCli::from_arg_matches(&matches)?),
        shim::Sub::Mcp => mcp::run(mcp::McpCli::from_arg_matches(&matches)?),
    }
}

fn main() -> Result<()> {
    let args: Vec<OsString> = std::env::args_os().collect();
    // The marker probe `links::answers_probe_marker` uses: answered before
    // any clap parsing, any panic-hook/state-migration/linking setup, and —
    // critically — before `devkit-mcp`'s normal path would start blocking on
    // stdin. Every shim is this same binary, so this one intercept covers all
    // six names.
    //
    // The other probe — `--version` — is an ordinary subcommand-shaped arg
    // with no intercept here, so a child spawned with it parses and runs the
    // way any real invocation does. What keeps that child from linking
    // anything is `DEVKIT_SKIP_AUTOLINK`, which `links::probe` sets on every
    // child it spawns; `ensure_current` returns on it before doing any work.
    if args.get(1).map(OsString::as_os_str) == Some(std::ffi::OsStr::new(shim::PROBE_FLAG)) {
        println!("{}", shim::PROBE_MARKER);
        return Ok(());
    }
    let argv0 = args.first().map(|a| a.to_string_lossy().into_owned());
    let shim = argv0.as_deref().and_then(shim::resolve);
    devkit_common::report::install_panic_hook(shim.map_or("devkit", |s| s.name));
    devkit_common::paths::migrate_legacy_state();
    // Checked against the raw argv, the same way the probe intercept above
    // is: `Cli::parse()` hasn't run yet, so this can't ask clap which
    // subcommand it resolved to. Over-matching an argument vector that merely
    // contains this string is fine — the cost is one skipped automatic pass,
    // and the next invocation does it. Skipping is what keeps `install-links`
    // able to report `created`/`replaced`: run unconditionally, this pass
    // would already have linked everything, and every outcome `links::run`
    // sees would be `AlreadyLinked`.
    let is_install_links = args
        .iter()
        .skip(1)
        .any(|a| a.as_os_str() == std::ffi::OsStr::new("install-links"));
    if !is_install_links && let Ok(exe) = std::env::current_exe() {
        links::ensure_current(&exe);
    }
    match shim {
        Some(s) => dispatch_shim(s, args),
        None => {
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
                Cmd::Completions { shell, all } => {
                    let scripts = if all {
                        every_completion_script()
                    } else {
                        vec![(Cli::command(), "devkit")]
                    };
                    Ok(completions::emit(shell, scripts)?)
                }
                Cmd::Ports(c) => ports::run(c),
                Cmd::Locks(c) => locks::run(c),
                Cmd::Docs(c) => docs::run(c),
                Cmd::Run(c) => run::run(c),
                Cmd::Issue(c) => issue::run(c),
                Cmd::Mcp(c) => mcp::run(c),
                Cmd::InstallLinks(a) => links::run(a),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `dispatch_shim` matches on `shim::Sub` exhaustively (no catch-all
    /// arm), so a `SHIMS` entry naming a `Sub` variant with no dispatch arm
    /// is a compile error, not a runtime panic — that guarantee needs no
    /// test. What still wants checking is the other half: that each
    /// variant's `name()` actually names a subcommand `Cli` registers, so a
    /// shim never resolves to a subcommand that does not exist.
    #[test]
    fn every_shim_names_a_real_subcommand() {
        for s in shim::SHIMS {
            assert!(
                Cli::command().find_subcommand(s.sub.name()).is_some(),
                "shim `{}` selects unknown subcommand `{}`",
                s.name,
                s.sub.name()
            );
        }
    }

    #[test]
    fn shim_help_names_every_shim() {
        for s in crate::shim::SHIMS {
            assert!(
                SHIM_HELP.contains(s.name),
                "SHIM_HELP never names the `{}` shim",
                s.name
            );
        }
    }
}
