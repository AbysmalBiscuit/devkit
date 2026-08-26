use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use devkit::completions::Shell;
use devkit_ports::registry::{self, Data, Role};

#[derive(Parser)]
#[command(version, about = "Port registry for local dev servers")]
struct Cli {
    /// Run as if portm had started in DIR instead of the current directory.
    #[arg(short = 'C', long = "dir")]
    dir: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show reserved/live ports for this project (every worktree of it).
    Status,
    /// Reserve a port per app for a holder (default: this worktree).
    #[command(visible_alias = "reserve")]
    Alloc {
        /// Worktree root the ports belong to; defaults to the current worktree.
        #[arg(long)]
        holder: Option<String>,
        /// Role the reservation belongs to; issue and baseline get separate ports.
        #[arg(long, value_enum, default_value = "issue")]
        role: Role,
        /// Apps to reserve a port for, one row each.
        apps: Vec<String>,
    },
    /// Release a holder's reservations (default: this worktree), optionally only the named apps.
    Release {
        /// Only these apps; omit to release everything the holder has.
        apps: Vec<String>,
        /// Worktree root whose ports to release; defaults to the current worktree.
        #[arg(long)]
        holder: Option<String>,
        /// Only this role; omit to release both.
        #[arg(long, value_enum)]
        role: Option<Role>,
    },
    /// Drop stale reservations (dead pids, vanished holders).
    Prune,
    /// Print a shell-completion script (bash, zsh, fish, …) to stdout.
    Completions {
        /// Shell to emit the script for.
        shell: Shell,
    },
}

/// The holder is the worktree root path; when `--holder` is omitted, resolve it
/// from the working directory the same way `devrun` does.
fn resolve_holder(explicit: Option<String>, cwd: &str) -> Result<String> {
    if let Some(h) = explicit {
        return Ok(h);
    }
    Ok(devkit_common::git::checkout_root(std::path::Path::new(cwd))
        .context("no --holder given and the current directory is not a git worktree")?
        .to_string_lossy()
        .into_owned())
}

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("portm");
    devkit_common::paths::migrate_legacy_state();
    let cli = Cli::parse();
    let cwd = cli.dir.clone().unwrap_or_else(|| ".".into());
    match cli.cmd.unwrap_or(Cmd::Status) {
        Cmd::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "portm", &mut std::io::stdout());
        }
        Cmd::Status => status()?,
        Cmd::Prune => {
            let freed = registry::prune()?;
            println!("pruned: {freed:?}");
        }
        Cmd::Release { apps, holder, role } => {
            let holder = resolve_holder(holder, &cwd)?;
            let freed = if apps.is_empty() {
                registry::release(&holder, role)?
            } else {
                let data = registry::snapshot()?;
                let ports: Vec<u16> = data
                    .entries
                    .iter()
                    .filter(|(_, e)| {
                        e.holder == holder
                            && apps.contains(&e.app)
                            && role.is_none_or(|r| e.role == r)
                    })
                    .map(|(p, _)| *p)
                    .collect();
                registry::release_ports(&ports)?
            };
            println!("released: {freed:?}");
        }
        Cmd::Alloc { holder, role, apps } => {
            let holder = resolve_holder(holder, &cwd)?;
            let loaded = devkit_ports::load::load(None, std::path::Path::new(&cwd))?;
            let mut reqs = Vec::with_capacity(apps.len());
            for app in &apps {
                let base = loaded
                    .catalog
                    .get(app)
                    .ok_or_else(|| anyhow::anyhow!("unknown app `{app}`"))?
                    .base_port;
                reqs.push((app.clone(), base));
            }
            for (app, port) in registry::alloc(&holder, &reqs, role)? {
                println!("{app}={port}");
            }
        }
    }
    Ok(())
}

fn status() -> Result<()> {
    let data: Data = registry::snapshot()?;
    println!("{}", registry::status_table(&data, None));
    Ok(())
}
