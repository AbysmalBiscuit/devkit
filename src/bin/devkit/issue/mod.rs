use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use devkit::completions::Shell;

use crate::template::VarArgs;

pub(crate) mod checkout;
mod create;
mod dashboard;
mod edit;
mod end;
mod event;
mod hooks;
mod info;
mod info_cache;
pub(crate) mod pr;
mod preserve;
mod prs;
pub(crate) mod receipt;
pub(crate) mod render;
mod review;
mod select;
pub(crate) mod setup;
mod slug;
mod status;
mod summary;
mod sync;
mod triage;

/// `--timing` verbosity, parsed by clap. `--timing` alone = summary,
/// `--timing=trace` = per-op detail.
#[derive(Clone, Copy, Debug, PartialEq, clap::ValueEnum)]
pub(crate) enum TimingFlag {
    Summary,
    Trace,
}

/// Resolve the timing mode: the flag wins; otherwise fall back to
/// `DEVKIT_TIMING`.
fn timing_mode(flag: Option<TimingFlag>) -> devkit_timing::Mode {
    use devkit_timing::Mode;
    match flag {
        Some(TimingFlag::Summary) => Mode::Summary,
        Some(TimingFlag::Trace) => Mode::Trace,
        None => devkit_timing::mode_from_env(),
    }
}

/// The flags every `ticket`, `workspace`, `pr` and `issue` verb takes.
#[derive(clap::Args, Debug, Default, PartialEq)]
pub struct Globals {
    /// Run as if this command had started in DIR instead of the current
    /// directory.
    #[arg(short = 'C', long = "dir", global = true)]
    pub dir: Option<String>,
    /// devkit.toml to load instead of the one discovered from the start
    /// directory.
    #[arg(long, global = true)]
    pub config: Option<String>,
    /// Print IO timing to stderr. `--timing` = summary, `--timing=trace` =
    /// per-op.
    #[arg(long, global = true, value_name = "MODE", num_args = 0..=1, default_missing_value = "summary")]
    pub timing: Option<TimingFlag>,
    /// Write one JSON record per timed IO op to FILE.
    #[arg(long = "timing-log", global = true, value_name = "FILE")]
    pub timing_log: Option<PathBuf>,
}

/// `devkit ticket`: the verbs that act on a tracker item.
#[derive(clap::Args)]
pub struct TicketCli {
    #[command(flatten)]
    pub globals: Globals,
    #[command(subcommand)]
    cmd: TicketTop,
}

#[derive(Subcommand)]
enum TicketTop {
    #[command(flatten)]
    Verb(TicketCmd),
    /// Pull-request lifecycle for this worktree, as `devkit pr`.
    #[command(hide = true)]
    Pr(PrGroup),
    /// Print a shell-completion script (bash, zsh, fish, ...) to stdout.
    Completions {
        /// Shell to emit the script for.
        shell: Shell,
    },
}

/// `devkit workspace`: the verbs that act on one branch's worktree.
#[derive(clap::Args)]
pub struct WorkspaceCli {
    #[command(flatten)]
    pub globals: Globals,
    #[command(subcommand)]
    cmd: Option<WorkspaceTop>,
}

#[derive(Subcommand)]
enum WorkspaceTop {
    #[command(flatten)]
    Verb(WorkspaceCmd),
    /// Print a shell-completion script (bash, zsh, fish, ...) to stdout.
    Completions {
        /// Shell to emit the script for.
        shell: Shell,
    },
}

/// `devkit pr`: the verbs that act on a branch's pull request.
#[derive(clap::Args)]
pub struct PrCli {
    #[command(flatten)]
    pub globals: Globals,
    #[command(subcommand)]
    cmd: Option<PrCmd>,
}

/// `issue`: the command tree `ticket`, `workspace` and `pr` replace, kept so
/// its old spellings keep working.
#[derive(clap::Args)]
pub struct IssueCli {
    #[command(flatten)]
    pub globals: Globals,
    #[command(subcommand)]
    cmd: Option<IssueCmd>,
}

#[derive(Subcommand)]
enum IssueCmd {
    #[command(flatten)]
    Ticket(TicketCmd),
    #[command(flatten)]
    Workspace(WorkspaceCmd),
    /// Pull-request lifecycle for this worktree.
    Pr(PrGroup),
    /// Check out an existing PR into a new worktree, as `pr checkout`.
    #[command(hide = true)]
    CheckoutPr(PrCheckout),
    /// Show one worktree's PR and issue id, as `pr status`.
    #[command(hide = true)]
    Info(PrStatus),
    /// At-a-glance triage of your open PRs, as `pr list`.
    Prs(PrList),
    /// Request or finish a review, as `pr review`.
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

/// A `pr` group nested under `ticket` or `issue`; bare, it runs `pr status`.
#[derive(clap::Args, Debug, PartialEq)]
struct PrGroup {
    #[command(subcommand)]
    cmd: Option<PrCmd>,
}

/// What a parsed command line asks for: one verb, or a completion script.
#[derive(Debug, PartialEq)]
pub(crate) enum Action {
    Run(Verb),
    Completions(Shell),
}

/// Every verb the `ticket`, `workspace` and `pr` commands carry. Each
/// spelling of a verb, old or new, parses to the same `Verb`, and `run` is
/// the one place a `Verb` is carried out.
#[derive(Debug, PartialEq)]
pub(crate) enum Verb {
    Ticket(TicketCmd),
    Workspace(WorkspaceCmd),
    Pr(PrCmd),
}

fn pr_or_status(cmd: Option<PrCmd>) -> Verb {
    Verb::Pr(cmd.unwrap_or_else(|| PrCmd::Status(PrStatus::default())))
}

fn workspace_status() -> Verb {
    Verb::Workspace(WorkspaceCmd::Status { ids: Vec::new() })
}

impl TicketCli {
    pub(crate) fn action(self) -> (Globals, Action) {
        let action = match self.cmd {
            TicketTop::Verb(cmd) => Action::Run(Verb::Ticket(cmd)),
            TicketTop::Pr(group) => Action::Run(pr_or_status(group.cmd)),
            TicketTop::Completions { shell } => Action::Completions(shell),
        };
        (self.globals, action)
    }
}

impl WorkspaceCli {
    pub(crate) fn action(self) -> (Globals, Action) {
        let action = match self.cmd {
            Some(WorkspaceTop::Verb(cmd)) => Action::Run(Verb::Workspace(cmd)),
            Some(WorkspaceTop::Completions { shell }) => Action::Completions(shell),
            None => Action::Run(workspace_status()),
        };
        (self.globals, action)
    }
}

impl PrCli {
    pub(crate) fn action(self) -> (Globals, Action) {
        (self.globals, Action::Run(pr_or_status(self.cmd)))
    }
}

impl IssueCli {
    pub(crate) fn action(self) -> (Globals, Action) {
        let verb = match self.cmd {
            Some(IssueCmd::Ticket(cmd)) => Verb::Ticket(cmd),
            Some(IssueCmd::Workspace(cmd)) => Verb::Workspace(cmd),
            Some(IssueCmd::Pr(group)) => pr_or_status(group.cmd),
            Some(IssueCmd::CheckoutPr(args)) => Verb::Pr(PrCmd::Checkout(args)),
            Some(IssueCmd::Info(args)) => Verb::Pr(PrCmd::Status(args)),
            Some(IssueCmd::Prs(args)) => Verb::Pr(PrCmd::List(args)),
            Some(IssueCmd::Review { cmd }) => Verb::Pr(PrCmd::Review { cmd }),
            Some(IssueCmd::Completions { shell }) => {
                return (self.globals, Action::Completions(shell));
            }
            None => workspace_status(),
        };
        (self.globals, Action::Run(verb))
    }
}

#[derive(Subcommand, Debug, PartialEq)]
pub(crate) enum TicketCmd {
    /// Render an issue title and body for a tracker MCP call.
    ///
    /// Uses the `ticket_title` and `ticket_body` templates. Prints
    /// {"title": ..., "body": ...}. Pass both unchanged to the tracker's MCP
    /// tool: inside an agent session this records a receipt, and the
    /// pre-tool-use hook denies an issue write whose text has none.
    Render {
        /// Issue title, the `input` of the `ticket_title` template.
        #[arg(long)]
        title: String,
        /// Issue body, the `input` of the `ticket_body` template.
        #[arg(long)]
        body: Option<String>,
        #[command(flatten)]
        vars: VarArgs,
    },
    /// Create a GitHub issue from the issue templates.
    Create {
        /// Issue title, the `input` of the `ticket_title` template.
        #[arg(long)]
        title: String,
        /// Issue body, the `input` of the `ticket_body` template.
        #[arg(long)]
        body: Option<String>,
        #[command(flatten)]
        vars: VarArgs,
    },
    /// Rewrite a GitHub issue's body (and title) from the issue templates.
    ///
    /// Without `--title` the issue keeps its current title, which the
    /// `ticket_body` template reads as `ticket_title` and `issue_title`.
    Edit {
        /// Issue number or issue URL.
        issue: String,
        /// New issue title, the `input` of the `ticket_title` template.
        #[arg(long)]
        title: Option<String>,
        /// New issue body, the `input` of the `ticket_body` template.
        #[arg(long)]
        body: String,
        #[command(flatten)]
        vars: VarArgs,
    },
    /// Move the issue's tracker status as [ticket.events] configures EVENT.
    ///
    /// Reads the status and moves it to the event's `to` when it is in the
    /// event's `from`. setup, start and pr_open already fire on their own;
    /// this reruns one, to retry a failed move or see its error.
    Event {
        /// The event whose transition to apply.
        event: event::EventArg,
        /// Issue id or issue URL. Defaults to this worktree's issue.
        issue: Option<String>,
        /// Also append the outcome, or the error, to FILE with a timestamp.
        /// The background run the SessionStart hook spawns writes here.
        #[arg(long, value_name = "FILE", hide = true)]
        log_file: Option<PathBuf>,
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
        /// Fold each timeline cumulatively or per period. Unset, the issue
        /// chart is cumulative and the commit and PR charts are per period.
        #[arg(long, value_enum)]
        aggregate: Option<dashboard::Aggregate>,
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
        /// Refetch from the forge instead of rendering the last run's cached
        /// rows.
        #[arg(long = "no-cache")]
        no_cache: bool,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub(crate) enum WorkspaceCmd {
    /// Prepare an issue worktree: branch, setup commands, ports.
    Setup {
        /// Issue id or issue URL (equivalent to --issue). Omit it, and pass
        /// --slug, for work that has no tracker issue.
        #[arg(value_name = "ISSUE", conflicts_with = "issue", group = "issue_ref")]
        issue_pos: Option<String>,
        /// Issue id or issue URL (equivalent to the positional ISSUE).
        #[arg(long, group = "issue_ref")]
        issue: Option<String>,
        /// Short kebab title, without the issue id, rendered into the branch
        /// and worktree names (e.g. `fix-bli-export`). Omit to take the
        /// slug a pasted issue URL already spells out, else the tracker's
        /// title. Required when no issue is given.
        #[arg(
            short = 'l',
            long,
            required_unless_present_any = ["issue_pos", "issue"]
        )]
        slug: Option<String>,
        /// Apps to bootstrap: writes each one's prep files and runs its setup
        /// commands. Omit for a worktree with no per-app setup.
        #[arg(long, value_delimiter = ',')]
        apps: Vec<String>,
        /// Also write an issue summary file at the path
        /// `templates.issue_summary_path` names: the tracker's own summary
        /// (a GitHub issue's body) verbatim, else the tracker facts and
        /// description as a markdown scaffold. Needs the tracker's credential,
        /// and never overwrites a summary that is already there. Set
        /// `defaults.issue_summary = true` to make this the default. With
        /// --dry-run, writes nothing and prints the text the file would get
        /// as `summary_text` instead.
        #[arg(short = 's', long)]
        summary: bool,
        /// Skip the issue summary file for this run, whatever
        /// `defaults.issue_summary` says.
        #[arg(long = "no-summary", conflicts_with = "summary")]
        no_summary: bool,
        /// Print the resolved issue, worktree, and branch as JSON without
        /// creating anything. With --summary, also the summary file's path
        /// and its text as `summary_text`.
        #[arg(long)]
        dry_run: bool,
        /// Leave the global gitignore alone instead of adding devkit's
        /// per-worktree artifacts and `*.local` config layers to it.
        #[arg(long = "no-gitignore")]
        no_gitignore: bool,
        /// Bind the checkout this runs in to the issue instead of creating a
        /// worktree: write its issue record (and summary) on the branch
        /// already checked out, creating no branch and running no worktree
        /// hooks. Refuses on the default branch. For a session that works
        /// in one checkout on a branch it was handed.
        #[arg(long, requires = "issue_ref", conflicts_with = "apps")]
        here: bool,
    },
    /// Read-only report of every issue worktree (optionally filtered by ID).
    Status {
        /// Issue ids to report on; omit for every issue worktree.
        ids: Vec<String>,
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
        #[arg(short = 'f', long)]
        force: bool,
        /// Count a merged PR plus a clean tree as finished, ignoring the
        /// tracker state and the issue-id gate.
        #[arg(short = 'p', long = "pr-only")]
        pr_only: bool,
        /// Remove the selected worktrees whether or not they are finished.
        /// Requires at least one selector.
        #[arg(short = 'w', long = "clean-worktree")]
        clean_worktree: bool,
        /// Remove without copying out the `[preserve]` entries first.
        #[arg(long = "no-preserve")]
        no_preserve: bool,
    },
    /// Re-copy worktree_include files into existing worktrees.
    ///
    /// Copies `defaults.worktree_include` files from the primary checkout.
    SyncIncludes {
        /// Issue ids, branches, worktree basenames, or paths to sync; omit for
        /// every worktree.
        selectors: Vec<String>,
        /// Replace files the worktree already has instead of leaving them
        /// alone. Asks once per worktree before clobbering anything, and needs
        /// a scope: one or more selectors, or --all.
        #[arg(short = 'o', long)]
        overwrite: bool,
        /// Widen --overwrite to every worktree in the repository, other
        /// sessions' included.
        #[arg(short = 'a', long)]
        all: bool,
        /// Answer the --overwrite prompt yes. Does nothing on its own: without
        /// --overwrite there is no prompt and nothing is replaced.
        #[arg(short = 'y', long)]
        yes: bool,
        /// Report what would be copied without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Name every file in the copied, overwritten, and left-alone lists
        /// instead of the first few from each top-level directory.
        #[arg(short = 'v', long)]
        verbose: bool,
    },
}

const PROOF_HELP: &str = "\
Proof check: with `defaults.pr_proof_variable` set, an agent in an issue
worktree is refused before the push unless that variable answers every item of
the issue's `Done when` or `Acceptance criteria` section, each on an unindented
line starting with the item's number:

--arg proof='1. test `refuses_a_gap`
2. test `opens_when_covered`'";

#[derive(Subcommand, Debug, PartialEq)]
pub(crate) enum PrCmd {
    /// Open (or reuse) this branch's PR.
    ///
    /// Pushes the branch first unless `--no-push`. A PR this run opens is a
    /// draft unless `--ready` or `defaults.pr_create_state` says otherwise;
    /// an open PR that already exists is reused with its state left alone.
    #[command(after_help = PROOF_HELP)]
    Create {
        /// Open as a draft, whatever `defaults.pr_create_state` says.
        #[arg(short = 'd', long)]
        draft: bool,
        /// Open ready for review, whatever `defaults.pr_create_state` says.
        #[arg(short = 'r', long, conflicts_with = "draft")]
        ready: bool,
        /// Reviewer: a `[people]` alias. Repeatable. Adds forge reviewers and
        /// sends no Slack.
        #[arg(long = "to")]
        to: Vec<String>,
        /// PR base branch, instead of the configured baseline ref.
        #[arg(long)]
        base: Option<String>,
        /// PR title, instead of the one the template renders.
        #[arg(short = 't', long = "pr-title")]
        pr_title: Option<String>,
        /// PR body, instead of the one the template renders.
        #[arg(short = 'b', long = "pr-body")]
        pr_body: Option<String>,
        /// Image or video to upload into the body of the PR this run opens.
        /// Repeatable. Alt text follows `#`. A body reference to the same path,
        /// like `![alt](./after.png)`, becomes the uploaded URL; an attachment
        /// the body does not reference is appended. GitHub only; paths are
        /// relative to the working directory (or `-C`). Refused when the PR
        /// already exists.
        #[arg(long, value_name = "FILE[#ALT]")]
        attach: Vec<String>,
        /// Open or update the PR without pushing the branch first.
        #[arg(long = "no-push")]
        no_push: bool,
        /// Use this PR for this run: a PR URL on the forge or a bare number
        /// (meaning `[forge] repo`). Replaces a wrong recorded binding.
        #[arg(long)]
        pr: Option<String>,
        #[command(flatten)]
        vars: VarArgs,
    },
    /// Print the PR title and body `pr create` would send.
    ///
    /// Takes the same title, body and template arguments, renders the
    /// `pr_title` and `pr_body` templates in this worktree, and prints
    /// {"title": ..., "body": ...} without touching the forge. Pass both
    /// unchanged to whatever opens the PR: inside an agent session this
    /// records a receipt of each.
    Render {
        /// PR title, the `input` of the `pr_title` template.
        #[arg(short = 't', long = "pr-title")]
        pr_title: Option<String>,
        /// PR body, the `input` of the `pr_body` template.
        #[arg(short = 'b', long = "pr-body")]
        pr_body: Option<String>,
        #[command(flatten)]
        vars: VarArgs,
    },
    /// Mark this branch's PR ready for review.
    ///
    /// Pushes the branch first unless `--no-push`. A PR that is already ready
    /// is reported and changed in no way.
    Ready {
        /// Reviewer: a `[people]` alias. Repeatable. Adds forge reviewers and
        /// sends no Slack.
        #[arg(long = "to")]
        to: Vec<String>,
        /// Mark ready without pushing the branch first.
        #[arg(long = "no-push")]
        no_push: bool,
        /// Use this PR for this run: a PR URL on the forge or a bare number
        /// (meaning `[forge] repo`). Replaces a wrong recorded binding.
        #[arg(long)]
        pr: Option<String>,
    },
    /// Show one worktree's PR and issue id.
    ///
    /// Reports the current worktree, or the one SELECTOR names.
    Status(PrStatus),
    /// Check out an existing PR into a new worktree.
    ///
    /// Accepts a PR number, issue id, or URL.
    Checkout(PrCheckout),
    /// At-a-glance triage of your open PRs on the project's forge.
    List(PrList),
    /// Request or finish a review.
    Review {
        #[command(subcommand)]
        cmd: ReviewCmd,
    },
}

#[derive(clap::Args, Debug, Default, PartialEq)]
pub(crate) struct PrStatus {
    /// Issue id, branch, worktree basename, or path. Defaults to cwd.
    selector: Option<String>,
    /// Emit the worktree as one JSON object instead of a table.
    #[arg(long)]
    json: bool,
    /// Skip the network: take the PR number from the worktree's cache and
    /// leave the issue state blank.
    #[arg(long = "cache-only")]
    cache_only: bool,
}

#[derive(clap::Args, Debug, PartialEq)]
pub(crate) struct PrCheckout {
    /// `#3340` | `3340` | `PREFIX-3340` | github PR URL | tracker issue
    /// URL.
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
}

#[derive(clap::Args, Debug, PartialEq)]
pub(crate) struct PrList {
    /// Show only your open PRs. Neither flag prints both sections.
    #[arg(short = 'm', long)]
    mine: bool,
    /// Show only PRs awaiting your review. Neither flag prints both
    /// sections.
    #[arg(short = 'r', long)]
    reviews: bool,
    /// owner/repo to triage instead of the current repository.
    #[arg(short = 'R', long)]
    repo: Option<String>,
    /// Refetch from the forge instead of rendering the last run's cached
    /// rows.
    #[arg(long = "no-cache")]
    no_cache: bool,
    /// PRs fetched per search page. Lower this if the forge returns
    /// HTTP 504 on a repo with many open PRs.
    #[arg(long, default_value_t = devkit_ticket::prs::DEFAULT_BATCH_SIZE, value_parser = clap::value_parser!(u32).range(1..=100))]
    batch_size: u32,
    /// Extra attempts per page after a failed fetch, with backoff.
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u32).range(0..=10))]
    retries: u32,
}

#[derive(Subcommand, Debug, PartialEq)]
pub(crate) enum ReviewCmd {
    /// Request review on this branch's PR and Slack the reviewers.
    Request {
        /// Slack body; fills the `review_request` template's `{{ input }}`.
        body: Option<String>,
        /// Recipient: a `[people]` alias or `#channel`. Repeatable.
        #[arg(long = "to")]
        to: Vec<String>,
        /// Update the PR without pushing the branch first.
        #[arg(long = "no-push")]
        no_push: bool,
        /// Add no reviewer beyond `--to`, leave draft state alone, and send no
        /// Slack: never falls back to the PR's current reviewers.
        #[arg(long = "no-notify")]
        no_notify: bool,
        /// Use this PR for this run: a PR URL on the forge or a bare number
        /// (meaning `[forge] repo`). Replaces a wrong recorded binding.
        #[arg(long)]
        pr: Option<String>,
        #[command(flatten)]
        vars: VarArgs,
    },
    /// Announce over Slack that you finished reviewing.
    ///
    /// Notifies the author, or the `--to` recipients if given.
    Finish {
        /// Slack body; fills the `review_finish` template's `{{ input }}`.
        body: Option<String>,
        /// Recipient: a `[people]` alias or `#channel`. Repeatable. Defaults to
        /// the PR author.
        #[arg(long = "to")]
        to: Vec<String>,
        /// PR number; required when not run inside the PR's worktree.
        #[arg(long)]
        pr: Option<u64>,
        #[command(flatten)]
        vars: VarArgs,
    },
}

fn start(dir: &Option<String>) -> String {
    dir.clone().unwrap_or_else(|| ".".to_string())
}

/// Select the tracker for `ticket <verb>`, a command that writes GitHub issues
/// with `gh`, and refuse any other tracker by pointing at `ticket render` and
/// the tracker's MCP.
fn github_only(
    verb: &str,
    start: &str,
    config: Option<&str>,
) -> Result<(devkit_common::tracker::Selected, devkit_config::Config)> {
    let mut sel = devkit_common::tracker::select(config.map(Path::new), start, None);
    let cfg = sel
        .config
        .take()
        .with_context(|| format!("`ticket {verb}` needs a loadable devkit.toml"))?;
    let kind = cfg
        .tracker
        .kind
        .unwrap_or_else(|| sel.tracker.tracker.kind());
    if kind != devkit_config::TrackerKind::Github {
        bail!(
            "`ticket {verb}` writes GitHub issues only, and this project's tracker is {}. \
             Render the issue with `devkit ticket render` and {verb} it through the tracker's MCP.",
            kind.as_str()
        );
    }
    Ok((sel, cfg))
}

/// Carry out `action`. `shim_name` is the name a completion script is
/// registered under.
pub fn run((globals, action): (Globals, Action), shim_name: &'static str) -> Result<()> {
    match action {
        Action::Completions(shell) => crate::emit_completions(shell, shim_name, shim_name),
        Action::Run(verb) => {
            let _timing =
                devkit_timing::init(timing_mode(globals.timing), globals.timing_log.clone());
            run_verb(globals.dir, globals.config, verb)
        }
    }
}

fn run_verb(dir: Option<String>, config: Option<String>, verb: Verb) -> Result<()> {
    match verb {
        Verb::Ticket(cmd) => run_ticket(dir, config, cmd),
        Verb::Workspace(cmd) => run_workspace(dir, config, cmd),
        Verb::Pr(cmd) => run_pr(dir, config, cmd),
    }
}

fn run_ticket(dir: Option<String>, config: Option<String>, cmd: TicketCmd) -> Result<()> {
    match cmd {
        TicketCmd::Render { title, body, vars } => render::run(render::RenderArgs {
            title,
            body,
            vars,
            dir,
            config,
        }),
        TicketCmd::Create { title, body, vars } => create::run(create::CreateArgs {
            title,
            body,
            vars,
            dir,
            config,
        }),
        TicketCmd::Edit {
            issue,
            title,
            body,
            vars,
        } => edit::run(edit::EditArgs {
            issue,
            title,
            body,
            vars,
            dir,
            config,
        }),
        TicketCmd::Event {
            event,
            issue,
            log_file,
        } => event::run(
            Path::new(&start(&dir)),
            config.as_deref().map(Path::new),
            event,
            issue.as_deref(),
            log_file.as_deref(),
        ),
        TicketCmd::Dashboard {
            bucket,
            chart,
            mode,
            aggregate,
            all_roles,
            author,
            no_plots,
            no_cache,
        } => dashboard::run(dashboard::DashboardArgs {
            bucket,
            chart,
            mode,
            aggregate,
            all_roles,
            author,
            no_plots,
            no_cache,
            dir,
            config,
        }),
    }
}

fn run_workspace(dir: Option<String>, config: Option<String>, cmd: WorkspaceCmd) -> Result<()> {
    match cmd {
        WorkspaceCmd::Setup {
            issue_pos,
            issue,
            slug,
            apps,
            summary,
            no_summary,
            dry_run,
            no_gitignore,
            here,
        } => setup::run(setup::SetupArgs {
            issue: issue_pos.or(issue),
            slug,
            apps,
            summary,
            no_summary,
            dry_run,
            no_gitignore,
            here,
            dir,
            config,
        }),
        WorkspaceCmd::Status { ids } => status::run(&start(&dir), &ids, config.as_deref()),
        WorkspaceCmd::End {
            ids,
            yes,
            force,
            pr_only,
            clean_worktree,
            no_preserve,
        } => end::run(
            &start(&dir),
            &ids,
            end::EndFlags {
                yes,
                force,
                pr_only,
                clean_worktree,
                no_preserve,
            },
            config.as_deref(),
        ),
        WorkspaceCmd::SyncIncludes {
            selectors,
            overwrite,
            all,
            yes,
            dry_run,
            verbose,
        } => sync::run(
            &start(&dir),
            &selectors,
            sync::Flags {
                overwrite,
                all,
                yes,
                dry_run,
                verbose,
            },
            config.as_deref(),
        ),
    }
}

fn run_pr(dir: Option<String>, config: Option<String>, cmd: PrCmd) -> Result<()> {
    match cmd {
        PrCmd::Create {
            draft,
            ready,
            to,
            base,
            pr_title,
            pr_body,
            attach,
            no_push,
            pr,
            vars,
        } => pr::create::run(pr::create::Args {
            draft,
            ready,
            to,
            base,
            pr_title,
            pr_body,
            attach,
            no_push,
            pr,
            vars,
            dir,
            config,
        }),
        PrCmd::Render {
            pr_title,
            pr_body,
            vars,
        } => pr::render::run(pr::render::Args {
            pr_title,
            pr_body,
            vars,
            dir,
            config,
        }),
        PrCmd::Ready { to, no_push, pr } => pr::ready::run(pr::ready::Args {
            to,
            no_push,
            pr,
            dir,
            config,
        }),
        PrCmd::Status(PrStatus {
            selector,
            json,
            cache_only,
        }) => info::run(
            &start(&dir),
            selector.as_deref(),
            json,
            cache_only,
            config.as_deref(),
        ),
        PrCmd::Checkout(PrCheckout {
            target,
            worktree_path,
            setup,
            apps,
        }) => checkout::run(checkout::CheckoutArgs {
            target,
            worktree_path,
            setup,
            apps,
            dir,
            config,
        }),
        PrCmd::List(PrList {
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
            config,
            devkit_ticket::prs::Fetch {
                batch_size,
                retries,
            },
        ),
        PrCmd::Review { cmd } => match cmd {
            ReviewCmd::Request {
                body,
                to,
                no_push,
                no_notify,
                pr,
                vars,
            } => review::request::run(review::request::Args {
                body,
                to,
                no_push,
                no_notify,
                pr,
                vars,
                dir,
                config,
            }),
            ReviewCmd::Finish { body, to, pr, vars } => review::finish::run(review::finish::Args {
                body,
                to,
                pr,
                vars,
                dir,
                config,
            }),
        },
    }
}
