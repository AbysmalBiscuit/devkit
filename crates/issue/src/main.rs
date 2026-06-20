use anyhow::Result;
use clap::{Parser, Subcommand};

mod setup;
mod status;
// end and dashboard subcommands (later tasks) consume remaining triage items
#[allow(dead_code)]
mod triage;

#[derive(Parser)]
#[command(name = "issue", about = "Issue lifecycle: setup, status, end, prs, dashboard, review")]
struct Cli {
    #[arg(short = 'C', long = "dir", global = true)]
    dir: Option<String>,
    #[arg(long, global = true)]
    config: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Prepare an issue worktree: branch, env symlinks, deps, reserved ports.
    Setup {
        #[arg(long)]
        issue: String,
        #[arg(long)]
        slug: String,
        #[arg(long, value_delimiter = ',')]
        apps: Vec<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Read-only report of every issue worktree (optionally filtered by ID).
    Status { ids: Vec<String> },
}

fn start(dir: &Option<String>) -> String {
    dir.clone().unwrap_or_else(|| ".".to_string())
}

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("issue");
    let cli = Cli::parse();
    match cli.cmd {
        Some(Cmd::Setup { issue, slug, apps, dry_run }) => setup::run(setup::SetupArgs {
            issue, slug, apps, dry_run, dir: cli.dir, config: cli.config,
        }),
        Some(Cmd::Status { ids }) => status::run(&start(&cli.dir), &ids),
        None => status::run(&start(&cli.dir), &[]),
    }
}
