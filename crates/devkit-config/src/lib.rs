use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

mod layers;
pub use layers::{CONFIG_FILE, Layer, LayerKind, project_layers};
pub mod harness;
pub use harness::{
    AppMatch, CommandRule, Fidelity, HarnessSection, LogSection, PolicyAction, PromptFidelity,
    RuleAction, Severity, ShellSetting,
};

#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
pub struct Config {
    /// Project-wide paths and branch conventions.
    #[serde(default)]
    pub defaults: Defaults,
    /// One table per runnable app, keyed by the app id passed to
    /// `issue setup --apps` and `devrun up`.
    #[serde(default)]
    pub apps: HashMap<String, AppConfig>,
    /// Reviewer/recipient aliases, keyed by the short name passed to
    /// `issue review --to`.
    #[serde(default)]
    pub people: HashMap<String, Person>,
    /// The `devkitd` supervisor: autostart gate, crash-loop budget, and the
    /// memory and health policies applied to supervised servers.
    #[serde(default)]
    pub daemon: DaemonConfig,
    /// Linear lookups that cost an extra API round trip.
    #[serde(default)]
    pub linear: LinearConfig,
    /// Which GitHub repositories back issues and pull requests.
    #[serde(default)]
    pub github: GithubConfig,
    /// Which issue tracker backs `issue`. Detected when the table is absent.
    #[serde(default)]
    pub tracker: TrackerConfig,
    /// Width of the shared worker pool. Machine tuning; carries no project
    /// convention, so a config may hold it alone.
    #[serde(default)]
    pub parallelism: ParallelismConfig,
    /// Minijinja templates for the strings `issue setup` and `issue review`
    /// generate — branch names, worktree directories, PR fields, Slack bodies.
    #[serde(default)]
    pub templates: Templates,
    /// Canned oneshot commands, keyed by the name passed to `devrun task`.
    #[serde(default)]
    pub tasks: HashMap<String, TaskConfig>,
    /// Which sections `devkit brief` emits at session start.
    #[serde(default)]
    pub brief: BriefConfig,
    /// Commands devkit runs when a lifecycle event fires, keyed
    /// `{before,after}_<event>`.
    #[serde(default)]
    pub hooks: HooksConfig,
    /// Files copied out of an issue worktree before `issue end` removes it,
    /// keyed by the name that labels the entry's progress step and its
    /// warnings.
    #[serde(default)]
    pub preserve: HashMap<String, PreserveConfig>,
    /// Rule injection settings: enablement, severity floor, and the per-event
    /// size budget.
    #[serde(default)]
    pub rules: RulesConfig,
    /// Files injected into an agent's context, each gated by its own
    /// condition.
    #[serde(default)]
    pub context: ContextConfig,
    /// The `devkit-mcp` server the agent plugins start.
    #[serde(default)]
    pub mcp: McpConfig,
}

/// The `devkitd` supervisor: whether it starts, how long it lingers, and the
/// crash-loop, memory, and health policies it applies to supervised servers.
///
/// ```
/// # use devkit_config::Config;
/// # let cfg = Config::parse(r#"
/// [daemon]
/// enabled         = true
/// memory_warn_mb  = 1024
/// memory_limit_mb = 2048   # soft: SIGTERM, then the crash path respawns
/// memory_action   = "restart"
/// memory_max_mb   = 4096   # hard: the kernel OOM-kills the tree
/// # "#).unwrap();
/// # assert!(cfg.daemon.enabled);
/// # assert_eq!(cfg.daemon.memory_action, "restart");
/// # assert!(cfg.daemon.memory_max_mb > cfg.daemon.memory_limit_mb);
/// # assert_eq!(cfg.daemon.max_restarts, 5);
/// ```
///
/// The ordering in that example is the one to keep: the soft action is the
/// graceful first responder, and the kernel cap is the backstop for a spike
/// too fast for the poll loop.
#[derive(Debug, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct DaemonConfig {
    /// Run gate: autostart the daemon only when true, `DEVKIT_DAEMON=1` is
    /// set, or `--supervise` is passed.
    pub enabled: bool,
    /// Exit after this many idle seconds with zero clients AND zero supervised
    /// children. Env: `DEVKIT_DAEMON_IDLE_SECS`.
    pub idle_timeout_secs: u64,
    /// Crash-loop guard: restarts allowed within `restart_window_secs`. Every
    /// restart path (crash, health probe, memory) spends this one budget.
    /// Env: `DEVKIT_DAEMON_MAX_RESTARTS`.
    pub max_restarts: u32,
    /// Length of the sliding window the `max_restarts` budget is counted over.
    /// Env: `DEVKIT_DAEMON_RESTART_WINDOW`.
    pub restart_window_secs: u64,
    /// Log a loud line past this supervised tree-RSS in MB (0 = off). Warns
    /// only; it restarts nothing. Env: `DEVKIT_DAEMON_MEM_WARN_MB`.
    pub memory_warn_mb: u64,
    /// Take `memory_action` past this tree-RSS in MB (0 = off). A restart needs
    /// `memory_action = "restart"`. Env: `DEVKIT_DAEMON_MEM_LIMIT_MB`.
    pub memory_limit_mb: u64,
    /// Action when tree-RSS crosses `memory_limit_mb`: "warn" (log a line) or
    /// "restart" (SIGTERM and let the crash path respawn). Any other value
    /// falls back to warn. Env: `DEVKIT_DAEMON_MEMORY_ACTION`.
    pub memory_action: String,
    /// Consecutive supervision ticks at or over `memory_limit_mb` before the
    /// restart action fires (debounces transient allocation spikes). A tick
    /// is 500 ms.
    /// Env: `DEVKIT_DAEMON_MEM_LIMIT_TICKS`.
    pub memory_limit_ticks: u32,
    /// Hard kernel memory ceiling per supervised tree, in MB (0 = off,
    /// Linux-only). A breach OOM-kills the tree and the crash path respawns it.
    /// Set above memory_limit_mb. Needs a delegated cgroup-v2 subtree, which
    /// `devkitd install-service` sets up; without one it warns once and falls
    /// back to the soft action. Env: `DEVKIT_DAEMON_MEM_MAX_MB`.
    pub memory_max_mb: u64,
    /// Health-probe interval in seconds; 0 disables probing (no probe thread).
    /// A server judged hung is SIGTERMed and respawned through the crash path.
    /// Env: `DEVKIT_DAEMON_HEALTH_PROBE_SECS`.
    pub health_probe_secs: u64,
    /// Consecutive probe failures, after the server first answers, before it
    /// is judged hung. Env: `DEVKIT_DAEMON_HEALTH_FAIL_THRESHOLD`.
    pub health_fail_threshold: u32,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            enabled: false,
            idle_timeout_secs: 1800,
            max_restarts: 5,
            restart_window_secs: 60,
            memory_warn_mb: 0,
            memory_limit_mb: 0,
            memory_action: "warn".to_string(),
            memory_limit_ticks: 3,
            memory_max_mb: 0,
            health_probe_secs: 0,
            health_fail_threshold: 3,
        }
    }
}

/// What `devkit brief` emits. All default on: the hooks ship enabled and
/// config decides whether they produce anything, so turning the output off is
/// one line rather than a hook-wiring task. A section with nothing to report
/// is omitted whatever its switch says; a switch turned off suppresses the
/// section, and its bullets, even when the checkout has something to put in
/// it. The brief's intro names only the sections that survive. Personal
/// defaults belong in `~/.config/devkit/config.toml`, overridden per project.
///
/// ```
/// # use devkit_config::Config;
/// # let cfg = Config::parse(r#"
/// [brief]
/// locks = false
/// pins  = false
/// # "#).unwrap();
/// # assert!(!cfg.brief.locks);
/// # assert!(cfg.brief.enabled);
/// # assert!(cfg.brief.apps);
/// ```
#[derive(Debug, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct BriefConfig {
    /// The whole brief, read before any work is done. A malformed `[brief]`
    /// table falls back to the defaults rather than withholding the brief.
    pub enabled: bool,
    /// The library-versions section.
    pub pins: bool,
    /// The `lockm` line. Whether sessions ever share this checkout is not
    /// something devkit can observe, so it is a switch rather than a probe.
    pub locks: bool,
    /// The apps line and the `devrun up` / `portm` bullets. Live servers this
    /// worktree holds are reported regardless: a bound port is a fact about
    /// the machine, not a listing the brief chose to carry.
    pub apps: bool,
    /// The task table and the `devrun task` bullet.
    pub tasks: bool,
}

impl Default for BriefConfig {
    fn default() -> Self {
        BriefConfig {
            enabled: true,
            pins: true,
            locks: true,
            apps: true,
            tasks: true,
        }
    }
}

/// The `devkit-mcp` server. The plugins register it unconditionally and a
/// harness may re-enable a server the user switched off, so config is where
/// it stays off.
///
/// ```
/// # use devkit_config::Config;
/// # let cfg = Config::parse(r#"
/// [mcp]
/// enabled = false
/// # "#).unwrap();
/// # assert!(!cfg.mcp.enabled);
/// # assert!(Config::parse("").unwrap().mcp.enabled);
/// ```
#[derive(Debug, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct McpConfig {
    /// Whether the server offers its tools. Off, it still answers the
    /// handshake but lists no tools and refuses every call. Read once at
    /// startup from the directory the harness started it in, so a change lands
    /// on the next session or reconnect. A config that fails to load leaves it
    /// on.
    pub enabled: bool,
}

impl Default for McpConfig {
    fn default() -> Self {
        McpConfig { enabled: true }
    }
}

/// Commands run on a devkit lifecycle event. Most keys are named
/// `{before,after}_<event>` after the state change rather than the command
/// that reached it; `after_end` is the exception and names its run, because a
/// run-level event has exactly one caller and no worktree state to be named
/// for. Each key holds a list of argv arrays — no shell, so pipes, `&&`, and
/// globs are not available. A hook that fails to render, spawn, or exit zero
/// warns on stderr and the remaining hooks still run. Output is captured and
/// discarded. A `devkit.toml` that fails to load leaves `issue end` with no
/// hooks, so none of its keys run.
///
/// ```
/// # use devkit_config::Config;
/// # let cfg = Config::parse(r#"
/// [hooks]
/// after_worktree_create = [["zoxide", "add", "{{ worktree }}"]]
/// after_worktree_remove = [["zoxide", "remove", "{{ worktree }}"]]
/// after_end             = [["alacritree", "project", "refresh"]]
/// # "#).unwrap();
/// # assert_eq!(cfg.hooks.after_worktree_create[0][0], "zoxide");
/// # assert_eq!(cfg.hooks.after_worktree_create[0][2], "{{ worktree }}");
/// # assert_eq!(cfg.hooks.after_end.len(), 1);
/// ```
///
/// Each key is an array, so a deeper `devkit.toml` replaces the whole list
/// rather than appending to it. A project defining a key drops the
/// machine-wide list from `~/.config/devkit/config.toml` entirely.
#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct HooksConfig {
    /// Runs once in the root of a worktree `issue setup` or
    /// `issue pr checkout` has just created, after its apps are prepared and
    /// after the command has reported the worktree. Each argv element is
    /// rendered as minijinja over `worktree`, `branch`, `issue`, `slug`,
    /// `apps`, `prefix`, `role`, and `[templates.variables]`. Output is
    /// discarded, so the command's JSON stays the last line on stdout.
    pub after_worktree_create: Vec<Vec<String>>,

    /// Runs once in the root of each worktree `issue end` is about to remove,
    /// after `[preserve]` has copied its files out and before any removal in
    /// the run starts. Worktrees run one at a time, in confirmed order.
    /// Rendered over the same keys as `after_worktree_remove`. A failing hook
    /// warns and the worktree is still removed. A worktree kept back or
    /// skipped fires nothing.
    pub before_worktree_remove: Vec<Vec<String>>,

    /// Runs once per worktree `issue end` removed, after every removal in the
    /// run has finished, in the main repository root. `issue`, `slug` and
    /// `apps` come from the `.devkit/issue.toml` record read before the
    /// removal. Rendered over `worktree`, `branch`, `issue`, `slug`, `apps`,
    /// `prefix`, `worktree_root`, `primary`, and `[templates.variables]`. A
    /// worktree kept back or skipped fires nothing. Runs after the run's
    /// summary, so a failing hook never un-reports a removal, and is skipped
    /// with a warning when the main repository root does not resolve.
    pub after_worktree_remove: Vec<Vec<String>>,

    /// Runs once at the end of an `issue end` run that removed at least one
    /// worktree, after every `after_worktree_remove` hook, in the main
    /// repository root. It carries `removed` (the removed paths, in confirmed
    /// order, rendered as one argv slot), `count`, `prefix`, `worktree_root`,
    /// `primary`, and `[templates.variables]`, and none of the single-worktree
    /// keys. Skipped with a warning, like `after_worktree_remove`, when the
    /// main repository root does not resolve.
    pub after_end: Vec<Vec<String>>,
}

/// Files copied out of a worktree before `issue end` removes it. Each entry
/// names its own destination, so one run can archive different files to
/// different places. A failure warns and the worktree goes anyway, unless the
/// entry is `required`. Entries run serially in sorted name order, before any
/// removal. Symlinks are followed, the opposite of `worktree_include`, so an
/// archive may outlive the link's target.
///
/// `deny_unknown_fields` because a misspelled `required` would otherwise be
/// consumed silently, leaving the entry fail-open while the user believes the
/// files are protected.
///
/// ```
/// # use devkit_config::Config;
/// # let cfg = Config::parse(r#"
/// [preserve.scratch]
/// from     = [".scratch/"]
/// to       = "{{ worktree_root }}/archive/{{ issue }}/scratch"
/// required = true
///
/// [preserve.notes]
/// from = ["docs/notes/*.md"]
/// to   = "{{ primary }}/.devkit/archive/{{ issue }}"
/// # "#).unwrap();
/// # assert!(cfg.preserve["scratch"].required);
/// # assert!(!cfg.preserve["notes"].required);
/// # assert_eq!(cfg.preserve["notes"].from, ["docs/notes/*.md"]);
/// ```
///
/// `scratch` is `required`, so a failure there keeps the worktree, its branch
/// and its summary instead of removing them. `notes` warns and the worktree
/// goes.
#[derive(Debug, JsonSchema, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreserveConfig {
    /// Glob patterns for the files to copy, relative to the worktree root and
    /// rendered as minijinja. Name a directory (`.scratch/`) or write
    /// `.scratch/**` to archive its whole tree; `dir/*` takes direct children
    /// only. A pattern that could reach outside the worktree (absolute, rooted,
    /// holding `..`, or drive-relative like `C:scratch`) is skipped with a
    /// warning; one that matches nothing is not a failure. The default issue
    /// summary sits outside the worktree, so no pattern reaches it unless
    /// `templates.issue_summary_path` renders `{{ worktree }}`.
    pub from: Vec<String>,
    /// Destination directory, rendered as minijinja over `worktree`, `branch`,
    /// `issue`, `slug`, `apps`, `prefix`, `worktree_root`, `primary` and
    /// `[templates.variables]`. Issue fields come from the worktree's
    /// `.devkit/issue.toml` and render empty without one, and `primary` fails
    /// when the primary checkout cannot be resolved. Must render to a
    /// non-empty absolute path outside every worktree the run removes; the
    /// filesystem decides that, so a symlink, `..` or case difference does not
    /// slip past. Created when the first file lands. An existing file there is
    /// truncated and rewritten, so an interrupted copy leaves a short file.
    /// Two worktrees writing one filename into the same `to` collide; render
    /// `{{ issue }}` into it.
    pub to: String,
    /// Keep the worktree, its branch and its summary when this entry warns,
    /// and exit non-zero. Governs errors only, never an empty match.
    #[serde(default)]
    pub required: bool,
}

/// Rule injection settings. When an agent is about to write a file, devkit
/// injects the rules governing it from a prebuilt JSON index.
///
/// Injection fires on the structured edits the write stage allows (`Edit`,
/// `Write`, `MultiEdit`, `NotebookEdit`, `apply_patch`), whether or not
/// `[harness] enforce_writes` is on; shell writes inject nothing. Each rule
/// fires once per session, and compaction resets that record. Only targets
/// inside the repository inject. Separately, the session-start and
/// post-compact hooks print the repository's `must` rules through
/// `devkit rules context`.
///
/// ```
/// # use devkit_config::Config;
/// # let cfg = Config::parse(r#"
/// [rules]
/// enabled = true
/// min_severity = "should"
/// per_event_limit = 5
/// # "#).unwrap();
/// # assert!(cfg.rules.enabled);
/// # assert_eq!(cfg.rules.min_severity, "should");
/// # assert_eq!(cfg.rules.per_event_limit, 5);
/// ```
#[derive(Debug, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct RulesConfig {
    /// Rule and file injection as a whole.
    pub enabled: bool,
    /// The least severe rule still injected: `must`, `should` or `can`.
    pub min_severity: String,
    /// How many rules and files one event may inject.
    pub per_event_limit: usize,
    /// A single injected file larger than this is skipped.
    pub max_file_bytes: usize,
    /// The rendered total for one event is truncated to this.
    pub max_event_bytes: usize,
    /// Where the rule index lives. Absent means the path `repo-rules-agent`
    /// writes for this checkout's main worktree, so a built index needs no
    /// config. `devkit rules add|edit|remove` change the same file. When
    /// `devkit rules stats` reports no index, build one or point this at it.
    pub index: Option<String>,
}

impl Default for RulesConfig {
    fn default() -> Self {
        RulesConfig {
            enabled: true,
            min_severity: "should".to_string(),
            per_event_limit: 5,
            max_file_bytes: 16384,
            max_event_bytes: 65536,
            index: None,
        }
    }
}

/// Files injected into an agent's context when the condition on them holds.
///
/// ```
/// # use devkit_config::Config;
/// # let cfg = Config::parse(r#"
/// [[context.files]]
/// path = "crates/devkit-ports/AGENTS.md"
///
/// [[context.files]]
/// path = "docs/deploy.md"
/// when = { harness = ["codex"], env = { DEPLOY_TARGET = "staging" } }
/// # "#).unwrap();
/// # assert_eq!(cfg.context.files.len(), 2);
/// # assert!(cfg.context.files[0].when.is_none());
/// # assert_eq!(cfg.context.files[1].when.as_ref().unwrap().harness, vec!["codex".to_string()]);
/// ```
///
/// An entry is an array element, so a deeper `devkit.toml` replaces the whole
/// list rather than appending to it, the same as `[hooks]`. Repeat the
/// parent's entries you still want.
#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct ContextConfig {
    pub files: Vec<ContextFile>,
}

/// One injected file. It fires once per session and counts against
/// `[rules] per_event_limit`. A missing file, a directory, non-UTF-8 content,
/// or a file over `[rules] max_file_bytes` is skipped.
#[derive(Debug, JsonSchema, Deserialize, Serialize)]
pub struct ContextFile {
    /// The file to inject, relative to the directory of the `devkit.toml` that
    /// declared it.
    pub path: String,
    /// When to inject it. Absent means: when the edited target is under this
    /// file's own directory.
    #[serde(default)]
    pub when: Option<FileCondition>,
}

/// Conditions on an injected file. Every condition present must hold.
#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct FileCondition {
    /// A glob over the repo-relative targets of the tool call.
    pub path: Option<String>,
    /// Harness names: `claude-code`, `codex`, `cursor`.
    pub harness: Vec<String>,
    /// Environment variables in the hook process. An empty value means
    /// "present with any value".
    pub env: std::collections::BTreeMap<String, String>,
}

/// Linear lookups that cost an extra API round trip, so each is opt-in.
///
/// ```
/// # use devkit_config::Config;
/// # let cfg = Config::parse(r#"
/// [linear]
/// resolve_pr_links = true
/// # "#).unwrap();
/// # assert!(cfg.linear.resolve_pr_links);
/// ```
///
/// The lookup authenticates with `LINEAR_API_KEY` from the environment or
/// `~/.config/devkit/secrets.toml`; no token lives in this table.
#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct LinearConfig {
    /// Query Linear for the issues linked to each PR in `issue prs` and the
    /// `issue.prs` MCP action, adding every linked id to the ISSUE column. One
    /// extra batched round trip per 25 PRs. Fail-soft: with no key or on a
    /// Linear error the column keeps the id derived from the PR text. Linear
    /// only; under the GitHub tracker the column carries each PR's closing
    /// issues regardless.
    pub resolve_pr_links: bool,
}

/// Which GitHub repositories this project uses. Both default to the `origin`
/// remote. They are separate because a fork opens its PRs upstream while its
/// issues may sit on either side, and because a project may track issues in a
/// repository separate from its code.
///
/// This table is not under `[tracker]`: a project on Linear with a fork
/// workflow needs `pr_repo` just as much as a GitHub one does. Each key
/// resolves on its own and only when an operation needs it, so a project that
/// only reads PRs never supplies `issues_repo`. Unknown keys are refused: a
/// misspelled `issue_repo` ignored would default from `origin` and query
/// another repository's issues.
///
/// ```
/// # use devkit_config::Config;
/// # let cfg = Config::parse(r#"
/// [github]
/// issues_repo = "org/planning"
/// pr_repo     = "upstream/app"
/// # "#).unwrap();
/// # assert_eq!(cfg.github.issues_repo.as_deref(), Some("org/planning"));
/// # assert_eq!(cfg.github.pr_repo.as_deref(), Some("upstream/app"));
/// # assert!(Config::parse("[github]\nissue_repo = \"org/planning\"\n").is_err());
/// ```
#[derive(Debug, Default, Clone, JsonSchema, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GithubConfig {
    /// Repository holding the issues, e.g. `org/planning`. Defaults from an
    /// `origin` remote that reaches github.com (an ssh alias is resolved with
    /// `ssh -G`); otherwise the error names this key.
    pub issues_repo: Option<String>,
    /// Repository pull requests are opened against, e.g. `upstream/app`.
    /// Defaults like `issues_repo`. `issue prs --repo` overrides it for one
    /// run, and an ambient `GH_REPO` never does.
    pub pr_repo: Option<String>,
}

/// Which issue tracker a project uses. Absent means detect: a resolvable
/// `LINEAR_API_KEY`, then a GitHub `origin` remote, then no tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, JsonSchema, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackerKind {
    Linear,
    Github,
    None,
}

impl TrackerKind {
    /// The `[tracker] kind` spelling, which is also the serialized form.
    pub fn as_str(self) -> &'static str {
        match self {
            TrackerKind::Linear => "linear",
            TrackerKind::Github => "github",
            TrackerKind::None => "none",
        }
    }
}

impl std::fmt::Display for TrackerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The `[tracker]` table.
///
/// ```
/// # use devkit_config::{Config, TrackerKind};
/// # let cfg = Config::parse(r#"
/// [tracker]
/// kind = "github"
/// # "#).unwrap();
/// # assert_eq!(cfg.tracker.kind, Some(TrackerKind::Github));
/// # assert_eq!(Config::parse("").unwrap().tracker.kind, None);
/// ```
///
/// Naming the kind is what stops a `LINEAR_API_KEY` exported machine-wide from
/// resolving Linear for a project that does not use it.
///
/// Every tracker question goes through the resolved tracker: `issue setup`'s
/// slug and summary, `issue pr checkout`'s bare-number disambiguation,
/// `issue dashboard`'s timeline, and the ISSUE column of `issue prs`.
#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct TrackerConfig {
    /// Force a tracker instead of detecting one. Detection also decides when
    /// no config resolves: outside any devkit project, or when the config
    /// fails to load. Every `issue` command and the `issue.status` MCP action
    /// read it. `github` authenticates through `gh auth login`, `GH_TOKEN` or
    /// `GITHUB_TOKEN`; under it a bare number is a PR, since issues and PRs
    /// share one numbering, and with no resolvable `[github] issues_repo` the
    /// project runs with no tracker. `none` declares no issue states, so a
    /// merged PR and a clean tree finish a worktree. Detection that finds
    /// nothing is different: it holds that verdict open, because `issue end`
    /// deletes branches. `devkit doctor`'s `tracker` row shows which tracker
    /// resolved and why.
    pub kind: Option<TrackerKind>,
}

/// Width of the shared worker pool. Machine tuning rather than a project
/// convention, so it belongs in the personal layer at
/// `~/.config/devkit/config.toml` rather than a repository's `devkit.toml`.
///
/// ```
/// # use devkit_config::Config;
/// # let cfg = Config::parse(r#"
/// [parallelism]
/// threads = 8
/// # "#).unwrap();
/// # assert_eq!(cfg.parallelism.threads.unwrap().get(), 8);
/// # assert!(Config::parse("[parallelism]\nthreads = 0\n").is_err());
/// ```
#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct ParallelismConfig {
    /// Threads in the pool devkit shares across its parallel work, 4 when
    /// unset. `DEVKIT_THREADS` wins over this; an unparseable or zero value
    /// there is ignored. Zero is refused here rather than clamped, because
    /// rayon reads a zero thread count as one thread per core. Raising it
    /// helps most where a stat is slow, such as a Windows drive under WSL.
    pub threads: Option<std::num::NonZeroUsize>,
}

/// The state a PR is opened in by `issue pr create` when neither `--draft` nor
/// `--ready` is passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PrCreateState {
    /// Opened as a draft. Reviewers are notified only once it is marked ready.
    #[default]
    Draft,
    /// Opened ready for review.
    Ready,
}

impl std::fmt::Display for PrCreateState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Draft => f.write_str("draft"),
            Self::Ready => f.write_str("ready"),
        }
    }
}

/// Project-wide paths and branch conventions. Each key is independently
/// optional and empty (or, for `apps_dir`/`pr_base`/`stray_scan_width`, its own
/// documented default) when unset; an empty path or branch key is not
/// validated at config load, and what happens when it is used is up to the
/// feature that reads it.
///
/// `worktree_root`, `baseline_dir` and `doppler_yaml` resolve once, when the
/// config loads. `${VAR}` becomes that environment variable, and an unset one
/// is an error naming the key and the variable; `$$` is a literal `$`, and a
/// `$` followed by anything else is left alone. A leading `~/` then becomes
/// `$HOME`. A path still relative anchors to what its key names, never to the
/// working directory, and `.` and `..` fold out. `branch_prefix` gets the
/// `${VAR}` step only.
///
/// `worktree_root` and `baseline_dir` anchor to the layer that declared them,
/// so a project can commit this table as it stands and have it hold on every
/// machine:
///
/// ```
/// # use devkit_config::Config;
/// # let cfg = Config::parse(r#"
/// [defaults]
/// worktree_root = "../myproject_worktrees"
/// baseline_dir  = "../myproject_worktrees/_baselines"
/// baseline_ref  = "origin/main"
/// # "#).unwrap();
/// # assert_eq!(cfg.defaults.baseline_ref, "origin/main");
/// # assert_eq!(cfg.defaults.apps_dir, "apps");
/// # assert!(cfg.defaults.branch_prefix.is_empty());
/// ```
///
/// Only `branch_prefix` is personal. Put it in `devkit.local.toml`, or write
/// `"${USER}/"`.
#[derive(Debug, JsonSchema, Deserialize, Serialize)]
pub struct Defaults {
    /// Directory issue worktrees are created under. Names a location on this
    /// machine, so a relative value anchors to the directory of the config
    /// layer that declared it, even when that layer is the repository's main
    /// checkout. Defaults to the primary checkout's `_worktrees` sibling, e.g.
    /// `~/git/example_worktrees` beside `~/git/example`.
    #[serde(default)]
    pub worktree_root: String,
    /// Prefix on branches created by `issue setup`, e.g. `you/`. `${VAR}`
    /// expands, as in `"${USER}/"`, and nothing else does.
    #[serde(default)]
    pub branch_prefix: String,
    /// Git ref the baseline server tracks, e.g. `origin/staging`. Defaults to
    /// the remote's default branch from `origin/HEAD`; when neither resolves,
    /// set this or run `git remote set-head origin -a`.
    #[serde(default)]
    pub baseline_ref: String,
    /// Directory baseline worktrees are created under, one per fork-point
    /// commit, each made on demand. Anchors like `worktree_root`. Defaults to
    /// `_baselines` under `worktree_root`. `devrun baseline prune` reclaims
    /// only directories carrying devkit's `.devkit/baseline.toml` marker, and
    /// only once no worktree's `.devkit/issue.toml` names them. A directory
    /// here without that marker is reported and never touched. A baseline
    /// devkit expects whose marker is missing is rebuilt in place, and one
    /// whose marker cannot be read is refused.
    #[serde(default)]
    pub baseline_dir: String,
    /// Path to the repo's `doppler.yaml`; its `setup` paths seed app path
    /// inference. Names a file inside the repository, so a relative value
    /// anchors to the checkout reading the config: each worktree resolves its
    /// own copy. Leave empty and every app needs its own `path`.
    #[serde(default)]
    pub doppler_yaml: String,
    /// Repo-relative directory apps live under (e.g. "apps"). Used to infer app
    /// paths from doppler.yaml and to detect changed apps in a diff.
    #[serde(default = "default_apps_dir")]
    pub apps_dir: String,
    /// Base branch `issue pr create` opens PRs against (e.g. "main",
    /// "staging"). `--base` overrides it for one run.
    #[serde(default = "default_pr_base")]
    pub pr_base: String,
    /// State `issue pr create` opens a PR in when neither `--draft` nor
    /// `--ready` is given. Draft by default, so a new PR never lands in
    /// anyone's review queue until it is marked ready.
    #[serde(default)]
    pub pr_create_state: PrCreateState,
    /// Refuse any run that would leave a PR ready for review with no human
    /// GitHub reviewer other than its own author: `issue pr create --ready`,
    /// `issue pr ready`, and the draft-to-ready flip in `issue review request`.
    /// Off by default.
    #[serde(default)]
    pub require_pr_reviewer: bool,
    /// Glob patterns for status-check names to discount from a PR's CHECK
    /// verdict — e.g. a deploy left red by an unfinished PR. Matched
    /// case-insensitively against each check's name; a PR reads green when only
    /// ignored checks fail, and the ignored failures are still surfaced in the
    /// triage output rather than hidden.
    #[serde(default)]
    pub ignored_checks: Vec<String>,
    /// Width of each app's port-band scan window for stray detection:
    /// ports `[base_port, base_port + stray_scan_width)`. Default 64.
    #[serde(default = "default_stray_scan_width")]
    pub stray_scan_width: u16,
    /// Glob patterns (relative to the primary checkout's root) for untracked
    /// local files `issue setup` and `issue pr checkout` copy into a new
    /// worktree, and `issue sync-includes` into existing ones. Existing
    /// destinations are left alone, and a failed copy warns. Anchor patterns
    /// (`apps/*/.env.local`): `**` descends into `node_modules`. `a/**` matches
    /// every path below `a`, direct children included; a bare `**` covers the
    /// checkout root. A directory match or a trailing `/` copies recursively,
    /// reading the whole subtree into memory first.
    ///
    /// A symlink is reproduced as a link to the same target; on Windows that
    /// needs Developer Mode or administrator rights, and a refused link warns
    /// and is skipped. A symlinked directory that is a pattern's own anchor
    /// (`linked/**`) is walked through; write `linked/` to get the link.
    #[serde(default)]
    pub worktree_include: Vec<String>,
    /// Write the issue summary file on every `issue setup`, as though
    /// `--summary` were passed. `--summary` / `--no-summary` still win for one
    /// run. Its path and body come from `templates.issue_summary_path` and
    /// `templates.issue_summary`.
    #[serde(default)]
    pub issue_summary: bool,
}

/// Written out rather than derived so the defaulted table matches what serde
/// fills in for an omitted key; a derived `Default` would silently disagree on
/// `apps_dir`, `pr_base`, and `stray_scan_width`.
impl Default for Defaults {
    fn default() -> Self {
        Defaults {
            worktree_root: String::new(),
            branch_prefix: String::new(),
            baseline_ref: String::new(),
            baseline_dir: String::new(),
            doppler_yaml: String::new(),
            apps_dir: default_apps_dir(),
            pr_base: default_pr_base(),
            pr_create_state: PrCreateState::default(),
            require_pr_reviewer: false,
            ignored_checks: Vec::new(),
            stray_scan_width: default_stray_scan_width(),
            worktree_include: Vec::new(),
            issue_summary: false,
        }
    }
}

fn default_apps_dir() -> String {
    "apps".to_string()
}

fn default_pr_base() -> String {
    "main".to_string()
}

fn default_stray_scan_width() -> u16 {
    64
}

/// A team member's handle mapping (Slack user-id, GitHub login, etc.). No
/// token lives here: `SLACK_TOKEN` resolves from the environment or
/// `~/.config/devkit/secrets.toml`.
///
/// ```
/// # use devkit_config::Config;
/// # let cfg = Config::parse(r#"
/// [people.alice]
/// slack  = "U0XXXXXXXXX"
/// github = "alice-gh"
/// # "#).unwrap();
/// # assert_eq!(cfg.people["alice"].slack, "U0XXXXXXXXX");
/// # assert_eq!(cfg.people["alice"].github.as_deref(), Some("alice-gh"));
/// ```
#[derive(Debug, JsonSchema, Deserialize, Serialize)]
pub struct Person {
    /// Slack user or channel id, e.g. `U0XXXXXXXXX`.
    pub slack: String,
    /// GitHub login requested as the PR reviewer when this alias is passed to
    /// `issue review request --to`. Omit and the alias only gets Slacked.
    #[serde(default)]
    pub github: Option<String>,
}

/// A file written into an app's directory during `issue setup`, before the
/// app's `setup` commands run. Parent directories are created. Existing files
/// are left untouched unless `overwrite` is set.
///
/// ```
/// # use devkit_config::Config;
/// # let cfg = Config::parse(r#"
/// [apps.web]
/// base_port = 4100
/// launch    = ["next", "dev", "-p", "{{ port }}"]
///
/// [[apps.web.prep_files]]
/// path    = ".env.local"
/// content = """
/// SOME_FEATURE_FLAG=dummy
/// """
/// # "#).unwrap();
/// # let prep = &cfg.apps["web"].prep_files[0];
/// # assert_eq!(prep.path, ".env.local");
/// # assert_eq!(prep.content, "SOME_FEATURE_FLAG=dummy\n");
/// # assert!(!prep.overwrite);
/// ```
#[derive(Debug, Clone, JsonSchema, Deserialize, Serialize)]
pub struct PrepFile {
    /// Target path, relative to the app's directory.
    pub path: String,
    /// File contents, rendered as minijinja over `prefix`, `issue`, `slug`,
    /// `apps`, `app`, `branch`, `worktree`, `role` and `[templates.variables]`,
    /// then written with no newline added. Emit a literal `{{` with
    /// `{% raw %}...{% endraw %}`.
    pub content: String,
    /// Overwrite an existing file rather than skipping it.
    #[serde(default)]
    pub overwrite: bool,
}

/// One step of a sequence task: run a sibling command task, or bring an app up.
///
/// ```
/// # use devkit_config::{Config, Step};
/// # let cfg = Config::parse(r#"
/// [tasks.stack]
/// steps = [{ task = "api-build" }, { up = "api" }]
/// # "#).unwrap();
/// # assert_eq!(
/// #     cfg.tasks["stack"].steps,
/// #     vec![Step::Task("api-build".into()), Step::Up("api".into())],
/// # );
/// ```
#[derive(Debug, Clone, JsonSchema, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    /// Name of a sibling command task to run at this point in the sequence.
    Task(String),
    /// Name of an app to bring up. Idempotent: an app already running in this
    /// worktree is reported rather than spawned again.
    Up(String),
}

/// One entry of a task's `run`, each rendering exactly one minijinja template
/// and differing only in what it does with the result.
///
/// ```
/// # use devkit_config::{Config, RunAction, RunArg};
/// # let cfg = Config::parse(r#"
/// [tasks.stage]
/// run = ["git", "add", "--", { split = "{{ files }}", on = ";" }]
/// # "#).unwrap();
/// # let run = &cfg.tasks["stage"].run;
/// # assert_eq!(run[0], RunArg::from("git"));
/// # assert_eq!(
/// #     run[3],
/// #     RunArg::Action(RunAction::Split { split: "{{ files }}".into(), on: ";".into() }),
/// # );
/// ```
///
/// `devrun task stage --arg 'files=new file.txt;other.txt'` reaches `git` as
/// four arguments, the space in the first filename included, because devkit
/// execs the argv rather than handing it to a shell.
#[derive(Debug, Clone, PartialEq, JsonSchema, Deserialize, Serialize)]
#[serde(untagged)]
pub enum RunArg {
    /// Renders to exactly one argument, whatever the result contains.
    Scalar(String),
    /// A table naming what to do with the rendered result.
    Action(RunAction),
}

/// A `run` entry written as a table. Each action is recognized by its own
/// template key, so the key sets of any two actions must stay disjoint.
///
/// `deny_unknown_fields` because actions are matched untagged: without it a
/// misspelled key alongside a well-formed `split`/`on` pair deserializes
/// silently, and the typo changes nothing the author can see.
#[derive(Debug, Clone, PartialEq, JsonSchema, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum RunAction {
    /// Renders, then splits on `on` (never empty) into a run of separate
    /// arguments. A template rendering empty contributes no arguments at all.
    /// Pick an `on` the values cannot contain. Splits come last in `run`,
    /// after at least two plain entries, or the command guard cannot recognize
    /// the task.
    Split { split: String, on: String },
}

impl RunArg {
    /// The minijinja template this entry renders. Port scanning and variable
    /// discovery read it without caring which variant produced it.
    pub fn template(&self) -> &str {
        match self {
            Self::Scalar(s) => s,
            Self::Action(RunAction::Split { split, .. }) => split,
        }
    }
}

impl From<String> for RunArg {
    fn from(value: String) -> Self {
        Self::Scalar(value)
    }
}

impl From<&str> for RunArg {
    fn from(value: &str) -> Self {
        Self::Scalar(value.into())
    }
}

/// A canned oneshot invoked by name via `devrun task`: either a command
/// (`run`, optionally scoped to an `app` for cwd + static_env) or a sequence
/// (`steps`). Exactly one of `run`/`steps` must be set; a sequence task
/// carries no `app`/`env`. Validated at resolution, not at parse.
///
/// ```
/// # use devkit_config::Config;
/// # let cfg = Config::parse(r#"
/// [tasks.api-prod-build]
/// description = "prod nitro build (node-server preset)"
/// app = "api-prod"        # run in this app's dir, inherit its static_env
/// run = ["doppler", "run", "-c", "dev_local",
///        "--preserve-env=NITRO_PRESET", "--", "bun", "nitro", "build"]
/// env = { NITRO_PRESET = "node-server" }
///
/// [tasks.commit]
/// run = ["git", "commit", "-m", "{% if issue is defined %}{{ issue }}: {% endif %}{{ msg }}"]
/// guard = true            # redirect an agent's own `git commit` here
///
/// [tasks.profile-lab-os]
/// description = "prod api + profiled lab-os, wired together"
/// steps = [
///   { task = "api-prod-build" },
///   { up = "api-prod" },
/// ]
/// # "#).unwrap();
/// # let build = &cfg.tasks["api-prod-build"];
/// # assert_eq!(build.app.as_deref(), Some("api-prod"));
/// # assert_eq!(build.env["NITRO_PRESET"], "node-server");
/// # assert_eq!(cfg.tasks["commit"].guard, Some(true));
/// # assert!(cfg.tasks["profile-lab-os"].run.is_empty());
/// # assert_eq!(cfg.tasks["profile-lab-os"].steps.len(), 2);
/// ```
///
/// `msg` is an arg of the `commit` task: no `[templates.variables]` entry
/// supplies it, so `devrun task commit --arg msg=...` is required.
#[derive(Debug, Default, Clone, JsonSchema, Deserialize, Serialize)]
pub struct TaskConfig {
    /// One line shown in `devrun task --list` and the session brief.
    #[serde(default)]
    pub description: Option<String>,
    /// App whose directory the task runs in, inheriting that app's
    /// `static_env`. Omit to run at the repo root.
    #[serde(default)]
    pub app: Option<String>,
    /// The command as one argv (program + args), run in the foreground with
    /// its exit code propagated. Every entry is a minijinja template over
    /// `{{ port }}`, `ports['<app>']`, `[templates.variables]`, and `issue`,
    /// `slug`, `branch` from the worktree; any other name read is an arg of
    /// the task, required unless a variable supplies a default. Minijinja's
    /// `default` filter and `is defined` do not make an arg optional. `issue`,
    /// `slug` and `branch` are undefined outside an issue worktree, so a task
    /// run in both guards them with `{% if issue is defined %}`. The program
    /// must be a plain string. A Doppler invocation is refused for `prd` the
    /// same as an app's `launch`. Mutually exclusive with `steps`.
    #[serde(default)]
    pub run: Vec<RunArg>,
    /// A sequence run in order, stopping at the first failure, each step a
    /// command task or an `up`. Each step re-resolves its ports and gates
    /// immediately before it runs. A sequence sets only `description` and
    /// `steps`, and cannot name another sequence. Mutually exclusive with
    /// `run`.
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Env vars set for this task, rendered like `run`, layered over the app's
    /// `static_env` and under the CLI's `--env-file` and `--env`. Tasks get no
    /// `url_env` wiring; reference the app through `ports[...]`.
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    /// Command tasks only: apps whose server must be live in this worktree
    /// when the task executes. Each name must be referenced through
    /// `ports['<app>']` (not `{{ port }}`); an `--env` override of every value
    /// referencing it waives the check and the allocation. References in
    /// `run` cannot be overridden.
    #[serde(default)]
    pub require_live: Vec<String>,
    /// Force the command guard's decision for this task instead of deriving it.
    /// `true` always redirects a matching command to `devrun task <name>`;
    /// `false` never does. Absent derives from whether devkit changes the
    /// process at all.
    #[serde(default)]
    pub guard: Option<bool>,
    /// Args this task requires beyond what `[templates.variables]` markings
    /// say, as name to marking. A task entry beats a variable entry, including
    /// relaxing one to `never`. Neither can lower the derived floor: an arg
    /// with no default is required whatever this says. Each name must be an
    /// arg the task reads. A sequence step inherits its task's markings.
    #[serde(default)]
    pub required_args: std::collections::BTreeMap<String, Required>,
}

/// Longest branch `issue setup` renders before it shortens the slug to fit,
/// and the width the `issue status` branch column renders before eliding. One
/// number so a branch devkit created is never the one the table has to cut.
pub const DEFAULT_BRANCH_MAX: usize = 46;
/// Longest worktree directory name `issue setup` renders. Shorter than the
/// branch limit because a directory name is charged against a filesystem path
/// limit and a branch name is not.
pub const DEFAULT_WORKTREE_DIR_MAX: usize = 24;
/// Longest worktree directory name `issue pr checkout` renders.
pub const DEFAULT_CHECKOUT_WORKTREE_DIR_MAX: usize = 46;

pub const DEFAULT_BRANCH: &str = "{{ prefix }}{{ slug }}";
pub const DEFAULT_WORKTREE_DIR: &str = "{{ slug }}";
pub const DEFAULT_PR_TITLE: &str = "{{ input }}";
pub const DEFAULT_PR_BODY: &str = "{{ input }}";
pub const DEFAULT_REVIEW_REQUEST: &str = "{{ input }} {{ pr_url }}";
pub const DEFAULT_REVIEW_FINISH: &str = "{{ input }} {{ pr_url }}";
pub const DEFAULT_ISSUE_SUMMARY_PATH: &str = "ISSUE_SUMMARY_{{ issue }}.md";
pub const DEFAULT_ISSUE_SUMMARY: &str = "\
# {{ issue }}: {{ title }}\n\
\n\
- **Issue:** {{ url }}\n\
{% if parent %}- **Parent:** {{ parent }}\n{% endif %}\
{% if project %}- **Project:** {{ project }}\n{% endif %}\
- **Worktree:** {{ worktree }}\n\
- **Branch:** {{ branch }}\n\
{% if apps %}- **Apps in scope:** {{ apps | join(\", \") }}\n{% endif %}\
- **State / assignee:** {{ state }} / {{ assignee }}\n\
- **Priority{% if estimate %} / estimate{% endif %}:** {{ priority }}\
{% if estimate %} / {{ estimate }}{% endif %}\n\
{% if labels %}- **Labels:** {{ labels | join(\", \") }}\n{% endif %}\
\n\
## Description\n\
\n\
{{ description }}\n\
\n\
## Summary\n\
\n\
## Pointers\n";

pub const DEFAULT_CHECKOUT_WORKTREE_DIR: &str =
    "{{ pr_number }}-{{ pr_title }}{% if linear_id %}_[{{ linear_id }}]{% endif %}";

/// Names devkit supplies when it renders a worktree's templates and hooks:
/// `role` for every worktree it creates, `sha` for a baseline, whose fork-point
/// commit it names. A `[templates.variables]` entry of the same name would be
/// shadowed silently there, because `template::render` merges variables
/// underneath the context.
///
/// Only these two. `issue`, `slug`, `branch`, `apps` and `prefix` have been
/// context keys from the start, so a project may already shadow one on
/// purpose.
const RESERVED_VARIABLES: [&str; 2] = ["role", "sha"];

/// Who must supply an `--arg` even though a default exists. `Never` is the
/// absence of a marking, not a licence to omit: the derived rule (an arg with
/// no default is required) is a floor this enum sits on top of and cannot
/// lower. `Never` is therefore meaningful only as a task-level override of a
/// variable-level marking. Who counts as an agent is `DEVKIT_CALLER`, then a
/// harness session variable, then a non-terminal stdin.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, JsonSchema, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Required {
    #[default]
    Never,
    Humans,
    Agents,
    Always,
}

/// One `[templates.variables]` entry: a bare string, or a table carrying a
/// `default`, a `required` marking and a `description`.
///
/// `deny_unknown_fields` for the reason `RunAction` documents: under untagged
/// matching a misspelled key beside a well-formed pair deserializes silently,
/// and the author sees no diagnostic while their guard does nothing.
///
/// ```
/// # use devkit_config::{Config, Required};
/// # let cfg = Config::parse(r#"
/// [templates.variables]
/// team = "platform"                              # a constant, never required
/// msg = { default = "wip", required = "agents" } # defaulted, but agents must pass it
/// ticket = { required = "always" }               # declared, no default, always required
/// body = { default = "", description = "why the change was made, if the subject does not say" }
/// # "#).unwrap();
/// # let v = &cfg.templates.variables;
/// # assert_eq!(v["team"].required(), Required::Never);
/// # assert_eq!(v["msg"].default_value(), Some("wip"));
/// # assert_eq!(v["ticket"].default_value(), None);
/// # assert!(v["body"].description().unwrap().starts_with("why"));
/// ```
///
/// `default` is the fallback used when nothing else supplies the name, which
/// is what these entries have always been: `template::render` merges them
/// underneath the context, and `--arg` overwrites them. Note that minijinja's
/// own `default` filter is a different thing and does not affect whether an
/// arg is required.
#[derive(Debug, Clone, PartialEq, Eq, JsonSchema, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum VariableDecl {
    Value(String),
    Table {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<String>,
        /// `None` is a marking the author did not write, which is not the same
        /// as one written as `never`: an unmarked valueless entry is required
        /// by the derived rule, while `required = "never"` on the same entry
        /// asks to relax that and is refused at load.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        required: Option<Required>,
        /// What to pass for this arg, shown beside it where devkit asks for
        /// it: a missing-arg error (`issue pr create` and `issue review`
        /// included), a command-guard redirect to a task that reads it,
        /// `devkit config tasks <name>` and `devkit config variables`.
        /// Explain an arg here rather than in a guard rule's `reason`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
}

impl VariableDecl {
    /// The fallback value, absent for a declaration that only marks a name.
    pub fn default_value(&self) -> Option<&str> {
        match self {
            VariableDecl::Value(v) => Some(v),
            VariableDecl::Table { default, .. } => default.as_deref(),
        }
    }

    /// The marking, defaulted for an entry that carries none.
    pub fn required(&self) -> Required {
        match self {
            VariableDecl::Value(_) => Required::Never,
            VariableDecl::Table { required, .. } => required.unwrap_or_default(),
        }
    }

    /// What to pass, absent for a bare string or an entry that says nothing.
    pub fn description(&self) -> Option<&str> {
        match self {
            VariableDecl::Value(_) => None,
            VariableDecl::Table { description, .. } => description.as_deref(),
        }
    }

    /// The marking exactly as written, `None` when the author wrote none.
    /// Only load-time validation needs this distinction; resolution reads
    /// `required()`.
    pub fn written_required(&self) -> Option<Required> {
        match self {
            VariableDecl::Value(_) => None,
            VariableDecl::Table { required, .. } => *required,
        }
    }
}

impl From<&str> for VariableDecl {
    fn from(v: &str) -> Self {
        VariableDecl::Value(v.to_string())
    }
}

/// Config-driven minijinja templates for the issue-lifecycle strings. Each
/// `None` field falls back to its `DEFAULT_*` constant, which reproduces the
/// historical hardcoded output. `variables` are user constants merged under
/// every render context. Rendering is strict: an undefined variable is an
/// error, so a typo surfaces on the first run.
///
/// ```
/// # use devkit_config::Config;
/// # let cfg = Config::parse(r#"
/// [templates]
/// branch             = "{{ prefix }}{{ issue }}-{{ slug }}"
/// worktree_dir       = "{{ slug }}"
/// pr_title           = "{{ issue }}: {{ input }}"
/// pr_body            = "Closes {{ issue }}.\n\n{{ input }}"
/// issue_summary_path = "{{ worktree }}/.devkit/issue.md"
///
/// [templates.variables]
/// team = "platform"
/// # "#).unwrap();
/// # let t = &cfg.templates;
/// # assert_eq!(t.branch(), "{{ prefix }}{{ issue }}-{{ slug }}");
/// # assert_eq!(t.pr_body(), "Closes {{ issue }}.\n\n{{ input }}");
/// # assert_eq!(t.worktree_dir_max(), 24);
/// # assert_eq!(t.variables["team"].default_value(), Some("platform"));
/// ```
///
/// `issue_summary_path` renders `{{ worktree }}` here, which keeps the summary
/// inside the worktree where a `[preserve]` pattern can name it; left relative
/// it is taken from `defaults.worktree_root` instead.
#[derive(Debug, JsonSchema, Deserialize, Serialize, Default)]
pub struct Templates {
    /// Branch name created by `issue setup`. Context: `prefix`, `issue`,
    /// `slug`, `short_slug`, `apps`. Defaults to `{{ prefix }}{{ slug }}`.
    pub branch: Option<String>,
    /// Longest branch `issue setup` will render. A derived slug is shortened on
    /// a word boundary to fit. A template whose fixed text already fills this
    /// yields the shortest slug still worth reading rather than an error, since
    /// a git ref has no hard length limit. `short_slug` is not measured by it.
    /// Defaults to 46.
    pub branch_max: Option<usize>,
    /// Worktree directory name created by `issue setup`, relative to
    /// `defaults.worktree_root`. Same context as `branch`; defaults to
    /// `{{ slug }}`.
    pub worktree_dir: Option<String>,
    /// Longest worktree directory name `issue setup` will render from
    /// `{{ short_slug }}`. A template whose fixed text already fills this is an
    /// error: a limit on a filesystem path that does not hold is worse than a
    /// setup that stops. Has no effect on a `worktree_dir` that does not render
    /// `{{ short_slug }}`. Defaults to 24.
    pub worktree_dir_max: Option<usize>,
    /// Worktree directory name created by `issue pr checkout`. Context:
    /// `pr_number`, `pr_title`, `linear_id`, `linear_title`. Titles are
    /// slugified, and the `linear_*` names are historical: they carry
    /// whichever tracker's id and title resolved, and are empty on the
    /// PR-only path. Defaults to `{{ pr_number }}-{{ pr_title }}`, plus
    /// `_[{{ linear_id }}]` when reached through an issue.
    pub checkout_worktree_dir: Option<String>,
    /// Longest worktree directory name `issue pr checkout` will render.
    /// `pr_title` and `linear_title` are shortened to fit, splitting the budget
    /// when a template renders both. A template whose fixed text already fills
    /// this is an error. Defaults to 46.
    pub checkout_worktree_dir_max: Option<usize>,
    /// Title of a PR opened by `issue pr create`. `{{ input }}` is the
    /// `--pr-title` argument; the rendered title must not be empty.
    pub pr_title: Option<String>,
    /// Body of a PR opened by `issue pr create`. `{{ input }}` is the
    /// `--pr-body` argument. Rendered only when a PR is actually opened, not
    /// when an existing one is reused.
    pub pr_body: Option<String>,
    /// Slack message sent by `issue review request`. Rendered once per
    /// recipient with `name`, `slack_id` (empty for a channel), `pr_url`,
    /// `pr_title` (the PR's own title from GitHub), `input`, and `branch`,
    /// `issue`, `slug`, `apps` from the worktree's `.devkit/issue.toml`.
    pub review_request: Option<String>,
    /// Slack message sent by `issue review finish`. Same context as
    /// `review_request`, plus `author`.
    pub review_finish: Option<String>,
    /// Where `issue setup --summary` writes the issue summary file. A relative
    /// path is taken from `defaults.worktree_root`, so the file outlives the
    /// worktree; render `{{ worktree }}` into it to keep it inside instead.
    /// Context: the `issue_summary` context below.
    pub issue_summary_path: Option<String>,
    /// Body of the file `issue setup --summary` writes when the tracker keeps
    /// no summary of its own; a GitHub issue's non-empty body is written
    /// verbatim instead. Context: `issue`,
    /// `title`, `url`, `description`, `state`, `assignee`, `priority`,
    /// `estimate`, `labels`, `parent`, `project`, `worktree`, `branch`, `slug`,
    /// `prefix`, `apps`. A field the tracker left empty renders as the empty
    /// string, so `{% if parent %}` drops its line.
    pub issue_summary: Option<String>,
    /// Constants available to every template above. A context field of the same
    /// name wins, and `--arg key=value` overrides either. An entry may instead
    /// be a table carrying a `default`, a `required` marking and a
    /// `description` of what to pass. `role` and `sha` are reserved and
    /// rejected here.
    #[serde(default)]
    pub variables: std::collections::BTreeMap<String, VariableDecl>,
}

impl Templates {
    pub fn branch(&self) -> &str {
        self.branch.as_deref().unwrap_or(DEFAULT_BRANCH)
    }

    pub fn branch_max(&self) -> usize {
        self.branch_max.unwrap_or(DEFAULT_BRANCH_MAX)
    }

    pub fn worktree_dir(&self) -> &str {
        self.worktree_dir.as_deref().unwrap_or(DEFAULT_WORKTREE_DIR)
    }

    pub fn worktree_dir_max(&self) -> usize {
        self.worktree_dir_max.unwrap_or(DEFAULT_WORKTREE_DIR_MAX)
    }

    pub fn checkout_worktree_dir(&self) -> &str {
        self.checkout_worktree_dir
            .as_deref()
            .unwrap_or(DEFAULT_CHECKOUT_WORKTREE_DIR)
    }

    pub fn checkout_worktree_dir_max(&self) -> usize {
        self.checkout_worktree_dir_max
            .unwrap_or(DEFAULT_CHECKOUT_WORKTREE_DIR_MAX)
    }

    pub fn pr_title(&self) -> &str {
        self.pr_title.as_deref().unwrap_or(DEFAULT_PR_TITLE)
    }

    pub fn pr_body(&self) -> &str {
        self.pr_body.as_deref().unwrap_or(DEFAULT_PR_BODY)
    }

    pub fn review_request(&self) -> &str {
        self.review_request
            .as_deref()
            .unwrap_or(DEFAULT_REVIEW_REQUEST)
    }

    pub fn issue_summary_path(&self) -> &str {
        self.issue_summary_path
            .as_deref()
            .unwrap_or(DEFAULT_ISSUE_SUMMARY_PATH)
    }

    pub fn issue_summary(&self) -> &str {
        self.issue_summary
            .as_deref()
            .unwrap_or(DEFAULT_ISSUE_SUMMARY)
    }

    pub fn review_finish(&self) -> &str {
        self.review_finish
            .as_deref()
            .unwrap_or(DEFAULT_REVIEW_FINISH)
    }

    /// Name to value, for building a render context. A declaration with no
    /// `default` is omitted, so a template reading it hits strict-undefined
    /// until a caller supplies the name.
    pub fn defaults(&self) -> std::collections::BTreeMap<String, String> {
        self.variables
            .iter()
            .filter_map(|(k, d)| d.default_value().map(|v| (k.clone(), v.to_string())))
            .collect()
    }

    /// Every declared name, valueless ones included. This is the `--arg`
    /// allowlist: without it a name declared only to be marked required would
    /// be unpassable.
    pub fn declared(&self) -> std::collections::BTreeSet<String> {
        self.variables.keys().cloned().collect()
    }
}

/// The `url` an app is addressed at when it sets none of its own.
pub const DEFAULT_APP_URL: &str = "http://localhost:{{ port }}";

/// One runnable app. `base_port` and `launch` are required; `path` is required
/// too whenever `defaults.doppler_yaml` cannot infer the directory.
///
/// ```
/// # use devkit_config::Config;
/// # let cfg = Config::parse(r#"
/// [apps.api]
/// base_port    = 9100
/// launch       = ["doppler", "run", "-c", "dev_local", "--", "nitro", "dev", "--port", "{{ port }}"]
/// url_env      = "API_BASE_URL"
/// provides_url = true
/// static_env   = { SOME_JWT_SECRET = "local-dev-placeholder-value" }
///
/// [apps.web]
/// base_port = 4100
/// path      = "apps/web"
/// launch    = ["next", "dev", "-p", "{{ port }}"]
/// url_env   = "API_BASE_URL"
/// # "#).unwrap();
/// # assert!(cfg.apps["api"].provides_url);
/// # assert_eq!(cfg.apps["web"].path.as_deref(), Some("apps/web"));
/// # assert_eq!(cfg.apps["web"].launch.last().unwrap(), "{{ port }}");
/// # assert_eq!(cfg.apps["api"].static_env["SOME_JWT_SECRET"], "local-dev-placeholder-value");
/// ```
///
/// `web` names the provider through `url_env` rather than by port, so the two
/// apps stay wired together in every worktree.
///
/// A per-app memory cap the daemon does not enforce is the runtime's own:
/// `static_env = { NODE_OPTIONS = "--max-old-space-size=2048" }`, or a
/// `ulimit -v` wrapper in `launch`. The runtime aborts on breach and the
/// crash path respawns it. `[daemon] memory_max_mb` is the kernel-enforced
/// alternative on Linux.
#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
pub struct AppConfig {
    /// Start of the app's port band. Each worktree is allocated its own port
    /// from here by the registry, so two worktrees never collide.
    pub base_port: u16,
    /// The complete launch command as one argv, run verbatim. devkit builds no
    /// prefix of its own, so any `doppler run -c <config> --` wrapper belongs
    /// here, and a Doppler launch whose config resolves to `prd` (or cannot be
    /// resolved) is refused. The config comes from `-c`/`--config` here, then
    /// `DOPPLER_CONFIG` in the app's env, then
    /// `doppler configure get config --scope <app dir>`. Rendered as strict
    /// minijinja over `{{ port }}`, `ports['<app>']` and
    /// `[templates.variables]`. `ports['<app>']` is that app's port in this
    /// worktree; when it is not running, devkit writes a pid-less reservation
    /// that a later `devrun up <app>` claims, so a consumer can bake the port
    /// first. A misspelled app name is an error, and a leftover `{port}` fails
    /// the launch.
    pub launch: Vec<String>,
    /// Address the app serves on, as a template over the same variables as
    /// `launch`. Defaults to `http://localhost:{{ port }}`; set it for https, a
    /// custom host, or a path prefix. It is what `devrun up` prints and what
    /// the provider wires into consumers. devkit never terminates TLS, and
    /// readiness stays a TCP probe of the port.
    #[serde(default)]
    pub url: Option<String>,
    /// Env var this app receives the `provides_url` app's URL through.
    #[serde(default)]
    pub url_env: Option<String>,
    /// This app serves the URL that consumer apps wire to via their `url_env`.
    /// Exactly one app should be marked. `devrun up` of a consumer brings the
    /// provider up too.
    #[serde(default)]
    pub provides_url: bool,
    /// Commands run in the app's directory during `issue setup`, in order. Each
    /// inner array is one argv (program + args), e.g.
    /// `[["doppler","run","-c","local","--","bun","install"]]`.
    #[serde(default)]
    pub setup: Vec<Vec<String>>,
    /// App subdirectory relative to the repo root, when it differs from the
    /// app's own name. Required unless `defaults.doppler_yaml` infers it, and
    /// wins over the inferred path when set.
    #[serde(default)]
    pub path: Option<String>,
    // Table-like fields (`static_env`, `prep_files`) kept last so the serialized
    // TOML groups scalars/arrays before the nested table and array-of-tables —
    // readable, stable output. (toml 0.8 also orders values before tables on its
    // own, so this is for source layout, not a serializer requirement.)
    /// Env vars always set for this app, rendered as minijinja over the same
    /// variables as `launch`. The lowest layer: a task's `env` and the CLI's
    /// `--env` both win over it.
    #[serde(default)]
    pub static_env: HashMap<String, String>,
    /// Files written into the app's directory during `issue setup`, before
    /// `setup` runs. A deeper layer's list replaces this one whole.
    #[serde(default)]
    pub prep_files: Vec<PrepFile>,
}

impl Config {
    pub fn parse(s: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(s).context("parsing devkit.toml")?;
        Ok(cfg)
    }
}

/// Per-leaf record of which config layer supplied each value.
#[derive(Debug, Default)]
pub struct Provenance {
    /// Resolved layer files, lowest->highest precedence.
    pub layers: Vec<PathBuf>,
    /// Dotted config path (e.g. `apps.api.base_port`) -> file that supplied it.
    pub origin: HashMap<String, PathBuf>,
    /// Dotted config path -> the layers a higher one overrode, lowest first.
    /// A leaf only one layer sets has no entry here.
    pub shadowed: HashMap<String, Vec<Shadow>>,
}

/// A value a later config layer overrode.
#[derive(Debug, Clone, PartialEq)]
pub struct Shadow {
    /// The layer that set it.
    pub file: PathBuf,
    /// What it held there.
    pub value: toml::Value,
}

/// Deep-merge parsed layers given lowest->highest precedence. Tables merge key
/// by key; every non-table value (scalar or array) is replaced wholesale by a
/// higher layer. Records, per leaf dotted-path, the highest layer that set it.
///
/// Public because the `[harness]` probe merges that one table across the same
/// layer files without resolving the whole config, and must not carry a second
/// copy of these semantics.
pub fn merge_layers(
    layers: &[(PathBuf, toml::Table)],
) -> (
    toml::Table,
    HashMap<String, PathBuf>,
    HashMap<String, Vec<Shadow>>,
) {
    let mut merged = toml::Table::new();
    let mut seen = Seen::default();
    for (path, table) in layers {
        deep_merge(&mut merged, table, path, "", &mut seen);
    }
    (merged, seen.winner, seen.shadowed)
}

/// What each leaf has been set to so far, as the layers are merged in
/// precedence order. `value` tracks the winning value alongside its file so
/// that overwriting a leaf can record both halves of what it displaced.
#[derive(Default)]
struct Seen {
    winner: HashMap<String, PathBuf>,
    value: HashMap<String, toml::Value>,
    shadowed: HashMap<String, Vec<Shadow>>,
}

fn deep_merge(
    acc: &mut toml::Table,
    overlay: &toml::Table,
    src: &Path,
    prefix: &str,
    seen: &mut Seen,
) {
    for (k, v) in overlay {
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        if let (Some(toml::Value::Table(at)), toml::Value::Table(ot)) = (acc.get_mut(k), v) {
            deep_merge(at, ot, src, &path, seen);
        } else {
            record_origin(&path, v, src, seen);
            acc.insert(k.clone(), v.clone());
        }
    }
}

/// Record the source file for every scalar/array leaf reachable from `v`. A
/// table recurses into its keys; everything else is a single leaf.
fn record_origin(path: &str, v: &toml::Value, src: &Path, seen: &mut Seen) {
    match v {
        toml::Value::Table(t) => {
            for (k, sub) in t {
                record_origin(&format!("{path}.{k}"), sub, src, seen);
            }
        }
        _ => {
            let prev_file = seen.winner.insert(path.to_string(), src.to_path_buf());
            let prev_value = seen.value.insert(path.to_string(), v.clone());
            if let (Some(file), Some(value)) = (prev_file, prev_value) {
                seen.shadowed
                    .entry(path.to_string())
                    .or_default()
                    .push(Shadow { file, value });
            }
        }
    }
}

/// Flatten a serialized config `Value` into `(dotted-path, leaf-value)` pairs.
/// Tables recurse; scalars and arrays are leaves. Mirrors `record_origin`'s
/// leaf model so every emitted path can be looked up in `Provenance::origin`.
pub fn flatten(v: &toml::Value, prefix: &str, out: &mut Vec<(String, toml::Value)>) {
    match v {
        toml::Value::Table(t) => {
            for (k, sub) in t {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(sub, &path, out);
            }
        }
        _ => out.push((prefix.to_string(), v.clone())),
    }
}

fn read_layer(p: &Path) -> Result<(PathBuf, toml::Table)> {
    // The recorded path is what `layer_dir` anchors this layer's relative
    // `[defaults]` paths to, so it has to be absolute however the caller
    // spelled it — an explicit `--config` and `$DEVKIT_CONFIG` both arrive
    // verbatim.
    let path = absolutize(p)?;
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("reading config layer {}", path.display()))?;
    let table: toml::Table = toml::from_str(&body)
        .with_context(|| format!("parsing config layer {}", path.display()))?;
    Ok((path, table))
}

/// A path anchored to the process's current directory, resolved lexically —
/// a config layer is routinely named relative to a directory that is not the
/// one the config describes.
fn absolutize(p: &Path) -> Result<PathBuf> {
    if p.is_absolute() {
        return Ok(p.to_path_buf());
    }
    let cwd = std::env::current_dir()
        .context("resolving the current directory to absolutize a config path")?;
    Ok(normalize_lexically(&cwd.join(p)))
}

/// The user's home directory. Windows sets `USERPROFILE` and not `HOME`, so a
/// `~` path and the personal config layer would both go missing there on `HOME`
/// alone. Matches what `devkit-common`'s `paths` resolves.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|s| !s.is_empty()))
        .map(PathBuf::from)
}

/// The personal fallback config layer (`~/.config/devkit/config.toml`).
/// Public so callers can tell a project-level layer from this global one.
pub fn home_config_path() -> Option<PathBuf> {
    Some(home_dir()?.join(".config/devkit/config.toml"))
}

/// The `[config]` table. Read straight off the raw layer by `is_root_layer`
/// rather than through `Config`, because it decides which layers exist before
/// there is a merged config to deserialize; this type exists so the published
/// JSON Schema can still describe it.
///
/// ```
/// # use devkit_config::LayerMarker;
/// # let doc: toml::Table = toml::from_str(r#"
/// [config]
/// root = true
/// # "#).unwrap();
/// # let marker: LayerMarker = doc["config"].clone().try_into().unwrap();
/// # assert!(marker.root);
/// ```
#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
pub struct LayerMarker {
    /// Stop walking upward at this file, and drop
    /// `~/.config/devkit/config.toml` from the layer stack.
    #[serde(default)]
    pub root: bool,
}

/// Whether a parsed layer declares `[config] root = true`.
pub(crate) fn is_root_layer(t: &toml::Table) -> bool {
    t.get("config")
        .and_then(|c| c.as_table())
        .and_then(|c| c.get("root"))
        .and_then(|r| r.as_bool())
        .unwrap_or(false)
}

/// Build the ordered layer list (lowest->highest precedence): the home config
/// (unless a `root = true` marker cuts it off), then the project layers
/// `project_layers` finds for `start`. An explicit path or `$DEVKIT_CONFIG` is
/// the sole layer.
fn discover(
    explicit: Option<&Path>,
    start: &Path,
    main_checkout: Option<&Path>,
    home: Option<&Path>,
) -> Result<Vec<(PathBuf, toml::Table)>> {
    if let Some(p) = explicit {
        return Ok(vec![read_layer(p)?]);
    }
    if let Some(p) = std::env::var_os("DEVKIT_CONFIG") {
        return Ok(vec![read_layer(&PathBuf::from(p))?]);
    }

    let found = layers::project_layers_rooted(start, main_checkout)?;

    let mut layers: Vec<(PathBuf, toml::Table)> = Vec::new();
    if !found.rooted
        && let Some(h) = home
        && h.is_file()
    {
        layers.push(read_layer(h)?);
    }
    // The root-barrier scan already parsed every layer it reached; reuse those
    // bodies rather than reading and parsing the same files a second time.
    for (layer, parsed) in found.layers.into_iter().zip(found.parsed) {
        match parsed {
            Some(table) => layers.push((absolutize(&layer.path)?, table)),
            None => layers.push(read_layer(&layer.path)?),
        }
    }

    if layers.is_empty() {
        return Err(anyhow::Error::new(NoConfig));
    }
    Ok(layers)
}

/// No config file exists anywhere the search looks. Carried as a distinct type
/// so a caller can tell it apart from a config that exists and fails to load:
/// the first means "not a devkit project" and warrants silence, the second is
/// a fault the user needs told about.
#[derive(Debug)]
pub struct NoConfig;

impl std::fmt::Display for NoConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "no devkit.toml found (--config / $DEVKIT_CONFIG / ./devkit.toml + ./devkit.local.toml walking up / ~/.config/devkit/config.toml)",
        )
    }
}

impl std::error::Error for NoConfig {}

/// What the config layers reachable from `start` amount to. `devkit brief` and
/// `devkit doctor` both have to report a fault without failing on one, and
/// both have to stay quiet outside a devkit project — neither is possible
/// while every failure is one opaque `Err`.
#[derive(Debug, PartialEq, Eq)]
pub enum Health {
    /// No devkit.toml anywhere: not a devkit project.
    Absent,
    Ok,
    /// A config exists and does not load. The string is the full `anyhow`
    /// cause chain, which names the offending file for a parse error and the
    /// offending key for a deserialization error.
    Broken(String),
}

impl Health {
    /// Classify what a `resolve` produced. Callers that need the config itself
    /// as well as the verdict classify their own `resolve` through this, so the
    /// two can never describe different loads — a caller that resolved an
    /// explicit `--config` must not be told a discovered one is fine.
    pub fn of(resolved: &Result<(Config, Provenance)>) -> Health {
        match resolved {
            Ok(_) => Health::Ok,
            Err(e) if e.downcast_ref::<NoConfig>().is_some() => Health::Absent,
            Err(e) => Health::Broken(
                e.chain()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(": "),
            ),
        }
    }
}

/// Classify the config reachable from `start` without ever failing.
/// `main_checkout` is this repository's main checkout when `start` is a
/// linked worktree of one — resolved by the caller, because `devkit-config`
/// asks git nothing.
pub fn health(start: &Path, main_checkout: Option<&Path>) -> Health {
    health_with_home(start, main_checkout, home_config_path().as_deref(), None)
}

pub(crate) fn health_with_home(
    start: &Path,
    main_checkout: Option<&Path>,
    home: Option<&Path>,
    default_worktree_root: Option<&Path>,
) -> Health {
    Health::of(&resolve_with_home(
        None,
        start,
        main_checkout,
        None,
        home,
        default_worktree_root,
    ))
}

/// Resolve the effective config by layering and deep-merging all applicable
/// files. `main_checkout` is this repository's main checkout when `start` is
/// a linked worktree of one, and `checkout_root` is the repository checkout
/// `start` lives in — both resolved by the caller, because `devkit-config`
/// asks git nothing. `default_worktree_root` fills an unset
/// `[defaults] worktree_root` — also resolved by the caller, for the same
/// reason.
pub fn resolve(
    explicit: Option<&Path>,
    start: &Path,
    main_checkout: Option<&Path>,
    checkout_root: Option<&Path>,
    default_worktree_root: Option<&Path>,
) -> Result<(Config, Provenance)> {
    resolve_with_home(
        explicit,
        start,
        main_checkout,
        checkout_root,
        home_config_path().as_deref(),
        default_worktree_root,
    )
}

/// `resolve` with an injectable home-config path (tests pass a controlled path
/// or `None` so the real `~/.config/devkit/config.toml` never participates).
pub(crate) fn resolve_with_home(
    explicit: Option<&Path>,
    start: &Path,
    main_checkout: Option<&Path>,
    checkout_root: Option<&Path>,
    home: Option<&Path>,
    default_worktree_root: Option<&Path>,
) -> Result<(Config, Provenance)> {
    // Every discovered layer path, and every `[defaults]` path resolved against
    // it, must be absolute — `strays/mod.rs` uses `worktree_root` as holder
    // identity and for prefix matching, so a relative `start` (e.g. `doctor`
    // passing `.`) must not leak into that value.
    let start_buf = absolutize(start)?;
    let start = start_buf.as_path();
    let layers = discover(explicit, start, main_checkout, home)?;
    let order: Vec<PathBuf> = layers.iter().map(|(p, _)| p.clone()).collect();
    let (merged, origin, shadowed) = merge_layers(&layers);
    if let Some(warning) = check_baseline_path(&origin, home)? {
        eprintln!("{warning}");
    }
    let mut cfg: Config = toml::Value::Table(merged)
        .try_into()
        .context("deserializing merged devkit config")?;
    reject_reserved_variables(&cfg, &origin)?;
    reject_never_without_default(&cfg, &origin)?;
    resolve_defaults(&mut cfg, &origin, checkout_root, default_worktree_root)?;
    Ok((cfg, Provenance {
        layers: order,
        origin,
        shadowed,
    }))
}

/// Check a leftover `defaults.baseline_path`: baselines are now per-fork-point
/// worktrees created under `defaults.baseline_dir`, not one checkout at a
/// fixed path, so the old key can no longer be honored. A project layer gets a
/// hard error naming the replacement. The home layer only gets a warning to
/// print (`Ok(Some(_))`) and is otherwise ignored — a home config is read
/// from every directory on the machine, including repositories that are not
/// devkit projects, and erroring there would fail every command run from any
/// of them. `Ok(None)` means the key is absent.
fn check_baseline_path(
    origin: &HashMap<String, PathBuf>,
    home: Option<&Path>,
) -> Result<Option<String>> {
    let Some(from) = origin.get("defaults.baseline_path") else {
        return Ok(None);
    };
    let home = home.map(absolutize).transpose()?;
    if home.as_deref() == Some(from.as_path()) {
        return Ok(Some(format!(
            "warning: `defaults.baseline_path` in {} is ignored; \
             baselines now live under `defaults.baseline_dir`",
            from.display()
        )));
    }
    bail!(
        "`defaults.baseline_path` in {} is no longer a checkout path. Set \
         `defaults.baseline_dir` to the directory baselines are created \
         under, and remove the old checkout with \
         `git worktree remove --force <path>`",
        from.display()
    );
}

/// Refuse a `[templates.variables]` entry that a render context already
/// supplies. Such an entry never reaches a template, so accepting it would
/// leave the author reading a value that is not the one being rendered.
fn reject_reserved_variables(cfg: &Config, origin: &HashMap<String, PathBuf>) -> Result<()> {
    for name in RESERVED_VARIABLES {
        if !cfg.templates.variables.contains_key(name) {
            continue;
        }
        let key = format!("templates.variables.{name}");
        let declared = match origin.get(&key) {
            Some(p) => format!(" in {}", p.display()),
            None => String::new(),
        };
        bail!(
            "`[templates.variables] {name}`{declared} is a reserved name: \
             devkit supplies `{name}` to every render context, so this value \
             would never be used. Rename the variable."
        );
    }
    Ok(())
}

/// Refuse `required = "never"` on a variable with no default. The derived rule
/// already requires such a name, `never` cannot lower that floor, and
/// honouring it would replace a named error with minijinja's strict-undefined
/// chain, which never mentions the arg.
///
/// Only a marking the author wrote is refused. A valueless entry carrying no
/// marking is the ordinary way to declare a name as passable, and stays
/// required by the derived rule.
///
/// A task's `required_args` is checked the same way: `never` there relaxes a
/// variable's marking, and with no default underneath there is no marking to
/// relax.
fn reject_never_without_default(cfg: &Config, origin: &HashMap<String, PathBuf>) -> Result<()> {
    for (name, decl) in &cfg.templates.variables {
        if decl.written_required() != Some(Required::Never) || decl.default_value().is_some() {
            continue;
        }
        // Provenance records the leaf that was written, and for a table entry
        // that is the `required` key rather than the variable itself.
        let declared = layer_note(origin, &format!("templates.variables.{name}.required"));
        anyhow::bail!(
            "`[templates.variables] {name}`{declared} sets `required = \"never\"` \
             with no `default`. An arg with nothing to fall back on is required \
             either way; give it a `default` or drop the marking."
        );
    }
    for (task, t) in &cfg.tasks {
        for (name, marking) in &t.required_args {
            if *marking != Required::Never {
                continue;
            }
            let has_default = cfg
                .templates
                .variables
                .get(name)
                .and_then(|d| d.default_value())
                .is_some();
            if has_default {
                continue;
            }
            let declared = layer_note(origin, &format!("tasks.{task}.required_args.{name}"));
            anyhow::bail!(
                "`[tasks.{task}] required_args.{name}`{declared} is `\"never\"`, but \
                 `{name}` has no `default` in `[templates.variables]`. A task can relax \
                 a marking, not the derived rule underneath it; give `{name}` a \
                 `default` or drop the entry."
            );
        }
    }
    Ok(())
}

/// ` (declared in <layer>)` when provenance knows which layer wrote `key`, and
/// nothing when it does not. An error that cannot name the layer still reads.
fn layer_note(origin: &HashMap<String, PathBuf>, key: &str) -> String {
    origin
        .get(key)
        .map(|p| format!(" (declared in {})", p.display()))
        .unwrap_or_default()
}

/// Expand `${VAR}` references in a config value. `$$` is a literal `$`; a `$`
/// followed by anything else is left alone, since it is a legal path character.
/// An unset variable is an error naming both the config key and the variable —
/// silently substituting an empty string would produce a plausible wrong path.
fn expand_vars(raw: &str, key: &str) -> Result<String> {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        if let Some(tail) = after.strip_prefix('$') {
            out.push('$');
            rest = tail;
        } else if let Some(tail) = after.strip_prefix('{') {
            let end = tail
                .find('}')
                .with_context(|| format!("`{key}`: unterminated `${{` in {raw:?}"))?;
            let name = &tail[..end];
            let val = std::env::var(name).map_err(|_| {
                anyhow::anyhow!("`{key}`: `${{{name}}}` is not set in the environment")
            })?;
            out.push_str(&val);
            rest = &tail[end + 1..];
        } else {
            out.push('$');
            rest = after;
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// Resolve `.` and `..` without touching the filesystem, so one directory has
/// one spelling. `fs::canonicalize` cannot stand in: a
/// `worktree_root`, or a `[preserve]` destination, is routinely a directory
/// that does not exist yet, and on Windows canonicalizing yields a verbatim
/// `\\?\` prefix that compares unequal to every other spelling of the same
/// path. Symlinks are therefore not followed, and neither is case folded — the
/// result names the path the components spell, not the one the filesystem
/// would resolve. A caller that needs those resolved canonicalizes the deepest
/// ancestor that exists and compares that, keeping this for the paths where
/// canonicalization fails.
///
/// Every caller comparing two paths needs this first. The ports registry
/// compares holder paths as strings, and `Path::starts_with` is a
/// component-prefix test that an unresolved `..` walks straight past — so a
/// containment gate applied to raw paths accepts a path that reaches back
/// inside the tree it was meant to exclude.
///
/// `PathBuf::pop`'s boolean return can't drive this: it pops a trailing `..`
/// just as readily as a real component (`Path::new("..").parent()` is
/// `Some("")`), which would let a second `ParentDir` cancel the first instead
/// of accumulating. Components are tracked explicitly instead: a `ParentDir`
/// pops only when the top of the stack is a real named component; otherwise it
/// is either appended (relative, nothing to pop) or dropped (already at root).
pub fn normalize_lexically(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut stack: Vec<Component> = Vec::new();
    let mut has_root = false;
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => {
                has_root = true;
                stack.push(c);
            }
            Component::ParentDir => match stack.last() {
                Some(Component::Normal(_)) => {
                    stack.pop();
                }
                Some(Component::ParentDir) | None if !has_root => {
                    stack.push(c);
                }
                _ => {}
            },
            Component::Normal(_) => stack.push(c),
        }
    }
    stack.into_iter().collect()
}

/// The directory of the config layer that supplied `key`, from the per-leaf
/// provenance map.
fn layer_dir<'a>(origin: &'a HashMap<String, PathBuf>, key: &str) -> Option<&'a Path> {
    origin.get(key).and_then(|p| p.parent())
}

/// What a `[defaults]` path key names, which decides what a relative value
/// anchors to.
enum PathKind {
    /// A location on this machine. Only the layer that declared it gives the
    /// value meaning, so it anchors to that layer's directory.
    Host,
    /// A file inside the repository being worked on. It anchors to the
    /// checkout reading the config, so each checkout resolves its own copy
    /// regardless of which layer declared the key.
    RepoRelative,
}

/// Expand `${VAR}`, then `~`, then anchor a still-relative path. A `Host` path
/// anchors to the config layer that declared it; a `RepoRelative` path anchors
/// to `checkout_root` when one is known, falling back to the declaring layer
/// otherwise — there is no repository to anchor to outside one. Empty stays
/// empty — an unset optional path must not silently become a directory.
/// Emptiness is checked after expansion: a variable that is set but empty
/// (`export FOO=`) must resolve to `""`, not to `dir.join("")`.
fn resolve_path_key(
    raw: &str,
    key: &str,
    kind: PathKind,
    origin: &HashMap<String, PathBuf>,
    checkout_root: Option<&Path>,
) -> Result<String> {
    let expanded = expand_vars(raw, key)?;
    if expanded.is_empty() {
        return Ok(String::new());
    }
    let p = expand_tilde(&expanded);
    let anchor = match kind {
        PathKind::Host => layer_dir(origin, key),
        PathKind::RepoRelative => checkout_root.or_else(|| layer_dir(origin, key)),
    };
    let joined = match (p.is_absolute(), anchor) {
        (true, _) | (false, None) => p,
        (false, Some(dir)) => dir.join(p),
    };
    Ok(normalize_lexically(&joined).to_string_lossy().into_owned())
}

/// Resolve every `[defaults]` value that carries a path or an environment
/// reference, in place, once, at load time.
fn resolve_defaults(
    cfg: &mut Config,
    origin: &HashMap<String, PathBuf>,
    checkout_root: Option<&Path>,
    default_worktree_root: Option<&Path>,
) -> Result<()> {
    if cfg.defaults.worktree_root.is_empty()
        && let Some(d) = default_worktree_root
    {
        cfg.defaults.worktree_root = d.to_string_lossy().into_owned();
    }
    cfg.defaults.worktree_root = resolve_path_key(
        &cfg.defaults.worktree_root,
        "defaults.worktree_root",
        PathKind::Host,
        origin,
        checkout_root,
    )?;
    // Computed from the now-resolved worktree_root, so a derived value is
    // already absolute before it goes through its own resolve_path_key below.
    if cfg.defaults.baseline_dir.is_empty() && !cfg.defaults.worktree_root.is_empty() {
        cfg.defaults.baseline_dir = Path::new(&cfg.defaults.worktree_root)
            .join("_baselines")
            .to_string_lossy()
            .into_owned();
    }
    for (key, kind, field) in [
        (
            "defaults.baseline_dir",
            PathKind::Host,
            &mut cfg.defaults.baseline_dir,
        ),
        (
            "defaults.doppler_yaml",
            PathKind::RepoRelative,
            &mut cfg.defaults.doppler_yaml,
        ),
    ] {
        *field = resolve_path_key(field, key, kind, origin, checkout_root)?;
    }
    cfg.defaults.branch_prefix =
        expand_vars(&cfg.defaults.branch_prefix, "defaults.branch_prefix")?;
    Ok(())
}

/// Expand `${VAR}` and `~` in a host path key, then anchor a still-relative one
/// to the directory that declared it, the way every other path key resolves.
///
/// Public because the `[harness]` probe reads its table outside the main
/// resolver — the hook cannot afford a full config load — and a path key there
/// must still obey the same rules. Empty stays empty: an unset optional path
/// must not silently become a directory.
pub fn resolve_host_path(raw: &str, key: &str, anchor: Option<&Path>) -> Result<PathBuf> {
    let expanded = expand_vars(raw, key)?;
    if expanded.is_empty() {
        return Ok(PathBuf::new());
    }
    let p = expand_tilde(&expanded);
    let joined = match (p.is_absolute(), anchor) {
        (true, _) | (false, None) => p,
        (false, Some(dir)) => dir.join(p),
    };
    Ok(normalize_lexically(&joined))
}

pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(h) = home_dir()
    {
        return h.join(rest);
    }
    PathBuf::from(p)
}

#[cfg(test)]
pub fn tests_sample() -> &'static str {
    tests::SAMPLE
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;
    pub(crate) const SAMPLE: &str = r#"
[defaults]
worktree_root = "~/git/example"
branch_prefix = "lev/"
baseline_ref = "origin/staging"
baseline_dir = "~/git/example/_baselines"
doppler_yaml = "~/git/example/app/doppler.yaml"
[apps.api]
base_port = 9100
launch = ["doppler", "run", "-c", "dev_local", "--", "nitro", "dev", "--port", "{{ port }}"]
url_env = "FOUNDRY_API_BASE_URL"
static_env = { SUPABASE_JWT_SECRET = "s" }
"#;
    #[test]
    fn worktree_include_parses_and_defaults_empty() {
        let cfg: Config = toml::from_str(
            r#"
            [defaults]
            worktree_root = "/w"
            branch_prefix = "you/"
            baseline_ref = "origin/staging"
            worktree_include = ["apps/*/.env.local", ".tool-versions"]
            [apps]
            "#,
        )
        .unwrap();
        assert_eq!(cfg.defaults.worktree_include, vec![
            "apps/*/.env.local".to_string(),
            ".tool-versions".to_string()
        ]);

        let bare: Config = toml::from_str(
            r#"
            [defaults]
            worktree_root = "/w"
            branch_prefix = "you/"
            baseline_ref = "origin/staging"
            [apps]
            "#,
        )
        .unwrap();
        assert!(bare.defaults.worktree_include.is_empty());
    }
    #[test]
    fn a_defaults_table_with_one_key_deserializes() {
        let cfg: Config = toml::from_str("[defaults]\nbranch_prefix = 'lev/'\n").unwrap();
        assert_eq!(cfg.defaults.branch_prefix, "lev/");
        assert_eq!(cfg.defaults.worktree_root, "");
        assert_eq!(cfg.defaults.baseline_ref, "");
    }
    #[test]
    fn a_config_with_a_section_but_no_defaults_resolves() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("devkit.toml"),
            "[config]\nroot = true\n[github]\nissues_repo = 'a/b'\n",
        )
        .unwrap();
        let (cfg, _) = resolve_with_home(None, dir.path(), None, None, None, None).unwrap();
        assert_eq!(cfg.defaults.worktree_root, "");
    }
    #[test]
    fn a_template_variable_colliding_with_a_context_key_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("devkit.toml"),
            "[config]\nroot = true\n[templates.variables]\nrole = 'x'\n",
        )
        .unwrap();
        let err = resolve_with_home(None, dir.path(), None, None, None, None).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("role"), "{msg}");
    }

    /// Resolve `body` as the only, root config layer.
    fn resolve_root(body: &str) -> Result<Config> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("devkit.toml"),
            format!("[config]\nroot = true\n{body}"),
        )
        .unwrap();
        resolve_with_home(None, dir.path(), None, None, None, None).map(|(cfg, _)| cfg)
    }

    /// Parse `body` under a `[defaults]` table that satisfies validation.
    fn parse_with_defaults(body: &str) -> Config {
        Config::parse(&format!(
            "[defaults]\nworktree_root='w'\nbranch_prefix='x/'\nbaseline_ref='m'\n{body}"
        ))
        .unwrap()
    }

    #[test]
    fn never_without_a_default_is_rejected() {
        let err =
            resolve_root("[templates.variables]\nticket = { required = 'never' }\n").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("ticket"),
            "the error names the variable: {msg}"
        );
        assert!(msg.contains("never"), "the error names the marking: {msg}");
    }

    #[test]
    fn a_task_never_on_an_arg_without_a_default_is_rejected() {
        for variables in ["", "[templates.variables]\nticket = {}\n"] {
            let err = resolve_root(&format!(
                "{variables}[tasks.commit]\nrun = ['git', '{{{{ ticket }}}}']\n\
                 required_args = {{ ticket = 'never' }}\n"
            ))
            .unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains("commit") && msg.contains("ticket") && msg.contains("never"),
                "the error names the task, the arg and the marking: {msg}"
            );
        }
    }

    #[test]
    fn a_task_never_relaxing_a_defaulted_variable_is_accepted() {
        let cfg = resolve_root(
            "[templates.variables]\nmsg = { default = 'wip', required = 'always' }\n\
             [tasks.commit]\nrun = ['git', '{{ msg }}']\nrequired_args = { msg = 'never' }\n",
        )
        .unwrap();
        assert_eq!(cfg.tasks["commit"].required_args["msg"], Required::Never);
    }

    #[test]
    fn a_valueless_entry_with_no_marking_is_accepted() {
        let cfg = resolve_root("[templates.variables]\nticket = {}\n")
            .expect("declaring a name without marking it is how an arg is made passable");
        let decl = &cfg.templates.variables["ticket"];
        assert_eq!(decl.default_value(), None);
        assert_eq!(decl.written_required(), None);
        assert_eq!(decl.required(), Required::Never);
    }

    #[test]
    fn never_with_a_default_is_accepted() {
        let cfg =
            resolve_root("[templates.variables]\nmsg = { default = 'wip', required = 'never' }\n")
                .unwrap();
        assert_eq!(cfg.templates.variables["msg"].default_value(), Some("wip"));
    }

    #[test]
    fn an_ordinary_template_variable_is_still_accepted() {
        let cfg = resolve_root("[templates.variables]\nregion = 'eu'\n").unwrap();
        assert_eq!(
            cfg.templates.variables["region"].default_value(),
            Some("eu")
        );
    }

    #[test]
    fn variable_declarations_parse_in_all_three_forms() {
        let c = parse_with_defaults(
            "[templates.variables]\n\
             team = 'platform'\n\
             msg = { default = 'wip', required = 'agents', description = 'commit subject' }\n\
             ticket = { required = 'always' }\n",
        );
        let v = &c.templates.variables;
        assert_eq!(v["team"].default_value(), Some("platform"));
        assert_eq!(v["team"].required(), Required::Never);
        assert_eq!(v["team"].description(), None);
        assert_eq!(v["msg"].default_value(), Some("wip"));
        assert_eq!(v["msg"].required(), Required::Agents);
        assert_eq!(v["msg"].description(), Some("commit subject"));
        assert_eq!(v["ticket"].default_value(), None);
        assert_eq!(v["ticket"].required(), Required::Always);

        // A plain string must still round-trip as a bare string.
        let out = toml::to_string(&c).unwrap();
        assert!(out.contains("team = \"platform\""), "{out}");
        let c2 = Config::parse(&out).unwrap();
        assert_eq!(c2.templates.variables["msg"].required(), Required::Agents);
        assert_eq!(
            c2.templates.variables["msg"].description(),
            Some("commit subject")
        );
        assert_eq!(c2.templates.variables["ticket"].default_value(), None);
    }

    #[test]
    fn defaults_omits_valueless_entries_and_declared_keeps_them() {
        let c = parse_with_defaults(
            "[templates.variables]\n\
             team = 'platform'\n\
             ticket = { required = 'always' }\n",
        );
        let d = c.templates.defaults();
        assert_eq!(d.get("team").map(String::as_str), Some("platform"));
        assert!(
            !d.contains_key("ticket"),
            "a valueless entry must not reach the render context"
        );
        let declared = c.templates.declared();
        assert!(declared.contains("team") && declared.contains("ticket"));
    }

    #[test]
    fn a_misspelled_key_in_a_variable_table_is_rejected() {
        let s = "[defaults]\nworktree_root='w'\nbranch_prefix='x/'\nbaseline_ref='m'\n\
                 [templates.variables]\n\
                 msg = { deafult = 'wip', required = 'agents' }\n";
        assert!(
            Config::parse(s).is_err(),
            "deny_unknown_fields must catch the typo"
        );
    }

    #[test]
    fn an_unset_worktree_root_takes_the_derived_default() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("devkit.toml"),
            "[config]\nroot = true\n[defaults]\nbranch_prefix = 'lev/'\n",
        )
        .unwrap();
        let derived = dir.path().join("proj_worktrees");
        let (cfg, _) =
            resolve_with_home(None, dir.path(), None, None, None, Some(derived.as_path())).unwrap();
        assert_eq!(
            Path::new(&cfg.defaults.worktree_root),
            normalize_lexically(&derived)
        );
    }
    #[test]
    fn a_set_worktree_root_wins_over_the_derived_default() {
        let dir = tempfile::tempdir().unwrap();
        let explicit = dir.path().join("explicit");
        std::fs::write(
            dir.path().join("devkit.toml"),
            format!(
                "[config]\nroot = true\n[defaults]\nworktree_root = '{}'\n",
                explicit.display()
            ),
        )
        .unwrap();
        let derived = dir.path().join("proj_worktrees");
        let (cfg, _) =
            resolve_with_home(None, dir.path(), None, None, None, Some(derived.as_path())).unwrap();
        assert_eq!(
            Path::new(&cfg.defaults.worktree_root),
            normalize_lexically(&explicit)
        );
    }
    #[test]
    fn a_worktree_root_containing_a_backslash_round_trips() {
        // Backslash is a legal filename character on Linux, so this builds a
        // directory whose name genuinely contains one and checks that a TOML
        // literal string carries it through verbatim: TOML literal strings
        // (single-quoted) take their content as-is, with no escape handling.
        // This discriminates on platforms where a backslash is an ordinary
        // filename character (Linux, macOS); where it is a path separator,
        // `normalize_lexically`'s component walk collapses the doubled and
        // single-backslash forms to the same path, absorbing the distinction.
        let dir = tempfile::tempdir().unwrap();
        let explicit = dir.path().join("a\\b");
        std::fs::write(
            dir.path().join("devkit.toml"),
            format!(
                "[config]\nroot = true\n[defaults]\nworktree_root = '{}'\n",
                explicit.display()
            ),
        )
        .unwrap();
        let (cfg, _) = resolve_with_home(None, dir.path(), None, None, None, None).unwrap();
        assert_eq!(
            Path::new(&cfg.defaults.worktree_root),
            normalize_lexically(&explicit)
        );
    }
    #[test]
    fn baseline_dir_defaults_under_worktree_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("w");
        std::fs::write(
            dir.path().join("devkit.toml"),
            format!(
                "[config]\nroot = true\n[defaults]\nworktree_root = '{}'\n",
                root.display()
            ),
        )
        .unwrap();
        let (cfg, _) = resolve_with_home(None, dir.path(), None, None, None, None).unwrap();
        assert_eq!(
            Path::new(&cfg.defaults.baseline_dir),
            normalize_lexically(&root.join("_baselines"))
        );
    }
    #[test]
    fn an_explicit_baseline_dir_wins_over_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("w");
        let explicit = dir.path().join("elsewhere");
        std::fs::write(
            dir.path().join("devkit.toml"),
            format!(
                "[config]\nroot = true\n[defaults]\nworktree_root = '{}'\nbaseline_dir = '{}'\n",
                root.display(),
                explicit.display()
            ),
        )
        .unwrap();
        let (cfg, _) = resolve_with_home(None, dir.path(), None, None, None, None).unwrap();
        assert_eq!(
            Path::new(&cfg.defaults.baseline_dir),
            normalize_lexically(&explicit)
        );
    }
    #[test]
    fn no_worktree_root_leaves_baseline_dir_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("devkit.toml"),
            "[config]\nroot = true\n[defaults]\n",
        )
        .unwrap();
        let (cfg, _) = resolve_with_home(None, dir.path(), None, None, None, None).unwrap();
        assert_eq!(cfg.defaults.baseline_dir, "");
    }
    #[test]
    fn parses_sample() {
        let c = Config::parse(SAMPLE).unwrap();
        assert_eq!(c.apps["api"].base_port, 9100);
        assert_eq!(
            c.apps["api"].url_env.as_deref(),
            Some("FOUNDRY_API_BASE_URL")
        );
    }
    #[test]
    fn linear_section_parses_and_defaults_off() {
        let c = Config::parse(&format!("{SAMPLE}\n[linear]\nresolve_pr_links = true\n")).unwrap();
        assert!(c.linear.resolve_pr_links);
        let bare = Config::parse(SAMPLE).unwrap();
        assert!(!bare.linear.resolve_pr_links);
    }
    #[test]
    fn github_section_parses_and_defaults_absent() {
        let c = Config::parse(&format!(
            "{SAMPLE}\n[github]\nissues_repo = \"org/planning\"\npr_repo = \"upstream/app\"\n"
        ))
        .unwrap();
        assert_eq!(c.github.issues_repo.as_deref(), Some("org/planning"));
        assert_eq!(c.github.pr_repo.as_deref(), Some("upstream/app"));
        let bare = Config::parse(SAMPLE).unwrap();
        assert_eq!(bare.github.issues_repo, None);
        assert_eq!(bare.github.pr_repo, None);
    }
    #[test]
    fn pr_create_state_defaults_to_draft() {
        let c = Config::parse(SAMPLE).unwrap();
        assert_eq!(c.defaults.pr_create_state, PrCreateState::Draft);
    }
    #[test]
    fn pr_create_state_parses_ready() {
        let src = r#"
[defaults]
worktree_root = "/w"
branch_prefix = "you/"
baseline_ref = "origin/main"
pr_create_state = "ready"
"#;
        let c = Config::parse(src).unwrap();
        assert_eq!(c.defaults.pr_create_state, PrCreateState::Ready);
        assert_eq!(c.defaults.pr_create_state.to_string(), "ready");
    }
    #[test]
    fn an_unknown_pr_create_state_is_an_error() {
        let src = r#"
[defaults]
worktree_root = "/w"
branch_prefix = "you/"
baseline_ref = "origin/main"
pr_create_state = "wip"
"#;
        assert!(Config::parse(src).is_err());
    }
    #[test]
    fn parses_app_setup_commands() {
        let src = format!(
            "{SAMPLE}setup = [[\"doppler\", \"run\", \"-c\", \"local\", \"--\", \"bun\", \"install\"]]\n"
        );
        let c = Config::parse(&src).unwrap();
        assert_eq!(c.apps["api"].setup, vec![vec![
            "doppler".to_string(),
            "run".to_string(),
            "-c".to_string(),
            "local".to_string(),
            "--".to_string(),
            "bun".to_string(),
            "install".to_string(),
        ]]);
    }
    #[test]
    fn setup_defaults_empty() {
        let c = Config::parse(SAMPLE).unwrap();
        assert!(c.apps["api"].setup.is_empty());
    }
    #[test]
    fn stray_scan_width_defaults_to_64() {
        let c = Config::parse(SAMPLE).unwrap();
        assert_eq!(c.defaults.stray_scan_width, 64);
    }
    #[test]
    fn stray_scan_width_parses_override() {
        let src = r#"
[defaults]
worktree_root = "~/git/example"
branch_prefix = "lev/"
baseline_ref = "origin/staging"
doppler_yaml = "~/git/example/app/doppler.yaml"
stray_scan_width = 128
[apps.api]
base_port = 9100
launch = ["nitro", "dev", "--port", "{{ port }}"]
"#;
        let c = Config::parse(src).unwrap();
        assert_eq!(c.defaults.stray_scan_width, 128);
    }
    #[test]
    fn parses_people_and_pr_base() {
        let src = r#"
[defaults]
worktree_root = "~/git/example"
branch_prefix = "lev/"
baseline_ref = "origin/staging"
doppler_yaml = "~/git/example/app/doppler.yaml"
pr_base = "staging"
[apps.api]
base_port = 9100
launch = ["nitro", "dev", "--port", "{{ port }}"]
[people.igor]
slack = "U0XXXXXXXXX"
github = "exampleuser"
"#;
        let c = Config::parse(src).unwrap();
        assert_eq!(c.defaults.pr_base, "staging");
        let igor = c.people.get("igor").unwrap();
        assert_eq!(igor.slack, "U0XXXXXXXXX");
        assert_eq!(igor.github.as_deref(), Some("exampleuser"));
    }
    #[test]
    fn pr_base_defaults_to_main() {
        let cfg: Config = toml::from_str("[defaults]\n").unwrap();
        assert_eq!(cfg.defaults.pr_base, "main");
    }
    #[test]
    fn ignored_checks_parse_and_default() {
        assert!(
            Config::parse(SAMPLE)
                .unwrap()
                .defaults
                .ignored_checks
                .is_empty()
        );
        let src = r#"
[defaults]
worktree_root = "~/git/example"
branch_prefix = "lev/"
baseline_ref = "origin/staging"
ignored_checks = ["vercel*", "*Preview*"]
[apps.api]
base_port = 9100
launch = ["nitro", "dev", "--port", "{{ port }}"]
"#;
        let c = Config::parse(src).unwrap();
        assert_eq!(c.defaults.ignored_checks, vec!["vercel*", "*Preview*"]);
    }
    #[test]
    fn require_pr_reviewer_parses_and_defaults_off() {
        assert!(!Config::parse(SAMPLE).unwrap().defaults.require_pr_reviewer);
        let src = r#"
[defaults]
worktree_root = "~/git/example"
branch_prefix = "lev/"
baseline_ref = "origin/staging"
require_pr_reviewer = true
[apps.api]
base_port = 9100
launch = ["nitro", "dev", "--port", "{{ port }}"]
"#;
        assert!(Config::parse(src).unwrap().defaults.require_pr_reviewer);
    }
    #[test]
    fn doppler_yaml_optional() {
        let without = SAMPLE
            .lines()
            .filter(|l| !l.trim_start().starts_with("doppler_yaml"))
            .collect::<Vec<_>>()
            .join("\n");
        let c = Config::parse(&without).unwrap();
        assert_eq!(c.defaults.doppler_yaml, "");
    }
    #[test]
    fn daemon_defaults_when_absent() {
        let c = Config::parse(SAMPLE).unwrap();
        assert!(!c.daemon.enabled);
        assert_eq!(c.daemon.idle_timeout_secs, 1800);
        assert_eq!(c.daemon.max_restarts, 5);
        assert_eq!(c.daemon.restart_window_secs, 60);
        assert_eq!(c.daemon.memory_warn_mb, 0);
        assert_eq!(c.daemon.memory_limit_mb, 0);
        assert_eq!(c.daemon.memory_action, "warn");
        assert_eq!(c.daemon.health_probe_secs, 0);
        assert_eq!(c.daemon.health_fail_threshold, 3);
        assert_eq!(c.daemon.memory_limit_ticks, 3);
        assert_eq!(c.daemon.memory_max_mb, 0);
    }
    #[test]
    fn parses_explicit_daemon_block() {
        let src = format!(
            "{SAMPLE}\n[daemon]\nenabled = true\nidle_timeout_secs = 600\nmemory_warn_mb = 6000\n"
        );
        let c = Config::parse(&src).unwrap();
        assert!(c.daemon.enabled);
        assert_eq!(c.daemon.idle_timeout_secs, 600);
        assert_eq!(c.daemon.memory_warn_mb, 6000);
        assert_eq!(c.daemon.max_restarts, 5); // untouched field keeps its default
    }

    #[test]
    fn config_roundtrips_through_toml_serialization() {
        let c = Config::parse(SAMPLE).unwrap();
        let s = toml::to_string_pretty(&c).expect("serialize config to toml");
        let c2 = Config::parse(&s).expect("reparse serialized config");
        assert_eq!(c2.apps["api"].base_port, 9100);
        assert_eq!(c2.defaults.branch_prefix, "lev/");
    }

    #[test]
    fn task_split_roundtrips_and_rejects_unknown_fields() {
        let text = r#"[tasks.stage]
run = ["git", "add", "--", { split = "{{ files }}", on = ";" }]
"#;
        let config: Config = toml::from_str(text).unwrap();
        let serialized = toml::to_string(&config).unwrap();
        let roundtrip: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(roundtrip.tasks["stage"].run, config.tasks["stage"].run);
        assert_eq!(
            config.tasks["stage"].run[3],
            RunArg::Action(RunAction::Split {
                split: "{{ files }}".into(),
                on: ";".into()
            })
        );
        assert_eq!(config.tasks["stage"].run[3].template(), "{{ files }}");
        assert!(toml::from_str::<Config>(&text.replace("split =", "typo =")).is_err());
        assert!(toml::from_str::<Config>(&text.replace(", on =", ", typo =")).is_err());
        assert!(toml::from_str::<Config>(&text.replace("{ split", "{ typo = 1, split")).is_err());
    }

    #[test]
    fn roundtrips_app_with_static_env_and_prep_files() {
        let src = format!(
            "{SAMPLE}\n\
[[apps.api.prep_files]]\n\
path = \".env.local\"\n\
content = \"FOO=bar\\n\"\n\
overwrite = true\n\
\n\
[[apps.api.prep_files]]\n\
path = \"config/extra.toml\"\n\
content = \"key = 1\\n\"\n"
        );
        let c = Config::parse(&src).unwrap();
        let s = toml::to_string(&c).expect("serialize app with static_env and prep_files");
        let c2 = Config::parse(&s).expect("reparse serialized config");

        let a1 = &c.apps["api"];
        let a2 = &c2.apps["api"];
        assert_eq!(a2.static_env, a1.static_env);
        assert_eq!(a2.prep_files.len(), 2);
        assert_eq!(a2.prep_files.len(), a1.prep_files.len());
        for (p1, p2) in a1.prep_files.iter().zip(a2.prep_files.iter()) {
            assert_eq!(p2.path, p1.path);
            assert_eq!(p2.content, p1.content);
            assert_eq!(p2.overwrite, p1.overwrite);
        }
        assert!(a2.prep_files[0].overwrite);
        assert!(!a2.prep_files[1].overwrite);
    }

    #[test]
    fn config_serializes_app_with_static_env_and_trailing_scalars() {
        // An app carrying both a map field (static_env) and scalar/array fields
        // (setup, path) serializes cleanly with all keys present.
        let src = format!("{SAMPLE}setup = [[\"bun\", \"install\"]]\npath = \"apps/api\"\n");
        let c = Config::parse(&src).unwrap();
        let s = toml::to_string_pretty(&c).expect("serialize app with trailing scalars");
        assert!(s.contains("setup"));
        assert!(s.contains("path"));
    }

    fn tbl(s: &str) -> toml::Table {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn deeper_layer_overrides_scalar_keeps_others() {
        let base = tbl("[defaults]\nworktree_root='/a'\nbranch_prefix='x/'\n");
        let top = tbl("[defaults]\nbranch_prefix='y/'\n");
        let (m, origin, shadowed) =
            merge_layers(&[(PathBuf::from("/base"), base), (PathBuf::from("/top"), top)]);
        assert_eq!(m["defaults"]["branch_prefix"].as_str(), Some("y/"));
        assert_eq!(m["defaults"]["worktree_root"].as_str(), Some("/a"));
        assert_eq!(origin["defaults.branch_prefix"], PathBuf::from("/top"));
        assert_eq!(origin["defaults.worktree_root"], PathBuf::from("/base"));
        // the overridden value is kept alongside the layer that held it
        assert_eq!(shadowed["defaults.branch_prefix"], vec![Shadow {
            file: PathBuf::from("/base"),
            value: toml::Value::String("x/".into()),
        }]);
        // a leaf only one layer sets is not recorded as shadowing anything
        assert!(!shadowed.contains_key("defaults.worktree_root"));
    }

    #[test]
    fn arrays_replace_wholesale() {
        let base = tbl("[apps.api]\nlaunch=['a','b']\n");
        let top = tbl("[apps.api]\nlaunch=['c']\n");
        let (m, origin, _) =
            merge_layers(&[(PathBuf::from("/b"), base), (PathBuf::from("/t"), top)]);
        let launch = m["apps"]["api"]["launch"].as_array().unwrap();
        assert_eq!(launch.len(), 1);
        assert_eq!(launch[0].as_str(), Some("c"));
        assert_eq!(origin["apps.api.launch"], PathBuf::from("/t"));
    }

    #[test]
    fn nested_maps_merge_per_key() {
        let base = tbl("[apps.api.static_env]\nA='1'\nB='2'\n");
        let top = tbl("[apps.api.static_env]\nB='9'\nC='3'\n");
        let (m, origin, _) =
            merge_layers(&[(PathBuf::from("/b"), base), (PathBuf::from("/t"), top)]);
        let se = &m["apps"]["api"]["static_env"];
        assert_eq!(se["A"].as_str(), Some("1"));
        assert_eq!(se["B"].as_str(), Some("9"));
        assert_eq!(se["C"].as_str(), Some("3"));
        assert_eq!(origin["apps.api.static_env.B"], PathBuf::from("/t"));
        assert_eq!(origin["apps.api.static_env.A"], PathBuf::from("/b"));
    }

    /// An absolute path, spelled the way the host spells one.
    /// `resolve_path_key` anchors anything `Path::is_absolute` rejects to
    /// its declaring layer, and a leading `/` is not absolute on Windows,
    /// so a fixture that spelled one there would move with the layer
    /// instead of standing still.
    #[cfg(not(windows))]
    const ABS_W: &str = "/w";
    #[cfg(windows)]
    const ABS_W: &str = "C:\\w";

    /// Every `[defaults]` key a `Config` requires, with `ABS_W` as the worktree
    /// root so the layering tests read back what they wrote.
    #[cfg(not(windows))]
    const FULL_DEFAULTS: &str = "worktree_root='/w'\nbranch_prefix='x/'\nbaseline_ref='r'\n";
    #[cfg(windows)]
    const FULL_DEFAULTS: &str = "worktree_root='C:\\w'\nbranch_prefix='x/'\nbaseline_ref='r'\n";

    #[test]
    fn resolve_merges_parent_and_child() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("repo");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(
            root.path().join("devkit.toml"),
            format!("[defaults]\n{FULL_DEFAULTS}[apps.api]\nbase_port=1\nlaunch=['a']\n"),
        )
        .unwrap();
        std::fs::write(
            child.join("devkit.toml"),
            "[defaults]\nbranch_prefix='y/'\n[apps.api]\nbase_port=2\n",
        )
        .unwrap();
        let (cfg, prov) = resolve_with_home(None, &child, None, None, None, None).unwrap();
        assert_eq!(cfg.defaults.branch_prefix, "y/"); // child overrides
        assert_eq!(cfg.defaults.worktree_root, ABS_W); // inherited from parent
        assert_eq!(cfg.apps["api"].base_port, 2); // child overrides
        assert_eq!(cfg.apps["api"].launch, vec!["a".to_string()]); // inherited
        assert_eq!(prov.layers.len(), 2);
        assert_eq!(
            prov.origin["defaults.branch_prefix"],
            child.join("devkit.toml")
        );
    }

    /// A linked worktree is a sibling of its main checkout, not a descendant,
    /// so the upward walk from the worktree never reaches the main
    /// checkout's own devkit.toml. Passing `main_checkout` is what lets a
    /// value declared only there survive into the merged config.
    #[test]
    fn resolve_layers_in_the_main_checkouts_config() {
        let main = tempfile::tempdir().unwrap();
        std::fs::write(
            main.path().join("devkit.toml"),
            format!("[defaults]\n{FULL_DEFAULTS}"),
        )
        .unwrap();

        let worktree = tempfile::tempdir().unwrap();

        let (cfg, _) =
            resolve_with_home(None, worktree.path(), Some(main.path()), None, None, None).unwrap();
        assert_eq!(cfg.defaults.worktree_root, ABS_W);
    }

    #[test]
    fn root_marker_stops_walk() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("repo");
        std::fs::create_dir_all(&child).unwrap();
        let home = root.path().join("home.toml");
        std::fs::write(&home, "[defaults]\nbranch_prefix='HOME/'\n").unwrap();
        std::fs::write(
            root.path().join("devkit.toml"),
            "[defaults]\nworktree_root='/PARENT'\n",
        )
        .unwrap();
        std::fs::write(
            child.join("devkit.toml"),
            format!("[config]\nroot=true\n[defaults]\n{FULL_DEFAULTS}[apps.api]\nbase_port=2\nlaunch=['a']\n"),
        )
        .unwrap();
        let (cfg, prov) = resolve_with_home(None, &child, None, None, Some(&home), None).unwrap();
        assert_eq!(cfg.defaults.worktree_root, ABS_W); // parent's /PARENT dropped
        assert_eq!(cfg.defaults.branch_prefix, "x/"); // home's HOME/ dropped
        assert_eq!(prov.layers, vec![child.join("devkit.toml")]);
    }

    #[test]
    fn home_layer_is_lowest_precedence() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let home = root.path().join("home.toml");
        std::fs::write(
            &home,
            "[defaults]\nbranch_prefix='HOME/'\nworktree_root='/hw'\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("devkit.toml"),
            format!("[defaults]\n{FULL_DEFAULTS}[apps.api]\nbase_port=2\nlaunch=['a']\n"),
        )
        .unwrap();
        let (cfg, prov) = resolve_with_home(None, &repo, None, None, Some(&home), None).unwrap();
        assert_eq!(cfg.defaults.branch_prefix, "x/"); // repo wins over home
        assert_eq!(
            prov.origin["defaults.branch_prefix"],
            repo.join("devkit.toml")
        );
        // a field only the home layer sets still resolves, attributed to home
        assert_eq!(prov.layers.first(), Some(&home));
    }

    #[test]
    fn local_layer_overrides_the_tracked_file_beside_it() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(
            repo.path().join("devkit.toml"),
            format!("[defaults]\n{FULL_DEFAULTS}[apps.api]\nbase_port=1\nlaunch=['a']\n"),
        )
        .unwrap();
        std::fs::write(
            repo.path().join("devkit.local.toml"),
            "[defaults]\nbranch_prefix='local/'\n",
        )
        .unwrap();
        let (cfg, prov) = resolve_with_home(None, repo.path(), None, None, None, None).unwrap();
        assert_eq!(cfg.defaults.branch_prefix, "local/");
        assert_eq!(cfg.defaults.worktree_root, ABS_W); // tracked layer still merges
        assert_eq!(
            prov.origin["defaults.branch_prefix"],
            repo.path().join("devkit.local.toml")
        );
    }

    #[test]
    fn deeper_tracked_layer_beats_a_shallower_local_layer() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("repo");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(
            root.path().join("devkit.toml"),
            format!("[defaults]\n{FULL_DEFAULTS}"),
        )
        .unwrap();
        std::fs::write(
            root.path().join("devkit.local.toml"),
            "[defaults]\nbranch_prefix='shallow-local/'\n",
        )
        .unwrap();
        std::fs::write(
            child.join("devkit.toml"),
            "[defaults]\nbranch_prefix='deep/'\n",
        )
        .unwrap();
        let (cfg, _) = resolve_with_home(None, &child, None, None, None, None).unwrap();
        assert_eq!(cfg.defaults.branch_prefix, "deep/");
    }

    #[test]
    fn a_local_layer_alone_resolves() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(
            repo.path().join("devkit.local.toml"),
            format!("[defaults]\n{FULL_DEFAULTS}"),
        )
        .unwrap();
        let (cfg, prov) = resolve_with_home(None, repo.path(), None, None, None, None).unwrap();
        assert_eq!(cfg.defaults.worktree_root, ABS_W);
        assert_eq!(prov.layers, vec![repo.path().join("devkit.local.toml")]);
    }

    #[test]
    fn root_marker_in_a_local_layer_stops_walk() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("repo");
        std::fs::create_dir_all(&child).unwrap();
        let home = root.path().join("home.toml");
        std::fs::write(&home, "[defaults]\nbranch_prefix='HOME/'\n").unwrap();
        std::fs::write(
            root.path().join("devkit.toml"),
            "[defaults]\nworktree_root='/PARENT'\n",
        )
        .unwrap();
        std::fs::write(
            child.join("devkit.toml"),
            format!("[defaults]\n{FULL_DEFAULTS}"),
        )
        .unwrap();
        std::fs::write(child.join("devkit.local.toml"), "[config]\nroot=true\n").unwrap();
        let (cfg, prov) = resolve_with_home(None, &child, None, None, Some(&home), None).unwrap();
        assert_eq!(cfg.defaults.worktree_root, ABS_W); // parent's /PARENT dropped
        assert_eq!(cfg.defaults.branch_prefix, "x/"); // home's HOME/ dropped
        assert_eq!(prov.layers, vec![
            child.join("devkit.toml"),
            child.join("devkit.local.toml")
        ]);
    }

    #[test]
    fn explicit_config_bypasses_layering() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("repo");
        std::fs::create_dir_all(&child).unwrap();
        let explicit = root.path().join("custom.toml");
        std::fs::write(
            &explicit,
            format!("[defaults]\n{FULL_DEFAULTS}[apps.api]\nbase_port=7\nlaunch=['a']\n"),
        )
        .unwrap();
        std::fs::write(
            child.join("devkit.toml"),
            "[defaults]\nbranch_prefix='IGNORED/'\n",
        )
        .unwrap();
        let (cfg, prov) =
            resolve_with_home(Some(&explicit), &child, None, None, None, None).unwrap();
        assert_eq!(cfg.apps["api"].base_port, 7);
        assert_eq!(cfg.defaults.branch_prefix, "x/"); // child file not consulted
        assert_eq!(prov.layers, vec![explicit]);
    }

    #[test]
    fn resolve_errors_when_no_config_found() {
        let root = tempfile::tempdir().unwrap();
        let err = resolve_with_home(None, root.path(), None, None, None, None).unwrap_err();
        assert!(err.to_string().contains("no devkit.toml"));
    }

    #[test]
    fn parses_prep_files_with_overwrite_default() {
        let toml = r#"
[defaults]
worktree_root = "~/wt"
branch_prefix = "x/"
baseline_ref = "origin/main"

[apps.api]
base_port = 9100
launch = ["nitro", "dev"]

[[apps.api.prep_files]]
path = ".env.local"
content = "A=1\n"

[[apps.api.prep_files]]
path = "config/local.json"
content = "{}\n"
overwrite = true
"#;
        let c = Config::parse(toml).unwrap();
        let pf = &c.apps["api"].prep_files;
        assert_eq!(pf.len(), 2);
        assert_eq!(pf[0].path, ".env.local");
        assert_eq!(pf[0].content, "A=1\n");
        assert!(!pf[0].overwrite); // default false
        assert!(pf[1].overwrite);
    }

    #[test]
    fn flatten_yields_sorted_dotted_leaves() {
        let v: toml::Value = toml::from_str("[a]\nx=1\n[a.b]\ny='z'\nlist=['p','q']\n").unwrap();
        let mut out = Vec::new();
        flatten(&v, "", &mut out);
        let paths: Vec<&str> = out.iter().map(|(p, _)| p.as_str()).collect();
        assert!(paths.contains(&"a.x"));
        assert!(paths.contains(&"a.b.y"));
        // arrays are single leaves, not flattened element-by-element
        assert!(paths.contains(&"a.b.list"));
        assert!(!paths.iter().any(|p| p.starts_with("a.b.list.")));
    }

    #[test]
    fn templates_default_when_absent() {
        let t: Templates = toml::from_str("").unwrap();
        assert!(t.branch.is_none());
        assert!(t.variables.is_empty());
        assert_eq!(t.branch(), DEFAULT_BRANCH);
        assert_eq!(t.worktree_dir(), DEFAULT_WORKTREE_DIR);
        assert_eq!(t.pr_title(), DEFAULT_PR_TITLE);
        assert_eq!(t.pr_body(), DEFAULT_PR_BODY);
        assert_eq!(t.review_request(), DEFAULT_REVIEW_REQUEST);
        assert_eq!(t.review_finish(), DEFAULT_REVIEW_FINISH);
    }

    #[test]
    fn templates_partial_override() {
        let t: Templates = toml::from_str("branch = \"{{ slug }}\"\n").unwrap();
        assert_eq!(t.branch(), "{{ slug }}");
        assert_eq!(t.worktree_dir(), DEFAULT_WORKTREE_DIR);
    }

    #[test]
    fn templates_variables_parse() {
        let t: Templates = toml::from_str("[variables]\nteam = \"platform\"\n").unwrap();
        assert_eq!(
            t.variables
                .get("team")
                .and_then(VariableDecl::default_value),
            Some("platform")
        );
    }

    #[test]
    fn config_has_default_templates() {
        let cfg = Config::parse(tests_sample()).unwrap();
        assert_eq!(cfg.templates.branch(), DEFAULT_BRANCH);
    }

    #[test]
    fn default_checkout_worktree_dir_template() {
        let t = Templates::default();
        assert_eq!(t.checkout_worktree_dir(), DEFAULT_CHECKOUT_WORKTREE_DIR);
        assert!(t.checkout_worktree_dir().contains("pr_number"));
        assert!(t.checkout_worktree_dir().contains("linear_id"));
    }

    #[test]
    fn default_length_limits() {
        let t: Templates = toml::from_str("").unwrap();
        assert_eq!(t.branch_max(), 46);
        assert_eq!(t.worktree_dir_max(), 24);
        assert_eq!(t.checkout_worktree_dir_max(), 46);
    }

    #[test]
    fn length_limit_overrides_win() {
        let t: Templates = toml::from_str(
            "branch_max = 60\nworktree_dir_max = 18\ncheckout_worktree_dir_max = 30\n",
        )
        .unwrap();
        assert_eq!(t.branch_max(), 60);
        assert_eq!(t.worktree_dir_max(), 18);
        assert_eq!(t.checkout_worktree_dir_max(), 30);
    }

    #[test]
    fn checkout_worktree_dir_override_wins() {
        let t: Templates = toml::from_str("checkout_worktree_dir = \"{{ pr_number }}\"\n").unwrap();
        assert_eq!(t.checkout_worktree_dir(), "{{ pr_number }}");
    }

    #[test]
    fn tasks_parse_command_and_sequence() {
        let src = r#"
[defaults]
worktree_root = "wts"
branch_prefix = "x/"
baseline_ref = "origin/main"

[tasks.api-prod-build]
description = "prod nitro build"
app = "api-prod"
run = ["doppler", "run", "-c", "dev_local", "--", "bun", "nitro", "build"]
env = { NITRO_PRESET = "node-server" }

[tasks.profile-lab-os]
steps = [
  { task = "api-prod-build" },
  { up = "api-prod" },
]
"#;
        let c = Config::parse(src).unwrap();
        let t = &c.tasks["api-prod-build"];
        assert_eq!(t.app.as_deref(), Some("api-prod"));
        assert_eq!(t.run[0], RunArg::Scalar("doppler".into()));
        assert_eq!(t.env["NITRO_PRESET"], "node-server");
        assert!(t.steps.is_empty());
        let s = &c.tasks["profile-lab-os"];
        assert_eq!(s.steps, vec![
            Step::Task("api-prod-build".to_string()),
            Step::Up("api-prod".to_string())
        ]);
        assert!(s.run.is_empty());
    }

    #[test]
    fn tasks_roundtrip_through_toml() {
        let src = "[defaults]\nworktree_root = \"w\"\nbranch_prefix = \"x/\"\n\
                   baseline_ref = \"m\"\n\
                   [tasks.t]\nrun = [\"git\", \"version\"]\n\
                   [tasks.s]\nsteps = [{ task = \"t\" }, { up = \"api\" }]\n";
        let c = Config::parse(src).unwrap();
        let out = toml::to_string(&c).expect("serialize config with tasks");
        let c2 = Config::parse(&out).unwrap();
        assert_eq!(c2.tasks["s"].steps, c.tasks["s"].steps);
        assert_eq!(c2.tasks["t"].run, c.tasks["t"].run);
    }

    #[test]
    fn tasks_absent_is_empty() {
        let c = Config::parse(tests_sample()).unwrap();
        assert!(c.tasks.is_empty());
    }

    #[test]
    fn tasks_merge_across_layers() {
        let base = tbl("[tasks.build]\nrun = ['git', 'version']\n[tasks.build.env]\nA = '1'\n");
        let top = tbl("[tasks.build.env]\nA = '9'\nB = '2'\n");
        let (m, ..) = merge_layers(&[(PathBuf::from("/b"), base), (PathBuf::from("/t"), top)]);
        let t = &m["tasks"]["build"];
        assert_eq!(t["run"][0].as_str(), Some("git"));
        assert_eq!(t["env"]["A"].as_str(), Some("9"));
        assert_eq!(t["env"]["B"].as_str(), Some("2"));
    }

    #[test]
    fn brief_defaults_on_and_the_project_layer_wins() {
        // The home layer supplies [defaults] so this fixture exercises [brief]
        // merging across two layers rather than the standalone-section path.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home.toml");
        std::fs::write(
            &home,
            format!("[defaults]\n{FULL_DEFAULTS}[brief]\npins = true\n"),
        )
        .unwrap();
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("devkit.toml"), "[brief]\npins = false\n").unwrap();

        let (cfg, _) = resolve_with_home(None, &project, None, None, Some(&home), None).unwrap();
        assert!(cfg.brief.enabled, "enabled defaults on");
        assert!(!cfg.brief.pins, "the project layer wins");

        // A config with no [brief] table at all gets every default.
        let bare = tmp.path().join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::write(
            bare.join("devkit.toml"),
            format!("[defaults]\n{FULL_DEFAULTS}"),
        )
        .unwrap();
        let (cfg, _) = resolve_with_home(None, &bare, None, None, None, None).unwrap();
        assert!(cfg.brief.enabled);
        assert!(cfg.brief.pins);
        assert!(cfg.brief.locks);
        assert!(cfg.brief.apps);
        assert!(cfg.brief.tasks);
    }

    #[test]
    fn a_config_of_standalone_sections_needs_no_defaults() {
        // `[harness]`, `[docs]`, `[brief]`, and `[config]` are read without any
        // path or branch convention — this repository's own devkit.toml is
        // `[harness]` alone, and a `[docs]`-only overlay is the documented way
        // to register a library for one project.
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("harness-only");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("devkit.toml"),
            "[config]\nroot = true\n[harness]\nenforce_writes = true\n[parallelism]\nthreads = 2\n",
        )
        .unwrap();

        let (cfg, _) = resolve_with_home(None, &project, None, None, None, None).unwrap();
        assert_eq!(cfg.defaults.worktree_root, "");
        assert_eq!(cfg.defaults.apps_dir, "apps");
        assert_eq!(cfg.defaults.pr_base, "main");
        assert_eq!(cfg.defaults.stray_scan_width, 64);
        assert_eq!(health_with_home(&project, None, None, None), Health::Ok);
    }

    #[test]
    fn health_tells_an_absent_config_from_a_broken_one() {
        let tmp = tempfile::tempdir().unwrap();

        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(health_with_home(&empty, None, None, None), Health::Absent);

        let good = tmp.path().join("good");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::write(
            good.join("devkit.toml"),
            format!("[config]\nroot = true\n[defaults]\n{FULL_DEFAULTS}"),
        )
        .unwrap();
        assert_eq!(health_with_home(&good, None, None, None), Health::Ok);

        // A required app key left out: the exact fault a user hits by adding
        // an app entry by hand.
        let missing_key = tmp.path().join("missing-key");
        std::fs::create_dir_all(&missing_key).unwrap();
        std::fs::write(
            missing_key.join("devkit.toml"),
            format!("[config]\nroot = true\n[defaults]\n{FULL_DEFAULTS}[apps.foobar]\nlaunch = ['echo']\n"),
        )
        .unwrap();
        let Health::Broken(msg) = health_with_home(&missing_key, None, None, None) else {
            panic!("a config that does not deserialize is Broken");
        };
        assert!(msg.contains("base_port"), "{msg}");
        assert!(msg.contains("apps.foobar"), "{msg}");

        let unparseable = tmp.path().join("unparseable");
        std::fs::create_dir_all(&unparseable).unwrap();
        std::fs::write(unparseable.join("devkit.toml"), "this is not toml [[[").unwrap();
        let Health::Broken(msg) = health_with_home(&unparseable, None, None, None) else {
            panic!("a config that does not parse is Broken");
        };
        assert!(msg.contains("parsing config layer"), "{msg}");
    }

    #[test]
    fn brief_sections_can_be_turned_off_per_project() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("devkit.toml"),
            format!(
                "[defaults]\n{FULL_DEFAULTS}[brief]\nlocks = false\napps = false\ntasks = false\n"
            ),
        )
        .unwrap();

        let (cfg, _) = resolve_with_home(None, &project, None, None, None, None).unwrap();
        assert!(!cfg.brief.locks);
        assert!(!cfg.brief.apps);
        assert!(!cfg.brief.tasks);
        assert!(cfg.brief.enabled, "the other keys keep their defaults");
        assert!(cfg.brief.pins);
    }

    #[test]
    fn brief_merges_per_key_not_per_section() {
        // Home sets `enabled` only; the project layer sets `pins` only. If the
        // project's [brief] table replaced home's wholesale, `enabled` would
        // fall back to the type default (true) instead of home's `false`.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home.toml");
        std::fs::write(
            &home,
            format!("[defaults]\n{FULL_DEFAULTS}[brief]\nenabled = false\n"),
        )
        .unwrap();
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("devkit.toml"), "[brief]\npins = false\n").unwrap();

        let (cfg, _) = resolve_with_home(None, &project, None, None, Some(&home), None).unwrap();
        assert!(!cfg.brief.enabled, "the home-layer key survives");
        assert!(!cfg.brief.pins, "the project-layer key overrides");
    }

    #[test]
    fn task_require_live_roundtrips() {
        let s = "[defaults]\nworktree_root='w'\nbranch_prefix='x/'\nbaseline_ref='m'\n[tasks.build]\nrun=['git']\nrequire_live=['api']\n";
        let c = Config::parse(s).unwrap();
        assert_eq!(c.tasks["build"].require_live, vec!["api"]);
        let out = toml::to_string(&c).unwrap();
        let c2 = Config::parse(&out).unwrap();
        assert_eq!(c2.tasks["build"].require_live, vec!["api"]);
    }

    #[test]
    fn expand_vars_substitutes_a_set_variable() {
        unsafe { std::env::set_var("DEVKIT_TEST_ROOT", "/srv/work") };
        let got = expand_vars("${DEVKIT_TEST_ROOT}/trees", "defaults.worktree_root").unwrap();
        assert_eq!(got, "/srv/work/trees");
    }

    #[test]
    fn expand_vars_errors_naming_the_key_and_the_variable() {
        let err = expand_vars("${DEVKIT_TEST_ABSENT}/x", "defaults.worktree_root")
            .expect_err("an unset variable must be an error");
        let msg = err.to_string();
        assert!(
            msg.contains("defaults.worktree_root"),
            "message names the key: {msg}"
        );
        assert!(
            msg.contains("DEVKIT_TEST_ABSENT"),
            "message names the variable: {msg}"
        );
    }

    #[test]
    fn expand_vars_treats_double_dollar_as_a_literal() {
        let got = expand_vars("/opt/$${NOT_A_VAR}/x", "defaults.baseline_dir").unwrap();
        assert_eq!(got, "/opt/${NOT_A_VAR}/x");
    }

    #[test]
    fn expand_vars_passes_a_bare_dollar_through() {
        // A `$` not followed by `{` or `$` is a legal path character, so it
        // stays.
        let got = expand_vars("/opt/a$b/c", "defaults.baseline_dir").unwrap();
        assert_eq!(got, "/opt/a$b/c");
    }

    #[test]
    fn expand_vars_errors_on_an_unterminated_brace() {
        let err = expand_vars("${OPEN", "defaults.worktree_root").expect_err("unterminated");
        assert!(err.to_string().contains("unterminated"), "{err}");
    }

    #[test]
    fn expand_vars_leaves_a_plain_value_alone() {
        let got = expand_vars("~/git/example", "defaults.worktree_root").unwrap();
        assert_eq!(got, "~/git/example");
    }

    /// Write `body` to `<dir>/devkit.toml` and return the file's path.
    fn write_cfg(dir: &Path, body: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join("devkit.toml");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn normalize_lexically_drops_dot_and_pops_dotdot() {
        assert_eq!(
            normalize_lexically(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
        assert_eq!(
            normalize_lexically(Path::new("/a/../..")),
            PathBuf::from("/")
        );
        assert_eq!(
            normalize_lexically(Path::new("/a/../../..")),
            PathBuf::from("/")
        );
    }

    #[test]
    fn normalize_lexically_accumulates_leading_dotdot_in_a_relative_path() {
        // `PathBuf::pop()`'s boolean can't drive this: it happily pops a
        // trailing `..` (`Path::new("..").parent()` is `Some("")`), so a naive
        // implementation lets the second `ParentDir` cancel the first instead
        // of appending another.
        assert_eq!(normalize_lexically(Path::new("..")), PathBuf::from(".."));
        assert_eq!(
            normalize_lexically(Path::new("../..")),
            PathBuf::from("../..")
        );
        assert_eq!(
            normalize_lexically(Path::new("../a/../..")),
            PathBuf::from("../..")
        );
        assert_eq!(
            normalize_lexically(Path::new("a/../../b")),
            PathBuf::from("../b")
        );
    }

    #[test]
    fn a_relative_path_resolves_against_its_declaring_layer() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        write_cfg(
            &proj,
            "[defaults]\n\
             worktree_root = \"../proj-worktrees\"\n\
             branch_prefix = \"lev/\"\n\
             baseline_ref = \"origin/main\"\n\
             baseline_dir = \"../proj-worktrees/_baseline\"\n",
        );
        let (cfg, _) = resolve_with_home(None, &proj, None, None, None, None).unwrap();
        assert_eq!(
            cfg.defaults.worktree_root,
            tmp.path().join("proj-worktrees").to_string_lossy()
        );
        assert_eq!(
            cfg.defaults.baseline_dir,
            tmp.path()
                .join("proj-worktrees")
                .join("_baseline")
                .to_string_lossy()
        );
    }

    #[test]
    fn the_same_relative_path_resolves_alike_from_two_start_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        let nested = proj.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        write_cfg(
            &proj,
            "[defaults]\n\
             worktree_root = \"../proj-worktrees\"\n\
             branch_prefix = \"lev/\"\n\
             baseline_ref = \"origin/main\"\n",
        );
        let (from_root, _) = resolve_with_home(None, &proj, None, None, None, None).unwrap();
        let (from_nested, _) = resolve_with_home(None, &nested, None, None, None, None).unwrap();
        assert_eq!(
            from_root.defaults.worktree_root,
            from_nested.defaults.worktree_root
        );
    }

    #[test]
    fn an_absolute_path_and_a_tilde_path_are_left_absolute() {
        let tmp = tempfile::tempdir().unwrap();
        write_cfg(
            tmp.path(),
            &format!(
                "[defaults]\n\
             worktree_root = '{ABS_W}'\n\
             branch_prefix = \"lev/\"\n\
             baseline_ref = \"origin/main\"\n\
             baseline_dir = \"~/wt/_baseline\"\n"
            ),
        );
        let (cfg, _) = resolve_with_home(None, tmp.path(), None, None, None, None).unwrap();
        assert_eq!(cfg.defaults.worktree_root, ABS_W);
        let home = home_dir().expect("a home directory to expand `~` against");
        assert_eq!(
            cfg.defaults.baseline_dir,
            home.join("wt").join("_baseline").to_string_lossy()
        );
    }

    #[test]
    fn an_empty_path_key_stays_empty() {
        let tmp = tempfile::tempdir().unwrap();
        write_cfg(
            tmp.path(),
            "[defaults]\n\
             worktree_root = \"/srv/trees\"\n\
             branch_prefix = \"lev/\"\n\
             baseline_ref = \"origin/main\"\n\
             doppler_yaml = \"\"\n",
        );
        let (cfg, _) = resolve_with_home(None, tmp.path(), None, None, None, None).unwrap();
        assert_eq!(
            cfg.defaults.doppler_yaml, "",
            "an unset path must not become the layer dir"
        );
    }

    #[test]
    fn branch_prefix_expands_vars_but_is_not_a_path() {
        unsafe { std::env::set_var("DEVKIT_TEST_DEV", "lev") };
        let tmp = tempfile::tempdir().unwrap();
        write_cfg(
            tmp.path(),
            "[defaults]\n\
             worktree_root = \"/srv/trees\"\n\
             branch_prefix = \"${DEVKIT_TEST_DEV}/\"\n\
             baseline_ref = \"origin/main\"\n",
        );
        let (cfg, _) = resolve_with_home(None, tmp.path(), None, None, None, None).unwrap();
        assert_eq!(cfg.defaults.branch_prefix, "lev/");
    }

    #[test]
    fn an_unset_var_fails_the_whole_config_load() {
        let tmp = tempfile::tempdir().unwrap();
        write_cfg(
            tmp.path(),
            "[defaults]\n\
             worktree_root = \"${DEVKIT_TEST_MISSING_ROOT}/trees\"\n\
             branch_prefix = \"lev/\"\n\
             baseline_ref = \"origin/main\"\n",
        );
        let err = resolve_with_home(None, tmp.path(), None, None, None, None)
            .expect_err("unset var must fail the load");
        assert!(
            err.to_string().contains("DEVKIT_TEST_MISSING_ROOT"),
            "{err}"
        );
    }

    #[test]
    fn a_set_but_empty_var_stays_empty_not_the_layer_dir() {
        // `std::env::var` only errors on an *unset* variable — a variable set
        // to the empty string (`export FOO=`, common under direnv/CI) expands
        // to `""` and must be treated the same as an unset optional path, not
        // silently resolved to the declaring layer's own directory.
        unsafe { std::env::set_var("DEVKIT_TEST_EMPTY", "") };
        let tmp = tempfile::tempdir().unwrap();
        write_cfg(
            tmp.path(),
            "[defaults]\n\
             worktree_root = \"/srv/trees\"\n\
             branch_prefix = \"lev/\"\n\
             baseline_ref = \"origin/main\"\n\
             doppler_yaml = \"${DEVKIT_TEST_EMPTY}\"\n",
        );
        let (cfg, _) = resolve_with_home(None, tmp.path(), None, None, None, None).unwrap();
        assert_eq!(
            cfg.defaults.doppler_yaml, "",
            "a set-but-empty variable must not become the layer dir"
        );
    }

    #[test]
    fn a_relative_start_still_resolves_to_an_absolute_path() {
        // `devkit doctor` calls `load(None, Path::new("."))` — a relative
        // `start` must not leak into the resolved `worktree_root`, since the
        // ports registry uses it as holder identity and for prefix matching.
        let tmp = tempfile::tempdir().unwrap();
        write_cfg(
            tmp.path(),
            "[defaults]\n\
             worktree_root = \"../proj-worktrees\"\n\
             branch_prefix = \"lev/\"\n\
             baseline_ref = \"origin/main\"\n",
        );
        // `resolve_with_home` absolutizes a relative `start` against the
        // process's current directory, so this test drives that path the same
        // way `devkit doctor` does. Other tests in this suite only ever *read*
        // `current_dir()` (to build an already-absolute path), so a transient
        // cwd change here does not corrupt them even if `cargo test` runs
        // threads concurrently.
        let cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();
        // The expectation is anchored to the directory the process reports, not
        // to `tmp`: macOS resolves a `/var/folders/...` temp dir through the
        // `/var` -> `/private/var` symlink, so the two spellings differ.
        let here = std::env::current_dir().unwrap();
        let result = resolve_with_home(None, Path::new("."), None, None, None, None);
        std::env::set_current_dir(&cwd).unwrap();
        let (cfg, _) = result.unwrap();
        let root = Path::new(&cfg.defaults.worktree_root);
        assert!(root.is_absolute(), "{root:?} must be absolute");
        let expected = here.parent().unwrap().join("proj-worktrees");
        assert_eq!(cfg.defaults.worktree_root, expected.to_string_lossy());
    }

    #[test]
    fn tracker_kind_parses_from_the_table() {
        let c: Config = toml::from_str(
            "[defaults]\nworktree_root = \"/x\"\nbranch_prefix = \"l/\"\n\
             baseline_ref = \"origin/main\"\n\
             [tracker]\nkind = \"github\"\n",
        )
        .unwrap();
        assert_eq!(c.tracker.kind, Some(TrackerKind::Github));
    }

    /// `as_str` is the config spelling, so it has to stay equal to what serde
    /// writes and reads — a divergence would have `devkit doctor` naming a
    /// `kind` value no config accepts.
    #[test]
    fn tracker_kind_spelling_matches_its_serialized_form() {
        for k in [TrackerKind::Linear, TrackerKind::Github, TrackerKind::None] {
            let wire = toml::Value::try_from(k).expect("a kind serializes");
            assert_eq!(wire, toml::Value::String(k.as_str().to_string()));
            assert_eq!(wire.try_into::<TrackerKind>().unwrap(), k);
        }
    }

    #[test]
    fn an_absent_tracker_table_leaves_the_kind_unset() {
        let c: Config = toml::from_str(
            "[defaults]\nworktree_root = \"/x\"\nbranch_prefix = \"l/\"\n\
             baseline_ref = \"origin/main\"\n",
        )
        .unwrap();
        assert_eq!(c.tracker.kind, None, "absent means detect, not linear");
    }

    #[test]
    fn baseline_path_in_a_project_layer_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("devkit.toml"),
            "[config]\nroot = true\n[defaults]\nbaseline_path = '/old'\n",
        )
        .unwrap();
        let err = resolve_with_home(None, dir.path(), None, None, None, None).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("baseline_dir"), "names the replacement: {msg}");
        assert!(msg.contains("devkit.toml"), "names the layer: {msg}");
    }

    /// `check_baseline_path` is what `resolve_with_home` calls, tested directly
    /// against the origin map so the outcome depends only on which layer
    /// declared the key — not on whatever else discovery would merge in above
    /// a tempdir. Paired with the error-arm test below: swapping the two
    /// branches in the implementation fails both.
    #[test]
    fn check_baseline_path_warns_when_its_origin_is_the_home_layer() {
        let home = tempfile::tempdir().unwrap();
        let home_cfg = home.path().join("config.toml");
        let mut origin = HashMap::new();
        origin.insert("defaults.baseline_path".to_string(), home_cfg.clone());

        let warning = check_baseline_path(&origin, Some(&home_cfg))
            .unwrap()
            .expect("the home layer warns rather than erroring");
        assert!(warning.contains("baseline_dir"), "{warning}");
    }

    #[test]
    fn check_baseline_path_errors_when_its_origin_is_not_the_home_layer() {
        let home = tempfile::tempdir().unwrap();
        let home_cfg = home.path().join("config.toml");
        let project = tempfile::tempdir().unwrap();
        let project_cfg = project.path().join("devkit.toml");
        let mut origin = HashMap::new();
        origin.insert("defaults.baseline_path".to_string(), project_cfg.clone());

        let err = check_baseline_path(&origin, Some(&home_cfg)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("baseline_dir"), "names the replacement: {msg}");
        assert!(
            msg.contains(&project_cfg.display().to_string()),
            "names the layer: {msg}"
        );
    }

    /// A typo in `required` is the one config mistake that produces no signal
    /// without `deny_unknown_fields`: serde consumes the unknown key as
    /// `IgnoredAny`, the entry stays fail-open, and files the user believed
    /// were protected are removed with the worktree.
    #[test]
    fn a_misspelled_preserve_key_is_rejected() {
        let err = toml::from_str::<Config>(
            "[preserve.notes]\nfrom = [\"a.md\"]\nto = \"/archive\"\nrequred = true\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("requred"), "{err}");
    }

    #[test]
    fn a_preserve_entry_parses_with_required_defaulting_off() {
        let cfg: Config = toml::from_str(
            "[preserve.notes]\nfrom = [\"scratch/\"]\nto = \"/archive/{{ issue }}\"\n",
        )
        .unwrap();
        let entry = &cfg.preserve["notes"];
        assert_eq!(entry.from, vec!["scratch/".to_string()]);
        assert_eq!(entry.to, "/archive/{{ issue }}");
        assert!(!entry.required);
    }

    /// A thread count is machine tuning, set in the personal layer, so a config
    /// carrying only `[parallelism]` has to resolve with no project around it.
    #[test]
    fn a_parallelism_only_config_needs_no_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("parallelism-only");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("devkit.toml"),
            "[config]\nroot = true\n[parallelism]\nthreads = 8\n",
        )
        .unwrap();

        let (cfg, _) = resolve_with_home(None, &project, None, None, None, None).unwrap();
        assert_eq!(cfg.parallelism.threads.map(|n| n.get()), Some(8));
        assert_eq!(health_with_home(&project, None, None, None), Health::Ok);
    }

    /// `ThreadPoolBuilder::num_threads(0)` means one thread per core, so zero
    /// is the opposite of what it looks like. `NonZeroUsize` refuses it at
    /// parse time instead of leaving a runtime clamp to remember.
    #[test]
    fn a_zero_thread_count_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("zero-threads");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("devkit.toml"),
            "[config]\nroot = true\n[parallelism]\nthreads = 0\n",
        )
        .unwrap();

        assert!(resolve_with_home(None, &project, None, None, None, None).is_err());
    }

    /// An absent table takes the pool's own default rather than a serde one, so
    /// the number lives in exactly one place.
    #[test]
    fn an_absent_parallelism_table_leaves_threads_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("no-parallelism");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("devkit.toml"), "[config]\nroot = true\n").unwrap();

        let (cfg, _) = resolve_with_home(None, &project, None, None, None, None).unwrap();
        assert!(cfg.parallelism.threads.is_none());
    }

    #[test]
    fn parses_the_issue_end_hook_keys() {
        let cfg: Config = toml::from_str(
            "[hooks]\n\
             after_worktree_remove = [[\"zoxide\", \"remove\", \"{{ worktree }}\"]]\n\
             after_end = [[\"alacritree\", \"project\", \"refresh\"]]\n",
        )
        .unwrap();
        assert_eq!(cfg.hooks.after_worktree_remove.len(), 1);
        assert_eq!(cfg.hooks.after_worktree_remove[0], [
            "zoxide",
            "remove",
            "{{ worktree }}"
        ]);
        assert_eq!(cfg.hooks.after_end.len(), 1);
        assert_eq!(cfg.hooks.after_end[0], ["alacritree", "project", "refresh"]);
    }

    #[test]
    fn the_issue_end_hook_keys_default_empty() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.hooks.after_worktree_remove.is_empty());
        assert!(cfg.hooks.after_end.is_empty());
    }
}
