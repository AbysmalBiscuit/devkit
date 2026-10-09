use std::{ffi::OsString, path::PathBuf};

use anyhow::Result;
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use devkit::completions::{self, Shell};
use strum::IntoEnumIterator;

mod activity;
mod auth;
mod baseline;
mod brief;
mod commit;
mod config;
mod docs;
mod doctor;
mod harness;
mod hook;
mod hook_log;
mod install;
mod issue;
mod links;
mod locks;
mod mcp;
mod ports;
mod rules;
mod run;
mod schema;
mod secret;
mod shim;
mod template;
mod todo;

const SHIM_HELP: &str = "\
Also installed under their own names:
  ticket      = devkit ticket
  workspace   = devkit workspace
  devrun      = devkit run
  portm       = devkit ports
  lockm       = devkit locks
  docm        = devkit docs
  devrules    = devkit rules
  devkit-mcp  = devkit mcp

Run `devkit install-links` if any of them are missing.";

#[derive(Parser)]
#[command(
    name = "devkit",
    version = devkit::VERSION,
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
    /// Store a token, report the GitHub identity, or sign in to Supabase.
    ///
    /// Linear and Slack tokens are stored. Validates the credential before
    /// writing it. GitHub stores nothing, since `gh auth login` or
    /// `GH_TOKEN`/`GITHUB_TOKEN` already cover that credential; for
    /// `github` this reports the identity behind whichever token resolves.
    /// `supabase` signs in to the project `[rules.supabase]` names, in the
    /// browser through a provider the project enables, by an emailed code
    /// (`--email`), or with the email and password the secrets resolve to
    /// (`--password`), and keeps the session for hooks.
    Auth {
        /// Credential to validate and store, `github` to report identity, or
        /// `supabase` to sign in to the rules API.
        provider: Provider,
        /// Provide the token non-interactively instead of being prompted.
        /// Refused for `github`, which stores nothing, and for `supabase`.
        #[arg(long)]
        token: Option<String>,
        #[command(flatten)]
        supabase: auth::SupabaseLogin,
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
        /// The harness whose hook runs this. The brief then travels inside
        /// that event's JSON answer, which every harness reads context from
        /// except Claude Code at session start, where it prints bare.
        #[arg(long)]
        harness: Option<pabal::AnyHarness>,
    },
    /// Commit named paths or a patch, or reword the last commit.
    ///
    /// The message comes from the `commit_message` template. The working
    /// tree, and changes staged by other sessions, stay as they were, and a
    /// selection that cannot be committed whole is refused.
    #[command(display_name = "devkit commit")]
    Commit(commit::CommitCli),
    /// Run a repository hook during `devkit commit` and check its commit.
    #[command(name = devkit_common::vcs::COMMIT_HOOK_VERB, hide = true)]
    CommitHook {
        state: PathBuf,
        hook: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
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
    /// Create, edit and render tracker tickets.
    #[command(display_name = "devkit ticket")]
    Ticket(issue::TicketCli),
    /// Set up, report on and retire branch worktrees.
    #[command(display_name = "devkit workspace")]
    Workspace(issue::WorkspaceCli),
    /// Open, ready, list and review this branch's pull request.
    #[command(display_name = "devkit pr")]
    Pr(issue::PrCli),
    /// Serve the devkit MCP tools over stdio.
    #[command(display_name = "devkit mcp")]
    Mcp(mcp::McpCli),
    /// Coding-agent harness hooks.
    #[command(display_name = "devkit harness")]
    Harness(harness::HarnessCli),
    /// Evaluate and record a coding-agent hook event.
    #[command(display_name = "devkit hook")]
    Hook(hook::HookCli),
    /// Read and sweep the harness log.
    #[command(display_name = "devkit hook-log")]
    HookLog(hook_log::HookLogCli),
    /// Query and summarize the rule index the hooks inject from.
    #[command(display_name = "devkit rules")]
    Rules(rules::RulesCli),
    /// List, show and render templates for the caller to deliver itself.
    #[command(display_name = "devkit template")]
    Template(template::TemplateCli),
    /// Todo lists shared by agents and people across sessions.
    #[command(display_name = "devkit todo")]
    Todo(todo::TodoCli),
    /// Report subagent runs and how long todos were held.
    ///
    /// Groups the runs by session and agent type, and the held time by todo.
    /// A run with no agent type reports as `subagent`.
    Activity(activity::ActivityCli),
    /// Set up this machine for devkit; safe to rerun.
    ///
    /// Links the old command names as `install-links` does, and appends each
    /// of devkit's ignore patterns (`.devkit/`, `*.local`, `*.local.*`) that
    /// git's global excludes file lacks, leaving its other lines as they are.
    Install(links::InstallLinksArgs),
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
    Supabase,
}

impl Provider {
    fn label(self) -> &'static str {
        match self {
            Provider::Linear => "Linear",
            Provider::Slack => "Slack",
            Provider::Github => "GitHub",
            Provider::Supabase => "Supabase",
        }
    }
}

/// The name `devkit issue` runs under. `issue` is no `Cli` subcommand, so no
/// help view or completion script offers it; `main` hands `devkit issue ...`
/// to the `issue` alias under this name instead.
const DEVKIT_ISSUE: &str = "devkit issue";

/// Build a tool's `Command` as a root command under `shim_name`, so an
/// installed hardlink of that name reports its own name and version instead of
/// `devkit`'s.
pub(crate) fn shim_command(subcommand: &str, shim_name: &'static str) -> clap::Command {
    let cmd = if subcommand == shim::Shim::Issue.subcommand() {
        issue::IssueCli::augment_args(clap::Command::new(shim_name))
            .about("The commands `ticket`, `workspace` and `pr` replace")
            .propagate_version(true)
    } else {
        Cli::command()
            .find_subcommand(subcommand)
            .unwrap_or_else(|| panic!("no `{subcommand}` subcommand"))
            .clone()
    };
    cmd.name(shim_name)
        // Overrides the `devkit <sub>` spelling the subcommand carries for its
        // own version line: under this name it *is* the root command.
        .display_name(shim_name)
        .bin_name(shim_name)
        .version(devkit::VERSION)
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
    scripts.extend(
        shim::Shim::iter()
            .filter(|s| !s.is_alias())
            .filter_map(|s| {
                let cmd = shim_command(s.subcommand(), s.name());
                cmd.find_subcommand("completions")?;
                Some((cmd, s.name()))
            }),
    );
    scripts
}

/// The shim an argument vector runs as, the name it runs under, and the
/// arguments to parse: `argv[0]`'s shim, else `issue` for `devkit issue ...`
/// and `devkit help issue ...` with `devkit issue` as its one root, else plain
/// `devkit`.
fn route(args: Vec<OsString>) -> (Option<shim::Shim>, &'static str, Vec<OsString>) {
    let argv0 = args.first().map(|a| a.to_string_lossy().into_owned());
    if let Some(s) = argv0.as_deref().and_then(shim::Shim::from_argv0) {
        return (Some(s), s.name(), args);
    }
    let word = |i: usize| args.get(i).map(OsString::as_os_str);
    let issue = Some(std::ffi::OsStr::new("issue"));
    let (help, skip) = if word(1) == issue {
        (false, 2)
    } else if word(1) == Some(std::ffi::OsStr::new("help")) && word(2) == issue {
        (true, 3)
    } else {
        return (None, "devkit", args);
    };
    let root = std::iter::once(OsString::from(DEVKIT_ISSUE));
    let help = help.then(|| OsString::from("help"));
    let args = root
        .chain(help)
        .chain(args.into_iter().skip(skip))
        .collect();
    (Some(shim::Shim::Issue), DEVKIT_ISSUE, args)
}

fn dispatch_shim(s: shim::Shim, name: &'static str, args: Vec<OsString>) -> Result<()> {
    let matches = shim_command(s.subcommand(), name).get_matches_from(args);
    match s {
        shim::Shim::Ports => ports::run(ports::PortsCli::from_arg_matches(&matches)?),
        shim::Shim::Locks => locks::run(locks::LocksCli::from_arg_matches(&matches)?),
        shim::Shim::Docs => docs::run(docs::DocsCli::from_arg_matches(&matches)?),
        shim::Shim::Run => run::run(run::RunCli::from_arg_matches(&matches)?),
        shim::Shim::Ticket => issue::run(
            issue::TicketCli::from_arg_matches(&matches)?.action(),
            s.name(),
        ),
        shim::Shim::Workspace => issue::run(
            issue::WorkspaceCli::from_arg_matches(&matches)?.action(),
            s.name(),
        ),
        shim::Shim::Issue => issue::run(
            issue::IssueCli::from_arg_matches(&matches)?.action(),
            s.name(),
        ),
        shim::Shim::Mcp => mcp::run(mcp::McpCli::from_arg_matches(&matches)?),
        shim::Shim::Rules => rules::run(rules::RulesCli::from_arg_matches(&matches)?),
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

/// Whether this argument vector is addressing the `hook` family, read off the
/// raw argv because clap has not resolved a subcommand yet — and, on a parse
/// error, never will.
fn is_hook_invocation(args: &[OsString]) -> bool {
    args.get(1).map(OsString::as_os_str) == Some(std::ffi::OsStr::new("hook"))
}

/// Parse, with the `hook` family's exit-code rule applied to a failure.
///
/// clap exits 2 for a usage error, and exit 2 blocks the tool call on Claude
/// Code `PreToolUse` and sets `should_block` on Codex. So an unrecognised verb,
/// an unrecognised `--harness` or a missing argument would deny every command
/// the agent ran. The error still reaches stderr and the exit is still
/// non-zero: what changes is that it no longer reads as a deny.
///
/// `use_stderr` is the discriminator rather than the error kind, because
/// `--help` and `--version` arrive here as errors too and are not failures.
fn parse_cli(args: &[OsString]) -> Cli {
    match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) if e.use_stderr() && is_hook_invocation(args) => {
            let _ = e.print();
            std::process::exit(1);
        }
        Err(e) => e.exit(),
    }
}

/// Run a hook verb with a panic caught. An escaping panic exits 101, which
/// aborts a Claude Code `WorktreeCreate` and reads as a failure on every other
/// verb; the panic hook has already reported it by the time this is reached.
fn run_hook_guarded(cli: hook::HookCli) -> Result<()> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hook::run(cli))) {
        Ok(r) => r,
        Err(_) => std::process::exit(1),
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
    let (shim, name, args) = route(args);
    devkit_common::report::install_panic_hook(name);
    devkit_common::paths::migrate_legacy_state();
    // Checked against the raw argv, the same way the probe intercept above
    // is: `Cli::parse()` hasn't run yet, so this can't ask clap which
    // subcommand it resolved to. Over-matching an argument vector that merely
    // contains this string, or names `install` first, is fine: the cost is
    // one skipped automatic pass, and the next invocation does it. Skipping is
    // what keeps `install-links` and `install` able to report
    // `created`/`replaced`: run unconditionally, this pass would already
    // have linked everything, and every outcome `links::run` sees would be
    // `AlreadyLinked`.
    let links_explicitly = args
        .iter()
        .skip(1)
        .any(|a| a.as_os_str() == std::ffi::OsStr::new("install-links"))
        || args.get(1).map(OsString::as_os_str) == Some(std::ffi::OsStr::new("install"));
    if !links_explicitly && let Ok(exe) = std::env::current_exe() {
        links::ensure_current(&exe);
    }
    // After the automatic linking pass on purpose: `docs/install.md` promises
    // that running devkit at all creates the shim hardlinks, and names
    // `devkit --help` as an invocation that does it.
    let root = match shim {
        Some(s) => shim_command(s.subcommand(), name),
        None => Cli::command(),
    };
    if intercept_help(&root, &args)? {
        return Ok(());
    }
    match shim {
        Some(s) => dispatch_shim(s, name, args),
        None => {
            let cli = parse_cli(&args);
            match cli.cmd {
                Cmd::Auth {
                    provider,
                    token,
                    supabase,
                } => auth::run(provider, token, supabase),
                Cmd::Brief {
                    pins_only,
                    if_changed,
                    harness,
                } => brief::run(pins_only, if_changed, harness),
                Cmd::Schema { cmd } => match cmd {
                    None => schema::run(),
                    Some(SchemaCmd::Init { path }) => schema::init(&path),
                },
                Cmd::Commit(c) => commit::run(c),
                Cmd::CommitHook { state, hook, args } => commit::run_hook(&state, &hook, &args),
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
                Cmd::Ticket(c) => issue::run(c.action(), "ticket"),
                Cmd::Workspace(c) => issue::run(c.action(), "workspace"),
                Cmd::Pr(c) => issue::run(c.action(), "pr"),
                Cmd::Mcp(c) => mcp::run(c),
                Cmd::Harness(c) => harness::run(c),
                Cmd::Hook(c) => run_hook_guarded(c),
                Cmd::HookLog(c) => hook_log::run(c),
                Cmd::Rules(c) => rules::run(c),
                Cmd::Template(c) => template::run(c),
                Cmd::Todo(c) => todo::run(c),
                Cmd::Activity(c) => activity::run(c),
                Cmd::Install(a) => install::run(a),
                Cmd::InstallLinks(a) => links::run(a),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `dispatch_shim` matches on `shim::Shim` exhaustively, so a variant
    /// with no dispatch arm is a compile error rather than a runtime panic
    /// and needs no test. The other half does: that each variant's
    /// `subcommand()` names one `Cli` registers, so a shim never resolves to
    /// a subcommand that does not exist. An alias is the exception, built by
    /// `shim_command` on its own so that `Cli` never offers it.
    #[test]
    fn every_shim_names_a_real_subcommand() {
        for s in shim::Shim::iter().filter(|s| !s.is_alias()) {
            assert!(
                Cli::command().find_subcommand(s.subcommand()).is_some(),
                "shim `{}` selects unknown subcommand `{}`",
                s.name(),
                s.subcommand()
            );
        }
    }

    #[test]
    fn shim_help_names_every_shim_but_the_aliases() {
        for s in shim::Shim::iter() {
            assert_eq!(
                SHIM_HELP.contains(&format!("  {} ", s.name())),
                !s.is_alias(),
                "SHIM_HELP should name the `{}` shim exactly when it is no alias",
                s.name()
            );
        }
    }

    /// Which verb each spelling of the `ticket`, `workspace`, `pr` and
    /// `issue` commands parses to, through the same parse and the same
    /// `action()` dispatch uses.
    mod spellings {
        use super::*;
        use crate::issue::{Action, Globals, IssueCli, TicketCli, WorkspaceCli};

        fn action(argv: &[&str]) -> (Globals, Action) {
            if let (Some(s), name, args) = route(argv.iter().map(OsString::from).collect()) {
                let m = shim_command(s.subcommand(), name)
                    .try_get_matches_from(args)
                    .unwrap_or_else(|e| panic!("`{}` did not parse: {e}", argv.join(" ")));
                return match s {
                    shim::Shim::Ticket => TicketCli::from_arg_matches(&m).unwrap().action(),
                    shim::Shim::Workspace => WorkspaceCli::from_arg_matches(&m).unwrap().action(),
                    shim::Shim::Issue => IssueCli::from_arg_matches(&m).unwrap().action(),
                    other => panic!("`{}` is not an issue-family shim", other.name()),
                };
            }
            let cli = Cli::try_parse_from(argv)
                .unwrap_or_else(|e| panic!("`{}` did not parse: {e}", argv.join(" ")));
            match cli.cmd {
                Cmd::Ticket(c) => c.action(),
                Cmd::Workspace(c) => c.action(),
                Cmd::Pr(c) => c.action(),
                _ => panic!("`{}` is not an issue-family command", argv.join(" ")),
            }
        }

        /// Every spelling in `spellings` parses to one action, and `name`
        /// says which verb that is.
        fn same_verb(name: &str, spellings: &[Vec<&str>]) -> Action {
            let first = action(&spellings[0]);
            for argv in &spellings[1..] {
                assert_eq!(
                    action(argv),
                    first,
                    "{name}: `{}` and `{}` disagree",
                    argv.join(" "),
                    spellings[0].join(" ")
                );
            }
            assert_ne!(first.0, Globals::default(), "{name}: the -C flag was lost");
            first.1
        }

        /// `argv` under `devkit <old...>`, `issue <old...>`, `devkit <new...>`
        /// and, for `ticket` and `workspace`, under that link's own name, each
        /// with `-C dir` in front of the verb's own arguments.
        fn spellings<'a>(old: &[&'a str], new: &[&'a str], args: &[&'a str]) -> Vec<Vec<&'a str>> {
            let mut out = Vec::new();
            let with = |prefix: &[&'a str], path: &[&'a str]| {
                let mut v = prefix.to_vec();
                v.extend_from_slice(path);
                v.extend_from_slice(&["-C", "dir"]);
                v.extend_from_slice(args);
                v
            };
            out.push(with(&["devkit", "issue"], old));
            out.push(with(&["issue"], old));
            out.push(with(&["devkit"], new));
            if matches!(new[0], "ticket" | "workspace") {
                out.push(with(&[new[0]], &new[1..]));
            }
            if new[0] == "pr" {
                out.push(with(&["devkit", "ticket"], new));
                out.push(with(&["ticket"], new));
            }
            out
        }

        fn verb(old: &[&str], new: &[&str], args: &[&str]) -> Action {
            same_verb(&new.join(" "), &spellings(old, new, args))
        }

        #[test]
        fn ticket_verbs() {
            use crate::issue::TicketCmd as T;
            let ticket = |a| match a {
                Action::Run(crate::issue::Verb::Ticket(t)) => t,
                other => panic!("not a ticket verb: {other:?}"),
            };
            assert!(matches!(
                ticket(verb(&["create"], &["ticket", "create"], &["--title", "t"])),
                T::Create { .. }
            ));
            assert!(matches!(
                ticket(verb(&["edit"], &["ticket", "edit"], &["7", "--body", "b"])),
                T::Edit { .. }
            ));
            assert!(matches!(
                ticket(verb(&["render"], &["ticket", "render"], &["--title", "t"])),
                T::Render { .. }
            ));
            assert!(matches!(
                ticket(verb(&["event"], &["ticket", "event"], &["start"])),
                T::Event { .. }
            ));
            assert!(matches!(
                ticket(verb(&["dashboard"], &["ticket", "dashboard"], &[
                    "--no-plots"
                ])),
                T::Dashboard { .. }
            ));
        }

        #[test]
        fn workspace_verbs() {
            use crate::issue::WorkspaceCmd as W;
            let workspace = |a| match a {
                Action::Run(crate::issue::Verb::Workspace(w)) => w,
                other => panic!("not a workspace verb: {other:?}"),
            };
            assert!(matches!(
                workspace(verb(&["setup"], &["workspace", "setup"], &[
                    "7",
                    "--dry-run"
                ])),
                W::Setup { .. }
            ));
            assert!(matches!(
                workspace(verb(&["status"], &["workspace", "status"], &["7"])),
                W::Status { .. }
            ));
            assert!(matches!(
                workspace(verb(&["end"], &["workspace", "end"], &["7", "--yes"])),
                W::End { .. }
            ));
            assert!(matches!(
                workspace(verb(
                    &["sync-includes"],
                    &["workspace", "sync-includes"],
                    &["--dry-run"]
                )),
                W::SyncIncludes { .. }
            ));
        }

        #[test]
        fn pr_verbs() {
            use crate::issue::{PrCmd as P, ReviewCmd as R};
            let pr = |a| match a {
                Action::Run(crate::issue::Verb::Pr(p)) => p,
                other => panic!("not a pr verb: {other:?}"),
            };
            assert!(matches!(
                pr(verb(&["pr", "create"], &["pr", "create"], &["--draft"])),
                P::Create { .. }
            ));
            assert!(matches!(
                pr(verb(&["pr", "render"], &["pr", "render"], &["-t", "t"])),
                P::Render { .. }
            ));
            assert!(matches!(
                pr(verb(&["pr", "ready"], &["pr", "ready"], &["--no-push"])),
                P::Ready { .. }
            ));
            assert!(matches!(
                pr(verb(&["pr", "status"], &["pr", "status"], &["--json"])),
                P::Status(_)
            ));
            assert!(matches!(
                pr(verb(&["pr", "checkout"], &["pr", "checkout"], &["12"])),
                P::Checkout(_)
            ));
            assert!(matches!(
                pr(verb(&["prs"], &["pr", "list"], &["--mine"])),
                P::List(_)
            ));
            assert!(matches!(
                pr(verb(
                    &["review", "request"],
                    &["pr", "review", "request"],
                    &["--no-notify"]
                )),
                P::Review {
                    cmd: R::Request { .. }
                }
            ));
            assert!(matches!(
                pr(verb(&["review", "finish"], &["pr", "review", "finish"], &[
                    "--pr", "3"
                ])),
                P::Review {
                    cmd: R::Finish { .. }
                }
            ));
        }

        /// The hidden `issue info` and `issue checkout-pr` reach `pr status`
        /// and `pr checkout`.
        #[test]
        fn hidden_old_aliases() {
            same_verb("pr status", &[
                vec!["devkit", "pr", "status", "-C", "dir", "x"],
                vec!["devkit", "issue", "info", "-C", "dir", "x"],
                vec!["issue", "info", "-C", "dir", "x"],
            ]);
            same_verb("pr checkout", &[
                vec!["devkit", "pr", "checkout", "-C", "dir", "12"],
                vec!["devkit", "issue", "checkout-pr", "-C", "dir", "12"],
                vec!["issue", "checkout-pr", "-C", "dir", "12"],
            ]);
        }

        /// A group named with no verb runs its default: `pr status` for every
        /// `pr`, and `workspace status` for `workspace` and `issue`.
        #[test]
        fn bare_groups() {
            same_verb("bare pr", &[
                vec!["devkit", "pr", "status", "-C", "dir"],
                vec!["devkit", "pr", "-C", "dir"],
                vec!["devkit", "ticket", "pr", "-C", "dir"],
                vec!["ticket", "pr", "-C", "dir"],
                vec!["devkit", "issue", "pr", "-C", "dir"],
                vec!["issue", "pr", "-C", "dir"],
            ]);
            same_verb("bare workspace", &[
                vec!["devkit", "workspace", "status", "-C", "dir"],
                vec!["devkit", "workspace", "-C", "dir"],
                vec!["workspace", "-C", "dir"],
                vec!["devkit", "issue", "-C", "dir"],
                vec!["issue", "-C", "dir"],
            ]);
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
