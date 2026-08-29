use anyhow::Result;
use clap::Subcommand;
use devkit::completions::Shell;
use std::path::PathBuf;

mod checkout;
mod dashboard;
mod end;
mod info;
mod info_cache;
mod prs;
mod review;
mod select;
mod setup;
mod slug;
mod status;
mod summary;
mod sync;
mod tracker;
mod triage;

/// `--timing` verbosity, parsed by clap. `--timing` alone = summary,
/// `--timing=trace` = per-op detail.
#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum TimingFlag {
    Summary,
    Trace,
}

/// Resolve the timing mode: the flag wins; otherwise fall back to `DEVKIT_TIMING`.
fn timing_mode(flag: Option<TimingFlag>) -> devkit_common::timing::Mode {
    use devkit_common::timing::Mode;
    match flag {
        Some(TimingFlag::Summary) => Mode::Summary,
        Some(TimingFlag::Trace) => Mode::Trace,
        None => devkit_common::timing::mode_from_env(),
    }
}

#[derive(clap::Args)]
pub struct IssueCli {
    /// Run as if this command had started in DIR instead of the current directory.
    #[arg(short = 'C', long = "dir", global = true)]
    pub dir: Option<String>,
    /// devkit.toml to load instead of the one discovered from the start directory.
    #[arg(long, global = true)]
    pub config: Option<String>,
    /// Print IO timing to stderr. `--timing` = summary, `--timing=trace` = per-op.
    #[arg(long, global = true, value_name = "MODE", num_args = 0..=1, default_missing_value = "summary")]
    pub timing: Option<TimingFlag>,
    /// Write one JSON record per timed IO op to FILE.
    #[arg(long = "timing-log", global = true, value_name = "FILE")]
    pub timing_log: Option<PathBuf>,
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand)]
pub(crate) enum Cmd {
    /// Prepare an issue worktree: branch, per-app setup commands, reserved ports.
    Setup {
        /// Linear issue id or issue URL (equivalent to --issue).
        #[arg(
            value_name = "ISSUE",
            required_unless_present = "issue",
            conflicts_with = "issue"
        )]
        issue_pos: Option<String>,
        /// Linear issue id or issue URL (equivalent to the positional ISSUE).
        #[arg(long)]
        issue: Option<String>,
        /// Short kebab title, without the issue id, rendered into the branch and
        /// worktree names (e.g. `fix-bli-export`). Omit to take the slug a
        /// pasted issue URL already spells out, else the Linear title.
        #[arg(long)]
        slug: Option<String>,
        /// Apps to bootstrap: writes each one's prep files and runs its setup
        /// commands. Omit for a worktree with no per-app setup.
        #[arg(long, value_delimiter = ',')]
        apps: Vec<String>,
        /// Also write an issue summary file, the Linear facts and
        /// description as a markdown scaffold, at the path
        /// `templates.issue_summary_path` names. Needs a Linear key, and never
        /// overwrites a summary that is already there. Set
        /// `defaults.issue_summary = true` to make this the default.
        #[arg(long)]
        summary: bool,
        /// Skip the issue summary file for this run, whatever
        /// `defaults.issue_summary` says.
        #[arg(long = "no-summary", conflicts_with = "summary")]
        no_summary: bool,
        /// Print the resolved issue, worktree, and branch as JSON without
        /// creating anything.
        #[arg(long)]
        dry_run: bool,
        /// Leave the global gitignore alone instead of adding devkit's
        /// per-worktree artifacts to it.
        #[arg(long = "no-gitignore")]
        no_gitignore: bool,
    },
    /// Check out an existing PR (by number, issue id, or URL) into a new worktree.
    CheckoutPr {
        /// `#3340` | `3340` | `PREFIX-3340` | github PR URL | tracker issue URL.
        target: String,
        /// Worktree path; defaults to the config-resolved placement.
        worktree_path: Option<String>,
        /// Also write each app's prep files and run its setup commands.
        #[arg(long)]
        setup: bool,
        /// Apps to bootstrap under --setup. Omit for a worktree with no per-app
        /// setup.
        #[arg(long, value_delimiter = ',')]
        apps: Vec<String>,
    },
    /// Read-only report of every issue worktree (optionally filtered by ID).
    Status {
        /// Issue ids to report on; omit for every issue worktree.
        ids: Vec<String>,
    },
    /// Show one worktree's PR + issue id (current worktree, or a SELECTOR).
    Info {
        /// Issue id, branch, worktree basename, or path. Defaults to cwd.
        selector: Option<String>,
        /// Emit the worktree as one JSON object instead of a table.
        #[arg(long)]
        json: bool,
        /// Skip the network: take the PR number from the worktree's cache and
        /// leave the issue state blank.
        #[arg(long = "cache-only")]
        cache_only: bool,
    },
    /// Remove FINISHED worktrees (PR merged + issue done + clean).
    End {
        /// Issue ids, branches, or worktree paths to consider; omit to scan
        /// every issue worktree.
        ids: Vec<String>,
        /// Remove without asking for confirmation.
        #[arg(short = 'y', long)]
        yes: bool,
        /// Discard uncommitted changes instead of refusing to remove a dirty
        /// worktree.
        #[arg(long)]
        force: bool,
        /// Count a merged PR plus a clean tree as finished, ignoring the
        /// tracker state and the issue-id gate.
        #[arg(long = "pr-only")]
        pr_only: bool,
        /// Remove the selected worktrees whether or not they are finished.
        /// Requires at least one selector.
        #[arg(long = "clean-worktree")]
        clean_worktree: bool,
    },
    /// Re-copy the `defaults.worktree_include` files from the monorepo into
    /// worktrees that already exist.
    SyncIncludes {
        /// Issue ids, branches, worktree basenames, or paths to sync; omit for
        /// every worktree.
        selectors: Vec<String>,
        /// Replace files the worktree already has instead of leaving them
        /// alone. Asks once per worktree before clobbering anything, and needs
        /// a scope: one or more selectors, or --all.
        #[arg(long)]
        overwrite: bool,
        /// Widen --overwrite to every worktree in the repository, other
        /// sessions' included.
        #[arg(long)]
        all: bool,
        /// Answer the --overwrite prompt yes. Does nothing on its own: without
        /// --overwrite there is no prompt and nothing is replaced.
        #[arg(short = 'y', long)]
        yes: bool,
        /// Report what would be copied without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// At-a-glance triage of your GitHub PRs via gh.
    Prs {
        /// Show only your open PRs. Neither flag prints both sections.
        #[arg(short = 'm', long)]
        mine: bool,
        /// Show only PRs awaiting your review. Neither flag prints both sections.
        #[arg(short = 'r', long)]
        reviews: bool,
        /// owner/repo to triage instead of the current repository.
        #[arg(short = 'R', long)]
        repo: Option<String>,
        /// Refetch from GitHub instead of rendering the last run's cached rows.
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// PRs fetched per GitHub search page. Lower this if GitHub returns
        /// HTTP 504 on a repo with many open PRs.
        #[arg(long, default_value_t = devkit_issue::prs::DEFAULT_BATCH_SIZE, value_parser = clap::value_parser!(u32).range(1..=100))]
        batch_size: u32,
        /// Extra attempts per page after a failed fetch, with backoff.
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u32).range(0..=10))]
        retries: u32,
    },
    /// Combined at-a-glance view plus issue/PR/commit timelines.
    Dashboard {
        /// Timeline bucket width: auto, day, week, or month.
        #[arg(long, default_value = "auto")]
        bucket: String,
        /// Plot style: bar or line.
        #[arg(long, default_value = "bar")]
        chart: String,
        /// Issue-status plot scale: absolute counts or proportional shares.
        #[arg(long, default_value = "absolute")]
        mode: String,
        /// Count PRs you reviewed in the timelines, not only the ones you
        /// authored.
        #[arg(long = "all-roles")]
        all_roles: bool,
        /// Git author to count commits for; defaults to your local git email.
        #[arg(long)]
        author: Option<String>,
        /// Print the tables without the timelines.
        #[arg(long = "no-plots")]
        no_plots: bool,
        /// Refetch from GitHub instead of rendering the last run's cached rows.
        #[arg(long = "no-cache")]
        no_cache: bool,
    },
    /// Request or finish a review.
    Review {
        #[command(subcommand)]
        cmd: ReviewCmd,
    },
    /// Print a shell-completion script (bash, zsh, fish, ...) to stdout.
    Completions {
        /// Shell to emit the script for.
        shell: Shell,
    },
}

#[derive(Subcommand)]
pub(crate) enum ReviewCmd {
    /// Push, open/reuse the PR, request review, and Slack the reviewers.
    Request {
        /// Slack body; fills the `review_request` template's `{{ input }}`.
        body: Option<String>,
        /// Recipient: a `[people]` alias or `#channel`. Repeatable.
        #[arg(long = "to")]
        to: Vec<String>,
        /// PR base branch, instead of the configured baseline ref.
        #[arg(long)]
        base: Option<String>,
        /// PR title, instead of the one the template renders.
        #[arg(long = "pr-title")]
        pr_title: Option<String>,
        /// PR body, instead of the one the template renders.
        #[arg(long = "pr-body")]
        pr_body: Option<String>,
        /// Open or update the PR without pushing the branch first.
        #[arg(long = "no-push")]
        no_push: bool,
        /// Add no reviewer beyond `--to` and send no Slack: never falls back to the
        /// PR's current reviewers.
        #[arg(long = "no-notify")]
        no_notify: bool,
        /// Use this PR for this run: a GitHub PR URL or a bare number (meaning
        /// `pr_repo`). Replaces a wrong recorded binding.
        #[arg(long)]
        pr: Option<String>,
        /// Override a declared template variable: `--arg key=value`. Repeatable.
        #[arg(long = "arg")]
        args: Vec<String>,
    },
    /// Announce over Slack that you finished reviewing; notify the author or --to.
    Finish {
        /// Slack body; fills the `review_finish` template's `{{ input }}`.
        body: Option<String>,
        /// Recipient: a `[people]` alias or `#channel`. Repeatable. Defaults to the PR author.
        #[arg(long = "to")]
        to: Vec<String>,
        /// PR number; required when not run inside the PR's worktree.
        #[arg(long)]
        pr: Option<u64>,
        /// Override a declared template variable: `--arg key=value`. Repeatable.
        #[arg(long = "arg")]
        args: Vec<String>,
    },
}

fn start(dir: &Option<String>) -> String {
    dir.clone().unwrap_or_else(|| ".".to_string())
}

pub fn run(cli: IssueCli) -> Result<()> {
    let _timing = devkit_common::timing::init(timing_mode(cli.timing), cli.timing_log.clone());
    match cli.cmd {
        Some(Cmd::Setup {
            issue_pos,
            issue,
            slug,
            apps,
            summary,
            no_summary,
            dry_run,
            no_gitignore,
        }) => setup::run(setup::SetupArgs {
            // clap guarantees exactly one of the two is present
            issue: issue_pos.or(issue).expect("issue id"),
            slug,
            apps,
            summary,
            no_summary,
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
        Some(Cmd::Status { ids }) => status::run(&start(&cli.dir), &ids, cli.config.as_deref()),
        Some(Cmd::Info {
            selector,
            json,
            cache_only,
        }) => info::run(
            &start(&cli.dir),
            selector.as_deref(),
            json,
            cache_only,
            cli.config.as_deref(),
        ),
        Some(Cmd::End {
            ids,
            yes,
            force,
            pr_only,
            clean_worktree,
        }) => end::run(
            &start(&cli.dir),
            &ids,
            yes,
            force,
            pr_only,
            clean_worktree,
            cli.config.as_deref(),
        ),
        Some(Cmd::SyncIncludes {
            selectors,
            overwrite,
            all,
            yes,
            dry_run,
        }) => sync::run(
            &start(&cli.dir),
            &selectors,
            overwrite,
            all,
            yes,
            dry_run,
            cli.config.as_deref(),
        ),
        Some(Cmd::Prs {
            mine,
            reviews,
            repo,
            no_cache,
            batch_size,
            retries,
        }) => prs::run(
            mine,
            reviews,
            repo,
            no_cache,
            cli.config,
            devkit_issue::prs::Fetch {
                batch_size,
                retries,
            },
        ),
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
                no_notify,
                pr,
                args,
            } => review::request::run(review::request::Args {
                body,
                to,
                base,
                pr_title,
                pr_body,
                no_push,
                no_notify,
                pr,
                args,
                dir: cli.dir,
                config: cli.config,
            }),
            ReviewCmd::Finish { body, to, pr, args } => review::finish::run(review::finish::Args {
                body,
                to,
                pr,
                args,
                dir: cli.dir,
                config: cli.config,
            }),
        },
        Some(Cmd::Completions { shell }) => crate::emit_completions(shell, "issue", "issue"),
        None => status::run(&start(&cli.dir), &[], cli.config.as_deref()),
    }
}
