use anyhow::Result;
use clap::{Parser, Subcommand};

mod end;
mod prs;
mod review;
mod setup;
mod status;
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
    /// Remove FINISHED worktrees (PR merged + Linear done + clean).
    End {
        ids: Vec<String>,
        #[arg(short = 'y', long)]
        yes: bool,
        #[arg(long)]
        force: bool,
        #[arg(long = "pr-only")]
        pr_only: bool,
        #[arg(long = "clean-worktree")]
        clean_worktree: bool,
    },
    /// At-a-glance triage of your GitHub PRs via gh.
    Prs {
        #[arg(short = 'm', long)]
        mine: bool,
        #[arg(short = 'r', long)]
        reviews: bool,
        #[arg(short = 'R', long)]
        repo: Option<String>,
        #[arg(long = "no-cache")]
        no_cache: bool,
    },
    /// Push, open/reuse a PR, add a reviewer, and Slack them the body + PR link.
    Review {
        /// Slack message body (PR URL is appended automatically).
        body: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        reviewer: Option<String>,
        #[arg(long)]
        base: Option<String>,
        #[arg(long = "pr-title")]
        pr_title: Option<String>,
        #[arg(long = "pr-body")]
        pr_body: Option<String>,
        #[arg(long = "no-push")]
        no_push: bool,
    },
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
        Some(Cmd::End { ids, yes, force, pr_only, clean_worktree }) =>
            end::run(&start(&cli.dir), &ids, yes, force, pr_only, clean_worktree),
        Some(Cmd::Prs { mine, reviews, repo, no_cache }) => prs::run(mine, reviews, repo, no_cache),
        Some(Cmd::Review { body, to, reviewer, base, pr_title, pr_body, no_push }) =>
            review::run(review::ReviewArgs {
                body, to, reviewer, base, pr_title, pr_body, no_push,
                dir: cli.dir, config: cli.config,
            }),
        None => status::run(&start(&cli.dir), &[]),
    }
}
