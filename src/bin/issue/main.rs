use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;

mod checkout;
mod dashboard;
mod end;
mod gitignore;
mod info;
mod info_cache;
mod prs;
mod record;
mod review;
mod select;
mod setup;
mod spin;
mod status;
mod triage;

#[derive(Parser)]
#[command(
    name = "issue",
    about = "Issue lifecycle: setup, status, info, end, prs, dashboard, review"
)]
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
    /// Prepare an issue worktree: branch, per-app setup commands, reserved ports.
    Setup {
        #[arg(long)]
        issue: String,
        #[arg(long)]
        slug: String,
        #[arg(long, value_delimiter = ',')]
        apps: Vec<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long = "no-gitignore")]
        no_gitignore: bool,
    },
    /// Check out an existing PR (by number, Linear id, or URL) into a new worktree.
    CheckoutPr {
        /// `#3340` | `3340` | `PREFIX-3340` | github PR URL | linear issue URL.
        target: String,
        /// Worktree path; defaults to the config-resolved placement.
        worktree_path: Option<String>,
        #[arg(long)]
        setup: bool,
        #[arg(long, value_delimiter = ',')]
        apps: Vec<String>,
    },
    /// Read-only report of every issue worktree (optionally filtered by ID).
    Status { ids: Vec<String> },
    /// Show one worktree's PR + Linear id (current worktree, or a SELECTOR).
    Info {
        /// Issue id, branch, worktree basename, or path. Defaults to cwd.
        selector: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long = "cache-only")]
        cache_only: bool,
    },
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
    /// Combined at-a-glance view plus issue/PR/commit timelines.
    Dashboard {
        #[arg(long, default_value = "auto")]
        bucket: String,
        #[arg(long, default_value = "bar")]
        chart: String,
        #[arg(long, default_value = "absolute")]
        mode: String,
        #[arg(long = "all-roles")]
        all_roles: bool,
        #[arg(long)]
        author: Option<String>,
        #[arg(long = "no-plots")]
        no_plots: bool,
        #[arg(long = "no-cache")]
        no_cache: bool,
    },
    /// Request or finish a review.
    Review {
        #[command(subcommand)]
        cmd: ReviewCmd,
    },
    /// Print a shell-completion script (bash, zsh, fish, …) to stdout.
    Completions { shell: Shell },
}

#[derive(Subcommand)]
enum ReviewCmd {
    /// Push, open/reuse the PR, request review, and Slack the reviewers.
    Request {
        /// Slack body; fills the `review_request` template's `{{ input }}`.
        body: Option<String>,
        /// Recipient: a `[people]` alias or `#channel`. Repeatable.
        #[arg(long = "to")]
        to: Vec<String>,
        #[arg(long)]
        base: Option<String>,
        #[arg(long = "pr-title")]
        pr_title: Option<String>,
        #[arg(long = "pr-body")]
        pr_body: Option<String>,
        #[arg(long = "no-push")]
        no_push: bool,
        /// Override a declared template variable: `--arg key=value`. Repeatable.
        #[arg(long = "arg")]
        args: Vec<String>,
    },
}

fn start(dir: &Option<String>) -> String {
    dir.clone().unwrap_or_else(|| ".".to_string())
}

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("issue");
    devkit_common::paths::migrate_legacy_state();
    let cli = Cli::parse();
    match cli.cmd {
        Some(Cmd::Setup {
            issue,
            slug,
            apps,
            dry_run,
            no_gitignore,
        }) => setup::run(setup::SetupArgs {
            issue,
            slug,
            apps,
            dry_run,
            no_gitignore,
            dir: cli.dir,
            config: cli.config,
        }),
        Some(Cmd::CheckoutPr {
            target,
            worktree_path,
            setup,
            apps,
        }) => checkout::run(checkout::CheckoutArgs {
            target,
            worktree_path,
            setup,
            apps,
            dir: cli.dir,
            config: cli.config,
        }),
        Some(Cmd::Status { ids }) => status::run(&start(&cli.dir), &ids),
        Some(Cmd::Info {
            selector,
            json,
            cache_only,
        }) => info::run(&start(&cli.dir), selector.as_deref(), json, cache_only),
        Some(Cmd::End {
            ids,
            yes,
            force,
            pr_only,
            clean_worktree,
        }) => end::run(&start(&cli.dir), &ids, yes, force, pr_only, clean_worktree),
        Some(Cmd::Prs {
            mine,
            reviews,
            repo,
            no_cache,
        }) => prs::run(mine, reviews, repo, no_cache, cli.config),
        Some(Cmd::Dashboard {
            bucket,
            chart,
            mode,
            all_roles,
            author,
            no_plots,
            no_cache,
        }) => dashboard::run(dashboard::DashboardArgs {
            bucket,
            chart,
            mode,
            all_roles,
            author,
            no_plots,
            no_cache,
            dir: cli.dir,
            config: cli.config,
        }),
        Some(Cmd::Review { cmd }) => match cmd {
            ReviewCmd::Request {
                body,
                to,
                base,
                pr_title,
                pr_body,
                no_push,
                args,
            } => review::request::run(review::request::Args {
                body,
                to,
                base,
                pr_title,
                pr_body,
                no_push,
                args,
                dir: cli.dir,
                config: cli.config,
            }),
        },
        Some(Cmd::Completions { shell }) => {
            clap_complete::generate(shell, &mut Cli::command(), "issue", &mut std::io::stdout());
            Ok(())
        }
        None => status::run(&start(&cli.dir), &[]),
    }
}
