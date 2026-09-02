use anyhow::Result;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use devkit::completions::{self, Shell};
use std::ffi::OsString;
use std::path::PathBuf;

mod auth;
mod baseline;
mod brief;
mod config;
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
    /// Store a Linear or Slack token, or report the GitHub identity.
    ///
    /// Validates the credential before writing it. GitHub stores nothing,
    /// since `gh auth login` or `GH_TOKEN`/`GITHUB_TOKEN` already cover that
    /// credential; for `github` this reports the identity behind whichever
    /// token resolves.
    Auth {
        /// Credential to validate and store, or `github` to report identity.
        provider: Provider,
        /// Provide the token non-interactively instead of being prompted.
        /// Refused for `github`, which stores nothing.
        #[arg(long)]
        token: Option<String>,
    },
    /// Print a project brief for the current checkout.
    ///
    /// Includes apps, tasks, live servers, and library versions; silent
    /// outside a devkit-managed project. Intended for coding-agent session
    /// hooks.
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
    /// Show the resolved config, or list configured apps or tasks.
    #[command(display_name = "devkit config")]
    Config(config::ConfigCli),
    /// Check configured credentials and report what is missing.
    Doctor {
        /// Emit the report as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Print the JSON Schema for `devkit.toml` to stdout.
    ///
    /// Editors that speak the TOML language server use it for completion and
    /// validation; see `docs/configuration.md`.
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
    /// Supervised dev servers and canned project tasks.
    #[command(display_name = "devkit run")]
    Run(run::RunCli),
    /// Set up, track, review and retire issue worktrees
    #[command(display_name = "devkit issue")]
    Issue(issue::IssueCli),
    /// Serve the devkit MCP tools over stdio.
    #[command(display_name = "devkit mcp")]
    Mcp(mcp::McpCli),
    /// Install the old command names as hardlinks beside this binary.
    ///
    /// Creates hardlinks such as `issue` and `devrun` beside this
    /// executable.
    InstallLinks(links::InstallLinksArgs),
}

#[derive(Subcommand)]
enum SchemaCmd {
    /// Point a devkit.toml at the published schema.
    ///
    /// Creates a starter one when it does not exist; leaves a file that
    /// already names a schema alone.
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

/// Answer a help request that the full view owns. Returns `true` when it
/// printed, meaning `main` is done; `false` hands the arguments back to clap
/// untouched, which is what keeps the terse view clap's own rendering.
fn intercept_help(root: &clap::Command, args: &[OsString]) -> Result<bool> {
    let Some(req) = devkit::help::resolve(root, args) else {
        return Ok(false);
    };
    if req.short_help {
        return Ok(false);
    }
    let decision = devkit::help::decide(
        req.full_flag,
        std::env::var(devkit::help::ENV).ok().as_deref(),
        devkit_common::ui::stdout_is_tty(),
    );
    if let Some(warning) = &decision.warning {
        eprintln!("warning: {warning}");
    }
    if decision.verbosity == devkit::help::Verbosity::Terse {
        return Ok(false);
    }

    // Build before walking. `build()` is what assigns each subcommand its
    // `devkit issue status` usage name and copies the parent's `global(true)`
    // arguments down; a subcommand cloned out of an unbuilt tree prints
    // `Usage: status [IDS]...` with no `-C`, `--config` or `--timing`.
    let mut built = root.clone();
    built.build();
    let mut node = built;
    for name in &req.path {
        // clap's synthetic `help` node carries a child per sibling command, so
        // walking into it would print a tree of `devkit help <cmd>` entries.
        // Decline and let clap render its own usage.
        let Some(next) = node.find_subcommand(name).filter(|_| name != "help") else {
            return Ok(false);
        };
        node = next.clone();
    }
    let path = std::iter::once(root.get_name().to_string())
        .chain(req.path.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");

    let mut out = std::io::stdout().lock();
    let printed = if node.get_subcommands().next().is_some() {
        // `build()` added a `help` subcommand at every level; the renderer
        // skips them, so building first costs the tree nothing.
        devkit::help::tree(&node, &path, &mut out)
    } else {
        // A leaf has no tree. Printing its long help directly is also what
        // keeps `--full` away from the real parse, which would reject it.
        node.print_long_help()
    };
    match printed {
        // `devkit --help | head` closes the pipe on us. The reader is done,
        // which is not this command failing; `completions::emit` treats a
        // broken pipe the same way.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(true),
        other => other.map(|()| true).map_err(Into::into),
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
    // After the automatic linking pass on purpose: `docs/install.md` promises
    // that running devkit at all creates the shim hardlinks, and names
    // `devkit --help` as an invocation that does it.
    let root = match shim {
        Some(s) => shim_command(s.sub.name(), s.name),
        None => Cli::command(),
    };
    if intercept_help(&root, &args)? {
        return Ok(());
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
                Cmd::Config(c) => config::run(c),
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

    /// The full-tree help view prints one line per node as `<path>  <about>`,
    /// capped at a hundred columns. An `about` longer than this budget would be
    /// truncated in that view, so the cap is enforced here rather than papered
    /// over at render time. Prose belongs in `long_about`, which a leaf's
    /// `--help` still prints in full.
    #[test]
    fn every_about_fits_the_tree_line() {
        fn walk(cmd: &clap::Command, path: &str, over: &mut Vec<String>) {
            if let Some(about) = cmd.get_about() {
                let text = about.to_string();
                if text.chars().count() > devkit::help::ABOUT_MAX {
                    over.push(format!("{path} ({} chars): {text}", text.chars().count()));
                }
            }
            for sub in cmd.get_subcommands() {
                walk(sub, &format!("{path} {}", sub.get_name()), over);
            }
        }
        let root = Cli::command();
        let mut over = Vec::new();
        for sub in root.get_subcommands() {
            walk(sub, sub.get_name(), &mut over);
        }
        assert!(
            over.is_empty(),
            "about strings over {} chars:\n{}",
            devkit::help::ABOUT_MAX,
            over.join("\n")
        );
    }
}
