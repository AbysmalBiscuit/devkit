use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

mod layers;
pub use layers::{Layer, LayerKind, project_layers};

#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
pub struct Config {
    /// Project-wide paths and branch conventions. Required of any config that
    /// configures a project; omitted only by one built entirely from
    /// `[config]`, `[harness]`, `[docs]`, and `[brief]`.
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
}

/// The `devkitd` supervisor: whether it starts, how long it lingers, and the
/// crash-loop, memory, and health policies it applies to supervised servers.
#[derive(Debug, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct DaemonConfig {
    /// Run gate: autostart the daemon only when true (or via DEVKIT_DAEMON=1 / --supervise).
    pub enabled: bool,
    /// Exit after this many idle seconds with zero clients AND zero supervised children.
    pub idle_timeout_secs: u64,
    /// Crash-loop guard: restarts allowed within `restart_window_secs`.
    pub max_restarts: u32,
    /// Length of the sliding window the `max_restarts` budget is counted over.
    pub restart_window_secs: u64,
    /// Log a loud line past this supervised tree-RSS in MB (0 = off).
    pub memory_warn_mb: u64,
    /// Take `memory_action` past this tree-RSS in MB (0 = off).
    pub memory_limit_mb: u64,
    /// Action when tree-RSS crosses `memory_limit_mb`: "warn" (log a line) or
    /// "restart" (SIGTERM and let the crash path respawn). Any other value falls
    /// back to warn.
    pub memory_action: String,
    /// Consecutive supervision ticks at or over `memory_limit_mb` before the
    /// restart action fires (debounces transient allocation spikes).
    pub memory_limit_ticks: u32,
    /// Hard kernel memory ceiling per supervised tree, in MB (0 = off,
    /// Linux-only). Enforced via a cgroup-v2 leaf with memory.max; a breach
    /// OOM-kills the tree and the crash path respawns it. Set above
    /// memory_limit_mb so the soft poll-based action stays the graceful first
    /// responder. Falls back to the soft action where cgroup-v2 delegation is
    /// unavailable.
    pub memory_max_mb: u64,
    /// Health-probe interval in seconds; 0 disables probing (no probe thread).
    pub health_probe_secs: u64,
    /// Consecutive post-arming probe failures before a server is judged hung.
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
/// section even when the checkout has something to put in it.
#[derive(Debug, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct BriefConfig {
    /// The whole brief.
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

/// Commands run on a devkit lifecycle event. Each key is named
/// `{before,after}_<event>` and holds a list of argv arrays — no shell, so
/// pipes, `&&`, and globs are not available. A hook that fails to render,
/// spawn, or exit zero warns on stderr and the remaining hooks still run.
#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct HooksConfig {
    /// Runs once in the root of a worktree `issue setup` or
    /// `issue checkout-pr` has just created, after its apps are prepared and
    /// after the command has reported the worktree. Each argv element is
    /// rendered as minijinja over `worktree`, `branch`, `issue`, `slug`,
    /// `apps`, `prefix`, and `[templates.variables]`.
    pub after_worktree_create: Vec<Vec<String>>,
}

/// Linear lookups that cost an extra API round trip, so each is opt-in.
#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct LinearConfig {
    /// Query Linear for the issues linked to each PR in `issue prs` (one
    /// extra batched round trip per run). Off by default.
    pub resolve_pr_links: bool,
}

/// Which GitHub repositories this project uses. Both default to the `origin`
/// remote. They are separate because a fork opens its PRs upstream while its
/// issues may sit on either side, and because a project may track issues in a
/// repository separate from its code.
///
/// This table is not under `[tracker]`: a project on Linear with a fork
/// workflow needs `pr_repo` just as much as a GitHub one does.
#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GithubConfig {
    /// Repository holding the issues, e.g. `org/planning`.
    pub issues_repo: Option<String>,
    /// Repository pull requests are opened against, e.g. `upstream/app`.
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
#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct TrackerConfig {
    /// Force a tracker instead of detecting one.
    pub kind: Option<TrackerKind>,
}

/// Project-wide paths and branch conventions. The first four keys are required
/// of any config that configures a project — without them no worktree, branch,
/// or baseline resolves. A config carrying only `[config]`, `[harness]`,
/// `[docs]`, or `[brief]` omits the table entirely and takes the defaults.
#[derive(Debug, JsonSchema, Deserialize, Serialize)]
pub struct Defaults {
    /// Directory issue worktrees are created under. `~` is expanded.
    pub worktree_root: String,
    /// Prefix on branches created by `issue setup`, e.g. `you/`.
    pub branch_prefix: String,
    /// Git ref the baseline server tracks, e.g. `origin/staging`.
    pub baseline_ref: String,
    /// Checkout path for the baseline server. `~` is expanded.
    pub baseline_path: String,
    /// Path to the repo's `doppler.yaml`; its `setup` paths seed app path
    /// inference. `~` is expanded. Leave empty and every app needs its own
    /// `path`.
    #[serde(default)]
    pub doppler_yaml: String,
    /// Repo-relative directory apps live under (e.g. "apps"). Used to infer app
    /// paths from doppler.yaml and to detect changed apps in a diff.
    #[serde(default = "default_apps_dir")]
    pub apps_dir: String,
    /// Base branch used when opening PRs (e.g. "staging", "main").
    #[serde(default = "default_pr_base")]
    pub pr_base: String,
    /// Refuse `issue review request` when it would open a PR with no `--to`
    /// reviewer. Off by default: a PR opens unreviewed and nobody is Slacked.
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
    /// Glob patterns (relative to the monorepo root) for untracked local files to
    /// copy into a newly created worktree. Each match is copied to the same
    /// relative path; a match that is a directory (or a pattern ending in `/`) is
    /// copied recursively. Existing destinations are left alone, and only
    /// `issue sync-includes --overwrite` replaces them. Empty by default — the
    /// backfill is opt-in.
    #[serde(default)]
    pub worktree_include: Vec<String>,
    /// Write the issue summary file on every `issue setup`, as though
    /// `--summary` were passed. `--summary` / `--no-summary` still win for one
    /// run. Off by default.
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
            baseline_path: String::new(),
            doppler_yaml: String::new(),
            apps_dir: default_apps_dir(),
            pr_base: default_pr_base(),
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
    "staging".to_string()
}

fn default_stray_scan_width() -> u16 {
    64
}

/// A team member's handle mapping (Slack user-id, GitHub login, etc.).
#[derive(Debug, JsonSchema, Deserialize, Serialize)]
pub struct Person {
    /// Slack user or channel id, e.g. `U0XXXXXXXXX`.
    pub slack: String,
    /// GitHub login requested as the PR reviewer when this alias is passed to
    /// `issue review request --to`. Omit and the alias only gets Slacked.
    #[serde(default)]
    pub github: Option<String>,
}

/// A file written into an app's directory during `issue setup`, before the app's
/// `setup` commands run. `content` is written verbatim — no format assembly or
/// newline injection. Parent directories are created. Existing files are left
/// untouched unless `overwrite` is set.
#[derive(Debug, Clone, JsonSchema, Deserialize, Serialize)]
pub struct PrepFile {
    /// Target path, relative to the app's directory.
    pub path: String,
    /// File contents, written byte-for-byte.
    pub content: String,
    /// Overwrite an existing file rather than skipping it.
    #[serde(default)]
    pub overwrite: bool,
}

/// One step of a sequence task: run a sibling command task, or bring an app up.
#[derive(Debug, Clone, JsonSchema, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    /// Name of a sibling command task to run at this point in the sequence.
    Task(String),
    /// Name of an app to bring up. Idempotent: an app already running in this
    /// worktree is reported rather than spawned again.
    Up(String),
}

/// A canned oneshot invoked by name via `devrun task`: either a command
/// (`run`, optionally scoped to an `app` for cwd + static_env) or a sequence
/// (`steps`). Exactly one of `run`/`steps` must be set; a sequence task
/// carries no `app`/`env`. Validated at resolution, not at parse.
#[derive(Debug, Default, Clone, JsonSchema, Deserialize, Serialize)]
pub struct TaskConfig {
    /// One line shown in `devrun task --list` and the session brief.
    #[serde(default)]
    pub description: Option<String>,
    /// App whose directory the task runs in, inheriting that app's
    /// `static_env`. Omit to run at the repo root.
    #[serde(default)]
    pub app: Option<String>,
    /// The command as one argv (program + args), rendered as minijinja over
    /// `{{ port }}`, `ports['<app>']`, and `[templates.variables]`. Mutually
    /// exclusive with `steps`.
    #[serde(default)]
    pub run: Vec<String>,
    /// A sequence run in order, each step either a command or an `up`. Each
    /// step re-resolves its ports immediately before it runs. Mutually
    /// exclusive with `run`.
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Env vars set for this task, layered over the app's `static_env` and
    /// under the CLI's `--env-file` and `--env`.
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    /// Apps whose server must be live in this worktree when the task executes.
    /// Each name must be referenced by the task's templates via `ports[...]`;
    /// a user `--env` override of every referencing value waives the check.
    #[serde(default)]
    pub require_live: Vec<String>,
}

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

/// Config-driven minijinja templates for the issue-lifecycle strings. Each
/// `None` field falls back to its `DEFAULT_*` constant, which reproduces the
/// historical hardcoded output. `variables` are user constants merged under
/// every render context.
#[derive(Debug, JsonSchema, Deserialize, Serialize, Default)]
pub struct Templates {
    /// Branch name created by `issue setup`. Context: `prefix`, `issue`,
    /// `slug`, `apps`. Defaults to `{{ prefix }}{{ slug }}`.
    pub branch: Option<String>,
    /// Worktree directory name created by `issue setup`, relative to
    /// `defaults.worktree_root`. Same context as `branch`; defaults to
    /// `{{ slug }}`.
    pub worktree_dir: Option<String>,
    /// Worktree directory name created by `issue checkout-pr`. Context:
    /// `pr_number`, `pr_title`, `linear_id`, `linear_title` — titles are
    /// slugified, and the `linear_*` names are historical: they carry
    /// whichever tracker's id and title resolved, and are empty on the
    /// PR-only path.
    pub checkout_worktree_dir: Option<String>,
    /// Title of a PR opened by `issue review request`. `{{ input }}` is the
    /// `--pr-title` argument.
    pub pr_title: Option<String>,
    /// Body of a PR opened by `issue review request`. `{{ input }}` is the
    /// `--pr-body` argument.
    pub pr_body: Option<String>,
    /// Slack message sent by `issue review request`. Rendered once per
    /// recipient with `name`, `slack_id`, `pr_url`, `pr_title`, and `input`.
    pub review_request: Option<String>,
    /// Slack message sent by `issue review finish`. Same context as
    /// `review_request`, plus `author`.
    pub review_finish: Option<String>,
    /// Where `issue setup --summary` writes the issue summary file. A relative
    /// path is taken from `defaults.worktree_root`, so the file outlives the
    /// worktree; render `{{ worktree }}` into it to keep it inside instead.
    /// Context: the `issue_summary` context below.
    pub issue_summary_path: Option<String>,
    /// Body of the file `issue setup --summary` writes. Context: `issue`,
    /// `title`, `url`, `description`, `state`, `assignee`, `priority`,
    /// `estimate`, `labels`, `parent`, `project`, `worktree`, `branch`, `slug`,
    /// `prefix`, `apps` — every Linear field is the empty string when Linear
    /// has nothing there.
    pub issue_summary: Option<String>,
    /// Constants available to every template above. A context field of the same
    /// name wins, and `--arg key=value` overrides either.
    #[serde(default)]
    pub variables: std::collections::BTreeMap<String, String>,
}

impl Templates {
    pub fn branch(&self) -> &str {
        self.branch.as_deref().unwrap_or(DEFAULT_BRANCH)
    }
    pub fn worktree_dir(&self) -> &str {
        self.worktree_dir.as_deref().unwrap_or(DEFAULT_WORKTREE_DIR)
    }
    pub fn checkout_worktree_dir(&self) -> &str {
        self.checkout_worktree_dir
            .as_deref()
            .unwrap_or(DEFAULT_CHECKOUT_WORKTREE_DIR)
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
}

/// The `url` an app is addressed at when it sets none of its own.
pub const DEFAULT_APP_URL: &str = "http://localhost:{{ port }}";

/// One runnable app. `base_port` and `launch` are required; `path` is required
/// too whenever `defaults.doppler_yaml` cannot infer the directory.
#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
pub struct AppConfig {
    /// Start of the app's port band. Each worktree is allocated its own port
    /// from here by the registry, so two worktrees never collide.
    pub base_port: u16,
    /// The complete launch command as one argv, run verbatim — devkit builds no
    /// prefix of its own, so any `doppler run -c <config> --` wrapper belongs
    /// here. Rendered as minijinja over `{{ port }}`, `ports['<app>']`, and
    /// `[templates.variables]`.
    pub launch: Vec<String>,
    /// Address the app serves on, as a template over the same variables as
    /// `launch` (`{{ port }}`, `ports['<app>']`, `[templates.variables]`).
    /// Defaults to `DEFAULT_APP_URL`; set it for an app that serves https, a
    /// custom host, or a path prefix — devkit never terminates TLS itself.
    #[serde(default)]
    pub url: Option<String>,
    /// Env var this app receives the `provides_url` app's URL through.
    #[serde(default)]
    pub url_env: Option<String>,
    /// This app serves the URL that consumer apps wire to via their `url_env`.
    /// Exactly one app (the API) is normally marked; consumers reference it by role,
    /// not by a hardcoded name.
    #[serde(default)]
    pub provides_url: bool,
    /// Commands run in the app's directory during `issue setup`, in order. Each
    /// inner array is one argv (program + args), e.g.
    /// `[["doppler","run","-c","local","--","bun","install"]]`.
    #[serde(default)]
    pub setup: Vec<Vec<String>>,
    /// App subdirectory relative to the repo root, when it differs from the
    /// app's own name. Required unless `defaults.doppler_yaml` infers it.
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
    /// Files written into the app's directory during `issue setup` (before `setup`).
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
    /// Resolved layer files, lowest→highest precedence.
    pub layers: Vec<PathBuf>,
    /// Dotted config path (e.g. `apps.api.base_port`) → file that supplied it.
    pub origin: HashMap<String, PathBuf>,
}

/// Deep-merge parsed layers given lowest→highest precedence. Tables merge key by
/// key; every non-table value (scalar or array) is replaced wholesale by a higher
/// layer. Records, per leaf dotted-path, the highest layer that set it.
pub(crate) fn merge_layers(
    layers: &[(PathBuf, toml::Table)],
) -> (toml::Table, HashMap<String, PathBuf>) {
    let mut merged = toml::Table::new();
    let mut origin = HashMap::new();
    for (path, table) in layers {
        deep_merge(&mut merged, table, path, "", &mut origin);
    }
    (merged, origin)
}

fn deep_merge(
    acc: &mut toml::Table,
    overlay: &toml::Table,
    src: &Path,
    prefix: &str,
    origin: &mut HashMap<String, PathBuf>,
) {
    for (k, v) in overlay {
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        if let (Some(toml::Value::Table(at)), toml::Value::Table(ot)) = (acc.get_mut(k), v) {
            deep_merge(at, ot, src, &path, origin);
        } else {
            record_origin(&path, v, src, origin);
            acc.insert(k.clone(), v.clone());
        }
    }
}

/// Record the source file for every scalar/array leaf reachable from `v`. A table
/// recurses into its keys; everything else is a single leaf.
fn record_origin(path: &str, v: &toml::Value, src: &Path, origin: &mut HashMap<String, PathBuf>) {
    match v {
        toml::Value::Table(t) => {
            for (k, sub) in t {
                record_origin(&format!("{path}.{k}"), sub, src, origin);
            }
        }
        _ => {
            origin.insert(path.to_string(), src.to_path_buf());
        }
    }
}

/// Flatten a serialized config `Value` into `(dotted-path, leaf-value)` pairs. Tables
/// recurse; scalars and arrays are leaves. Mirrors `record_origin`'s leaf model so
/// every emitted path can be looked up in `Provenance::origin`.
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
    // `[defaults]` paths to, so it has to be absolute however the caller spelled
    // it — an explicit `--config` and `$DEVKIT_CONFIG` both arrive verbatim.
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
#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
pub struct LayerMarker {
    /// Stop walking upward at this file, and drop `~/.config/devkit/config.toml`
    /// from the layer stack.
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

/// Build the ordered layer list (lowest→highest precedence): the home config (unless
/// a `root = true` marker cuts it off), then the project layers `project_layers`
/// finds for `start`. An explicit path or `$DEVKIT_CONFIG` is the sole layer.
fn discover(
    explicit: Option<&Path>,
    start: &Path,
    home: Option<&Path>,
) -> Result<Vec<(PathBuf, toml::Table)>> {
    if let Some(p) = explicit {
        return Ok(vec![read_layer(p)?]);
    }
    if let Some(p) = std::env::var_os("DEVKIT_CONFIG") {
        return Ok(vec![read_layer(&PathBuf::from(p))?]);
    }

    let (project, rooted) = layers::project_layers_rooted(start, None)?;

    let mut layers: Vec<(PathBuf, toml::Table)> = Vec::new();
    if !rooted
        && let Some(h) = home
        && h.is_file()
    {
        layers.push(read_layer(h)?);
    }
    for layer in &project {
        layers.push(read_layer(&layer.path)?);
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

/// Classify the config reachable from `start` without ever failing.
pub fn health(start: &Path) -> Health {
    health_with_home(start, home_config_path().as_deref())
}

pub(crate) fn health_with_home(start: &Path, home: Option<&Path>) -> Health {
    match resolve_with_home(None, start, home) {
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

/// Tables a devkit.toml may carry on its own, with no project configured around
/// them. Each is read by a crate that needs no path or branch convention:
/// `[harness]` by `devkit-locks`, `[docs]` by `devkit-docs`, `[config]` by the
/// layer walk below, `[brief]` by the session summary. A config built only from
/// these resolves without a `[defaults]` table; anything else demands one.
const STANDALONE_SECTIONS: [&str; 4] = ["config", "harness", "docs", "brief"];

/// Resolve the effective config by layering and deep-merging all applicable files.
pub fn resolve(explicit: Option<&Path>, start: &Path) -> Result<(Config, Provenance)> {
    resolve_with_home(explicit, start, home_config_path().as_deref())
}

/// `resolve` with an injectable home-config path (tests pass a controlled path or
/// `None` so the real `~/.config/devkit/config.toml` never participates).
pub(crate) fn resolve_with_home(
    explicit: Option<&Path>,
    start: &Path,
    home: Option<&Path>,
) -> Result<(Config, Provenance)> {
    // Every discovered layer path, and every `[defaults]` path resolved against
    // it, must be absolute — `strays/mod.rs` uses `worktree_root` as holder
    // identity and for prefix matching, so a relative `start` (e.g. `doctor`
    // passing `.`) must not leak into that value.
    let start_buf = absolutize(start)?;
    let start = start_buf.as_path();
    let layers = discover(explicit, start, home)?;
    let order: Vec<PathBuf> = layers.iter().map(|(p, _)| p.clone()).collect();
    let (merged, origin) = merge_layers(&layers);
    if !merged.contains_key("defaults")
        && let Some(section) = merged
            .keys()
            .find(|k| !STANDALONE_SECTIONS.contains(&k.as_str()))
    {
        anyhow::bail!(
            "deserializing merged devkit config: missing field `defaults`, \
             which `[{section}]` needs"
        );
    }
    let mut cfg: Config = toml::Value::Table(merged)
        .try_into()
        .context("deserializing merged devkit config")?;
    resolve_defaults(&mut cfg, &origin)?;
    Ok((
        cfg,
        Provenance {
            layers: order,
            origin,
        },
    ))
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

/// Resolve `.` and `..` without touching the filesystem. `worktree_root` is
/// routinely a directory that does not exist yet, so `fs::canonicalize` is not
/// available; and the ports registry compares holder paths as strings, so a
/// surviving `..` would let one directory have two spellings.
///
/// `PathBuf::pop`'s boolean return can't drive this: it pops a trailing `..`
/// just as readily as a real component (`Path::new("..").parent()` is
/// `Some("")`), which would let a second `ParentDir` cancel the first instead
/// of accumulating. Components are tracked explicitly instead: a `ParentDir`
/// pops only when the top of the stack is a real named component; otherwise it
/// is either appended (relative, nothing to pop) or dropped (already at root).
fn normalize_lexically(p: &Path) -> PathBuf {
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

/// Expand `${VAR}`, then `~`, then anchor a still-relative path to the config
/// layer that declared it. Empty stays empty — an unset optional path must not
/// silently become the layer's own directory. Emptiness is checked after
/// expansion: a variable that is set but empty (`export FOO=`) must resolve to
/// `""`, not to the layer directory `dir.join("")` would otherwise produce.
fn resolve_path_key(raw: &str, key: &str, origin: &HashMap<String, PathBuf>) -> Result<String> {
    let expanded = expand_vars(raw, key)?;
    if expanded.is_empty() {
        return Ok(String::new());
    }
    let p = expand_tilde(&expanded);
    let joined = match (p.is_absolute(), layer_dir(origin, key)) {
        (true, _) | (false, None) => p,
        (false, Some(dir)) => dir.join(p),
    };
    Ok(normalize_lexically(&joined).to_string_lossy().into_owned())
}

/// Resolve every `[defaults]` value that carries a path or an environment
/// reference, in place, once, at load time.
fn resolve_defaults(cfg: &mut Config, origin: &HashMap<String, PathBuf>) -> Result<()> {
    for (key, field) in [
        ("defaults.worktree_root", &mut cfg.defaults.worktree_root),
        ("defaults.baseline_path", &mut cfg.defaults.baseline_path),
        ("defaults.doppler_yaml", &mut cfg.defaults.doppler_yaml),
    ] {
        *field = resolve_path_key(field, key, origin)?;
    }
    cfg.defaults.branch_prefix =
        expand_vars(&cfg.defaults.branch_prefix, "defaults.branch_prefix")?;
    Ok(())
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
    use super::*;
    use std::io::Write as _;
    pub(crate) const SAMPLE: &str = r#"
[defaults]
worktree_root = "~/Git/example"
branch_prefix = "lev/"
baseline_ref = "origin/staging"
baseline_path = "~/Git/example/_baseline"
doppler_yaml = "~/Git/example/monorepo/doppler.yaml"
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
            baseline_path = "/b"
            worktree_include = ["apps/*/.env.local", ".tool-versions"]
            [apps]
            "#,
        )
        .unwrap();
        assert_eq!(
            cfg.defaults.worktree_include,
            vec![
                "apps/*/.env.local".to_string(),
                ".tool-versions".to_string()
            ]
        );

        let bare: Config = toml::from_str(
            r#"
            [defaults]
            worktree_root = "/w"
            branch_prefix = "you/"
            baseline_ref = "origin/staging"
            baseline_path = "/b"
            [apps]
            "#,
        )
        .unwrap();
        assert!(bare.defaults.worktree_include.is_empty());
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
    fn parses_app_setup_commands() {
        let src = format!(
            "{SAMPLE}setup = [[\"doppler\", \"run\", \"-c\", \"local\", \"--\", \"bun\", \"install\"]]\n"
        );
        let c = Config::parse(&src).unwrap();
        assert_eq!(
            c.apps["api"].setup,
            vec![vec![
                "doppler".to_string(),
                "run".to_string(),
                "-c".to_string(),
                "local".to_string(),
                "--".to_string(),
                "bun".to_string(),
                "install".to_string(),
            ]]
        );
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
worktree_root = "~/Git/example"
branch_prefix = "lev/"
baseline_ref = "origin/staging"
baseline_path = "~/Git/example/_baseline"
doppler_yaml = "~/Git/example/monorepo/doppler.yaml"
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
worktree_root = "~/Git/example"
branch_prefix = "lev/"
baseline_ref = "origin/staging"
baseline_path = "~/Git/example/_baseline"
doppler_yaml = "~/Git/example/monorepo/doppler.yaml"
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
worktree_root = "~/Git/example"
branch_prefix = "lev/"
baseline_ref = "origin/staging"
baseline_path = "~/Git/example/_baseline"
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
worktree_root = "~/Git/example"
branch_prefix = "lev/"
baseline_ref = "origin/staging"
baseline_path = "~/Git/example/_baseline"
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
        let (m, origin) =
            merge_layers(&[(PathBuf::from("/base"), base), (PathBuf::from("/top"), top)]);
        assert_eq!(m["defaults"]["branch_prefix"].as_str(), Some("y/"));
        assert_eq!(m["defaults"]["worktree_root"].as_str(), Some("/a"));
        assert_eq!(origin["defaults.branch_prefix"], PathBuf::from("/top"));
        assert_eq!(origin["defaults.worktree_root"], PathBuf::from("/base"));
    }

    #[test]
    fn arrays_replace_wholesale() {
        let base = tbl("[apps.api]\nlaunch=['a','b']\n");
        let top = tbl("[apps.api]\nlaunch=['c']\n");
        let (m, origin) = merge_layers(&[(PathBuf::from("/b"), base), (PathBuf::from("/t"), top)]);
        let launch = m["apps"]["api"]["launch"].as_array().unwrap();
        assert_eq!(launch.len(), 1);
        assert_eq!(launch[0].as_str(), Some("c"));
        assert_eq!(origin["apps.api.launch"], PathBuf::from("/t"));
    }

    #[test]
    fn nested_maps_merge_per_key() {
        let base = tbl("[apps.api.static_env]\nA='1'\nB='2'\n");
        let top = tbl("[apps.api.static_env]\nB='9'\nC='3'\n");
        let (m, origin) = merge_layers(&[(PathBuf::from("/b"), base), (PathBuf::from("/t"), top)]);
        let se = &m["apps"]["api"]["static_env"];
        assert_eq!(se["A"].as_str(), Some("1"));
        assert_eq!(se["B"].as_str(), Some("9"));
        assert_eq!(se["C"].as_str(), Some("3"));
        assert_eq!(origin["apps.api.static_env.B"], PathBuf::from("/t"));
        assert_eq!(origin["apps.api.static_env.A"], PathBuf::from("/b"));
    }

    /// An absolute path, spelled the way the host spells one. `resolve_path_key`
    /// anchors anything `Path::is_absolute` rejects to its declaring layer, and a
    /// leading `/` is not absolute on Windows, so a fixture that spelled one
    /// there would move with the layer instead of standing still.
    #[cfg(not(windows))]
    const ABS_W: &str = "/w";
    #[cfg(windows)]
    const ABS_W: &str = "C:\\w";

    /// Every `[defaults]` key a `Config` requires, with `ABS_W` as the worktree
    /// root so the layering tests read back what they wrote.
    #[cfg(not(windows))]
    const FULL_DEFAULTS: &str =
        "worktree_root='/w'\nbranch_prefix='x/'\nbaseline_ref='r'\nbaseline_path='/b'\n";
    #[cfg(windows)]
    const FULL_DEFAULTS: &str =
        "worktree_root='C:\\w'\nbranch_prefix='x/'\nbaseline_ref='r'\nbaseline_path='C:\\b'\n";

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
        let (cfg, prov) = resolve_with_home(None, &child, None).unwrap();
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
        let (cfg, prov) = resolve_with_home(None, &child, Some(&home)).unwrap();
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
        let (cfg, prov) = resolve_with_home(None, &repo, Some(&home)).unwrap();
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
        let (cfg, prov) = resolve_with_home(None, repo.path(), None).unwrap();
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
        let (cfg, _) = resolve_with_home(None, &child, None).unwrap();
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
        let (cfg, prov) = resolve_with_home(None, repo.path(), None).unwrap();
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
        let (cfg, prov) = resolve_with_home(None, &child, Some(&home)).unwrap();
        assert_eq!(cfg.defaults.worktree_root, ABS_W); // parent's /PARENT dropped
        assert_eq!(cfg.defaults.branch_prefix, "x/"); // home's HOME/ dropped
        assert_eq!(
            prov.layers,
            vec![child.join("devkit.toml"), child.join("devkit.local.toml")]
        );
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
        let (cfg, prov) = resolve_with_home(Some(&explicit), &child, None).unwrap();
        assert_eq!(cfg.apps["api"].base_port, 7);
        assert_eq!(cfg.defaults.branch_prefix, "x/"); // child file not consulted
        assert_eq!(prov.layers, vec![explicit]);
    }

    #[test]
    fn resolve_errors_when_no_config_found() {
        let root = tempfile::tempdir().unwrap();
        let err = resolve_with_home(None, root.path(), None).unwrap_err();
        assert!(err.to_string().contains("no devkit.toml"));
    }

    #[test]
    fn parses_prep_files_with_overwrite_default() {
        let toml = r#"
[defaults]
worktree_root = "~/wt"
branch_prefix = "x/"
baseline_ref = "origin/main"
baseline_path = "/b"

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
            t.variables.get("team").map(String::as_str),
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
baseline_path = "~/tmp/baseline"

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
        assert_eq!(t.run[0], "doppler");
        assert_eq!(t.env["NITRO_PRESET"], "node-server");
        assert!(t.steps.is_empty());
        let s = &c.tasks["profile-lab-os"];
        assert_eq!(
            s.steps,
            vec![
                Step::Task("api-prod-build".to_string()),
                Step::Up("api-prod".to_string())
            ]
        );
        assert!(s.run.is_empty());
    }

    #[test]
    fn tasks_roundtrip_through_toml() {
        let src = "[defaults]\nworktree_root = \"w\"\nbranch_prefix = \"x/\"\n\
                   baseline_ref = \"m\"\nbaseline_path = \"b\"\n\
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
        let (m, _) = merge_layers(&[(PathBuf::from("/b"), base), (PathBuf::from("/t"), top)]);
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

        let (cfg, _) = resolve_with_home(None, &project, Some(&home)).unwrap();
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
        let (cfg, _) = resolve_with_home(None, &bare, None).unwrap();
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
            "[config]\nroot = true\n[harness]\nenforce_writes = true\n",
        )
        .unwrap();

        let (cfg, _) = resolve_with_home(None, &project, None).unwrap();
        assert_eq!(cfg.defaults.worktree_root, "");
        assert_eq!(cfg.defaults.apps_dir, "apps");
        assert_eq!(cfg.defaults.pr_base, "staging");
        assert_eq!(cfg.defaults.stray_scan_width, 64);
        assert_eq!(health_with_home(&project, None), Health::Ok);
    }

    #[test]
    fn a_config_that_configures_a_project_still_needs_defaults() {
        // Relaxing the requirement for standalone sections must not let an
        // `[apps]` table through with empty paths — that would launch servers
        // against a worktree root of "".
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("apps-no-defaults");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("devkit.toml"),
            "[config]\nroot = true\n[apps.api]\nbase_port = 9100\nlaunch = ['echo']\n",
        )
        .unwrap();

        match health_with_home(&project, None) {
            Health::Broken(why) => assert!(why.contains("defaults"), "{why}"),
            other => panic!("expected Broken, got {other:?}"),
        }
    }

    #[test]
    fn health_tells_an_absent_config_from_a_broken_one() {
        let tmp = tempfile::tempdir().unwrap();

        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(health_with_home(&empty, None), Health::Absent);

        let good = tmp.path().join("good");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::write(
            good.join("devkit.toml"),
            format!("[config]\nroot = true\n[defaults]\n{FULL_DEFAULTS}"),
        )
        .unwrap();
        assert_eq!(health_with_home(&good, None), Health::Ok);

        // A required app key left out: the exact fault a user hits by adding
        // an app entry by hand.
        let missing_key = tmp.path().join("missing-key");
        std::fs::create_dir_all(&missing_key).unwrap();
        std::fs::write(
            missing_key.join("devkit.toml"),
            format!("[config]\nroot = true\n[defaults]\n{FULL_DEFAULTS}[apps.foobar]\nlaunch = ['echo']\n"),
        )
        .unwrap();
        let Health::Broken(msg) = health_with_home(&missing_key, None) else {
            panic!("a config that does not deserialize is Broken");
        };
        assert!(msg.contains("base_port"), "{msg}");
        assert!(msg.contains("apps.foobar"), "{msg}");

        let unparseable = tmp.path().join("unparseable");
        std::fs::create_dir_all(&unparseable).unwrap();
        std::fs::write(unparseable.join("devkit.toml"), "this is not toml [[[").unwrap();
        let Health::Broken(msg) = health_with_home(&unparseable, None) else {
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

        let (cfg, _) = resolve_with_home(None, &project, None).unwrap();
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

        let (cfg, _) = resolve_with_home(None, &project, Some(&home)).unwrap();
        assert!(!cfg.brief.enabled, "the home-layer key survives");
        assert!(!cfg.brief.pins, "the project-layer key overrides");
    }

    #[test]
    fn task_require_live_roundtrips() {
        let s = "[defaults]\nworktree_root='w'\nbranch_prefix='x/'\nbaseline_ref='m'\nbaseline_path='b'\n[tasks.build]\nrun=['git']\nrequire_live=['api']\n";
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
        let got = expand_vars("/opt/$${NOT_A_VAR}/x", "defaults.baseline_path").unwrap();
        assert_eq!(got, "/opt/${NOT_A_VAR}/x");
    }

    #[test]
    fn expand_vars_passes_a_bare_dollar_through() {
        // A `$` not followed by `{` or `$` is a legal path character, so it stays.
        let got = expand_vars("/opt/a$b/c", "defaults.baseline_path").unwrap();
        assert_eq!(got, "/opt/a$b/c");
    }

    #[test]
    fn expand_vars_errors_on_an_unterminated_brace() {
        let err = expand_vars("${OPEN", "defaults.worktree_root").expect_err("unterminated");
        assert!(err.to_string().contains("unterminated"), "{err}");
    }

    #[test]
    fn expand_vars_leaves_a_plain_value_alone() {
        let got = expand_vars("~/Git/example", "defaults.worktree_root").unwrap();
        assert_eq!(got, "~/Git/example");
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
             baseline_path = \"../proj-worktrees/_baseline\"\n",
        );
        let (cfg, _) = resolve_with_home(None, &proj, None).unwrap();
        assert_eq!(
            cfg.defaults.worktree_root,
            tmp.path().join("proj-worktrees").to_string_lossy()
        );
        assert_eq!(
            cfg.defaults.baseline_path,
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
             baseline_ref = \"origin/main\"\n\
             baseline_path = \"\"\n",
        );
        let (from_root, _) = resolve_with_home(None, &proj, None).unwrap();
        let (from_nested, _) = resolve_with_home(None, &nested, None).unwrap();
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
             baseline_path = \"~/wt/_baseline\"\n"
            ),
        );
        let (cfg, _) = resolve_with_home(None, tmp.path(), None).unwrap();
        assert_eq!(cfg.defaults.worktree_root, ABS_W);
        let home = home_dir().expect("a home directory to expand `~` against");
        assert_eq!(
            cfg.defaults.baseline_path,
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
             baseline_path = \"\"\n",
        );
        let (cfg, _) = resolve_with_home(None, tmp.path(), None).unwrap();
        assert_eq!(
            cfg.defaults.baseline_path, "",
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
             baseline_ref = \"origin/main\"\n\
             baseline_path = \"\"\n",
        );
        let (cfg, _) = resolve_with_home(None, tmp.path(), None).unwrap();
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
             baseline_ref = \"origin/main\"\n\
             baseline_path = \"\"\n",
        );
        let err =
            resolve_with_home(None, tmp.path(), None).expect_err("unset var must fail the load");
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
             baseline_path = \"${DEVKIT_TEST_EMPTY}\"\n",
        );
        let (cfg, _) = resolve_with_home(None, tmp.path(), None).unwrap();
        assert_eq!(
            cfg.defaults.baseline_path, "",
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
             baseline_ref = \"origin/main\"\n\
             baseline_path = \"\"\n",
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
        let result = resolve_with_home(None, Path::new("."), None);
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
             baseline_ref = \"origin/main\"\nbaseline_path = \"\"\n\
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
             baseline_ref = \"origin/main\"\nbaseline_path = \"\"\n",
        )
        .unwrap();
        assert_eq!(c.tracker.kind, None, "absent means detect, not linear");
    }
}
