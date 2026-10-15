//! Canned oneshot tasks (`[tasks]`): resolve a named task against the config,
//! app catalog, and port registry into runnable plans, and execute command
//! plans in the foreground. Shared seam for the `devrun task` CLI (and any
//! future MCP surface).

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use devkit_common::{
    caller::Caller,
    record,
    required::{ensure_supplied, missing_args, required_of},
    template,
    vcs::{Vcs, VersionControl},
};
use devkit_config::{Config, Required, RunAction, RunArg, Step, TaskConfig};

use crate::{
    apps::App,
    registry::{self, Role},
    run,
};

/// A command task resolved to a runnable process: rendered argv, cwd, env.
#[derive(Debug, Clone)]
pub struct CommandPlan {
    pub name: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    /// The apps that must have a live server in the worktree before it
    /// runs. Enforced against the registry when the ports are reserved
    /// there for execution; an [`ExecutionPlan`] carries them unchecked.
    pub require_live: Vec<String>,
}

/// A resolved sequence step.
#[derive(Debug, Clone)]
pub enum SeqItem {
    Run(CommandPlan),
    Up(String),
}

/// A named task resolved for execution.
#[derive(Debug, Clone)]
pub enum Resolved {
    Command(CommandPlan),
    Sequence(Vec<SeqItem>),
}

/// Whether a task runs one command or a list of steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Command,
    Sequence,
}

impl TaskKind {
    pub fn label(self) -> &'static str {
        match self {
            TaskKind::Command => "command",
            TaskKind::Sequence => "sequence",
        }
    }
}

/// A task as every reader sees it. [`view`] builds it, and nothing else
/// decides whether a task can run.
pub struct TaskView<'a> {
    pub name: String,
    pub task: &'a TaskConfig,
    /// What the task is and needs, or why `devrun task` refuses it.
    pub checked: Result<Checked, String>,
}

/// What a well-formed task is and needs.
#[derive(Debug, Clone)]
pub struct Checked {
    pub kind: TaskKind,
    /// Every name its `run` and `env` templates read, across its steps for a
    /// sequence. An app's `static_env` renders for `devrun up` too, where no
    /// `--arg` exists, so it is left out.
    pub reads: BTreeSet<String>,
    /// The servers that must be live before it runs: its own `require_live`,
    /// or for a sequence its steps', minus any app an earlier `up` step
    /// starts.
    pub require_live: Vec<String>,
}

impl TaskView<'_> {
    /// The checked task, or its refusal as an error.
    pub fn runnable(&self) -> Result<&Checked> {
        self.checked.as_ref().map_err(|why| anyhow!("{why}"))
    }

    /// The variables it takes from `[templates.variables]` or `--arg`, each
    /// marked required or not for this caller. None for an invalid task.
    pub fn args(&self, cfg: &Config, caller: Caller) -> Vec<TaskArg> {
        match &self.checked {
            Ok(c) => arg_rows(cfg, Some(&self.name), args_among(c.reads.clone()), caller),
            Err(_) => Vec::new(),
        }
    }

    fn row(&self, cfg: &Config, caller: Caller) -> TaskRow {
        TaskRow {
            name: self.name.clone(),
            kind: self.checked.as_ref().map(|c| c.kind).map_err(Clone::clone),
            app: self.task.app.clone().unwrap_or_else(|| "-".into()),
            args: self.args(cfg, caller),
            require_live: self
                .checked
                .as_ref()
                .map(|c| c.require_live.clone())
                .unwrap_or_default(),
            description: self.task.description.clone().unwrap_or_default(),
        }
    }
}

/// Task `name` checked against the config and app catalog. Errs only for a
/// task that is not configured; a malformed one comes back carrying why.
///
/// Everything decidable before any variable has a value is checked here. Port
/// references are found by rendering, so [`resolve`] checks them against the
/// run's variables.
pub fn view<'a>(
    cfg: &'a Config,
    catalog: &HashMap<String, App>,
    name: &str,
) -> Result<TaskView<'a>> {
    let task = lookup(cfg, name)?;
    Ok(TaskView {
        name: name.to_string(),
        task,
        // A table cell or a doctor row holds one line.
        checked: check(cfg, catalog, name, task).map_err(|e| {
            format!("{e:#}")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        }),
    })
}

/// Every configured task's [`view`], sorted by name.
pub fn views<'a>(cfg: &'a Config, catalog: &HashMap<String, App>) -> Vec<TaskView<'a>> {
    let mut names: Vec<&String> = cfg.tasks.keys().collect();
    names.sort();
    names
        .into_iter()
        .map(|n| view(cfg, catalog, n).expect("a configured task"))
        .collect()
}

/// One row for the `devrun task` listing.
pub struct TaskRow {
    pub name: String,
    /// The task's kind, or why it cannot run.
    pub kind: Result<TaskKind, String>,
    pub app: String,
    pub args: Vec<TaskArg>,
    /// See [`Checked::require_live`].
    pub require_live: Vec<String>,
    pub description: String,
}

impl TaskRow {
    /// The kind as the listing prints it.
    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            Ok(k) => k.label(),
            Err(_) => "invalid",
        }
    }
}

/// A variable a task's or template's source reads, set with `--arg
/// name=value`.
#[derive(Debug, serde::Serialize)]
pub struct TaskArg {
    pub name: String,
    /// This caller cannot run the task without the `--arg`. Caller-relative:
    /// the same task lists a name bare for the caller it binds and bracketed
    /// for the one it does not.
    pub required: bool,
    /// Which callers cannot run the task without the `--arg`, whoever asks.
    pub required_of: Required,
    /// The `[templates.variables]` value used when no `--arg` is given.
    pub default: Option<String>,
    pub description: Option<String>,
}

impl TaskArg {
    /// The name as the listing prints it: bare when required, bracketed when
    /// optional.
    pub fn label(&self) -> String {
        if self.required {
            self.name.clone()
        } else {
            format!("[{}]", self.name)
        }
    }
}

/// Configured tasks sorted by name. A task [`view`] refuses is listed as
/// invalid with the reason rather than hidden, so a typo is visible in the
/// listing.
pub fn list(cfg: &Config, catalog: &HashMap<String, App>, caller: Caller) -> Vec<TaskRow> {
    views(cfg, catalog)
        .iter()
        .map(|v| v.row(cfg, caller))
        .collect()
}

/// Configured tasks as a text table, or a hint when none are configured.
/// Shared by `devrun task`/`devkit config tasks` and `devkit brief` so all
/// render identically. An optional arg is bracketed.
pub fn tasks_text(rows: &[TaskRow]) -> String {
    if rows.is_empty() {
        return "no tasks configured (add [tasks.<name>] to devkit.toml)\n".into();
    }
    let mut t = devkit_common::ui::table(&["NAME", "KIND", "APP", "ARGS", "DESCRIPTION"]);
    for r in rows {
        t.add_row(vec![
            r.name.clone(),
            r.kind_label().to_string(),
            r.app.clone(),
            args_text(&r.args),
            match &r.kind {
                Ok(_) => r.description.clone(),
                Err(why) => why.clone(),
            },
        ]);
    }
    t.to_string()
}

/// A task's args as the listing prints them: required ones bare, optional ones
/// bracketed, `-` for none.
pub fn args_text(args: &[TaskArg]) -> String {
    if args.is_empty() {
        return "-".into();
    }
    args.iter()
        .map(TaskArg::label)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Names the port registry supplies. The render context sets them above every
/// variable, so an `--arg` of either name could never take effect.
const PORT_NAMES: [&str; 2] = ["port", "ports"];
/// Names [`variables`] fills from the worktree. `--arg` can override them, but
/// they are never a task's args.
const ISSUE_FIELDS: [&str; 4] = ["ticket", "issue", "slug", "branch"];

/// `reads` minus the names the render context supplies itself.
fn args_among(mut reads: BTreeSet<String>) -> BTreeSet<String> {
    reads.retain(|n| !PORT_NAMES.contains(&n.as_str()) && !ISSUE_FIELDS.contains(&n.as_str()));
    reads
}

/// Every arg task `name` takes, each marked required or not for this caller.
pub fn task_args(
    cfg: &Config,
    catalog: &HashMap<String, App>,
    name: &str,
    caller: Caller,
) -> Result<Vec<TaskArg>> {
    let v = view(cfg, catalog, name)?;
    v.runnable()?;
    Ok(v.args(cfg, caller))
}

/// `names` as arg rows, each marked required or not for this caller under
/// `task`'s markings, or the variables' own when `task` is `None`.
pub fn arg_rows(
    cfg: &Config,
    task: Option<&str>,
    names: BTreeSet<String>,
    caller: Caller,
) -> Vec<TaskArg> {
    let required: BTreeSet<String> = missing_args(cfg, task, &names, &BTreeMap::new(), caller)
        .into_iter()
        .map(|m| m.name)
        .collect();
    names
        .into_iter()
        .map(|arg| TaskArg {
            required: required.contains(&arg),
            required_of: required_of(cfg, task, &arg),
            default: cfg
                .templates
                .variables
                .get(&arg)
                .and_then(|d| d.default_value())
                .map(str::to_string),
            description: devkit_common::required::description(cfg, &arg),
            name: arg,
        })
        .collect()
}

/// Task `name`'s listing row, refusing what [`resolve`] would refuse about
/// its shape instead of listing it as invalid.
pub fn describe(
    cfg: &Config,
    catalog: &HashMap<String, App>,
    name: &str,
    caller: Caller,
) -> Result<TaskRow> {
    let v = view(cfg, catalog, name)?;
    v.runnable()?;
    Ok(v.row(cfg, caller))
}

fn lookup<'a>(cfg: &'a Config, name: &str) -> Result<&'a TaskConfig> {
    cfg.tasks
        .get(name)
        .ok_or_else(|| anyhow!("unknown task `{name}` (run `devrun task` to list)"))
}

/// What [`view`] decides: the shape, the templates, the apps named, each step,
/// and the `required_args` entries.
fn check(
    cfg: &Config,
    catalog: &HashMap<String, App>,
    name: &str,
    t: &TaskConfig,
) -> Result<Checked> {
    let checked = match (!t.run.is_empty(), !t.steps.is_empty()) {
        (true, true) => bail!("task `{name}` sets both `run` and `steps`"),
        (false, false) => bail!("task `{name}` sets neither `run` nor `steps`"),
        (true, false) => check_command(catalog, name, t)?,
        (false, true) => check_sequence(cfg, catalog, name, t)?,
    };
    // An entry that silently guards nothing leaves the author believing it
    // does.
    let args = args_among(checked.reads.clone());
    for n in t.required_args.keys() {
        ensure!(
            args.contains(n),
            "task `{name}` lists `{n}` in required_args but reads no such arg"
        );
    }
    Ok(checked)
}

fn check_command(catalog: &HashMap<String, App>, name: &str, t: &TaskConfig) -> Result<Checked> {
    ensure!(
        matches!(t.run.first(), Some(RunArg::Scalar(_))),
        "task `{name}` program must be a plain string, not a table"
    );
    for entry in &t.run {
        match entry {
            RunArg::Scalar(_) => {}
            // Splitting on the empty pattern yields a boundary between every
            // character, so it would silently produce one argument per byte.
            RunArg::Action(RunAction::Split { on, .. }) => ensure!(
                !on.is_empty(),
                "task `{name}` has a split with an empty `on`"
            ),
        }
    }
    if let Some(a) = &t.app {
        ensure!(
            catalog.contains_key(a),
            "task `{name}` names unknown app `{a}`"
        );
    }
    for r in &t.require_live {
        ensure!(
            catalog.contains_key(r),
            "task `{name}` lists unknown app `{r}` in require_live"
        );
    }
    let mut templates: Vec<&str> = t.run.iter().map(RunArg::template).collect();
    templates.extend(t.env.values().map(String::as_str));
    Ok(Checked {
        kind: TaskKind::Command,
        reads: template::undeclared(&templates).with_context(|| format!("task `{name}`"))?,
        require_live: t.require_live.clone(),
    })
}

/// A sequence's steps must each be a well-formed command task or an app to
/// bring up. Its reads are its steps' reads.
fn check_sequence(
    cfg: &Config,
    catalog: &HashMap<String, App>,
    name: &str,
    t: &TaskConfig,
) -> Result<Checked> {
    ensure!(
        t.app.is_none() && t.env.is_empty() && t.require_live.is_empty(),
        "sequence task `{name}` may only set `description`, `steps`, and `required_args`"
    );
    let mut reads = BTreeSet::new();
    let mut require_live: Vec<String> = Vec::new();
    let mut up = BTreeSet::new();
    for step in &t.steps {
        match step {
            Step::Task(r) => {
                let sub = cfg
                    .tasks
                    .get(r)
                    .ok_or_else(|| anyhow!("task `{name}` references unknown task `{r}`"))?;
                ensure!(
                    !sub.run.is_empty() && sub.steps.is_empty(),
                    "task `{name}` references `{r}`, which is not a command task \
                     (sequences cannot nest)"
                );
                let step = check(cfg, catalog, r, sub)
                    .with_context(|| format!("task `{name}` runs `{r}`"))?;
                reads.extend(step.reads);
                for app in step.require_live {
                    if !up.contains(&app) && !require_live.contains(&app) {
                        require_live.push(app);
                    }
                }
            }
            Step::Up(app) => {
                ensure!(
                    catalog.contains_key(app),
                    "task `{name}` brings up unknown app `{app}`"
                );
                up.insert(app.clone());
            }
        }
    }
    Ok(Checked {
        kind: TaskKind::Sequence,
        reads,
        require_live,
    })
}

/// Refuse an `--arg` task `name` never reads and a required arg left unset
/// for this caller, before any step of it resolves.
fn check_args(
    cfg: &Config,
    name: &str,
    reads: &BTreeSet<String>,
    given: &BTreeMap<String, String>,
    caller: Caller,
) -> Result<()> {
    for k in given.keys() {
        ensure!(
            (reads.contains(k) && !PORT_NAMES.contains(&k.as_str()))
                || cfg.templates.variables.contains_key(k),
            "task `{name}` reads no variable `{k}`"
        );
    }
    let missing = missing_args(cfg, Some(name), &args_among(reads.clone()), given, caller);
    ensure_supplied(&format!("task `{name}`"), &missing)
}

/// The variables task templates render over, lowest first:
/// `[templates.variables]`, `ticket`/`issue`/`slug` from `.devkit/issue.toml`
/// and `branch` from the repository, then `--arg`. An issue field with no
/// source stays undefined rather than empty.
fn variables(
    cfg: &Config,
    worktree_root: &Path,
    args: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut vars = cfg.templates.defaults();
    if let Some(r) = record::read(worktree_root) {
        vars.insert("ticket".into(), r.issue.clone());
        vars.insert("issue".into(), r.issue);
        vars.insert("slug".into(), r.slug);
    }
    if let Ok(branch) = Vcs::at(worktree_root).branch(worktree_root) {
        vars.insert("branch".into(), branch);
    }
    vars.extend(args.iter().map(|(k, v)| (k.clone(), v.clone())));
    vars
}

/// Resolve task `name` for execution in `worktree_root`. Command tasks get
/// their port references allocated (workspace role, pid-less reservations for
/// apps not yet running) and their templates rendered; sequence tasks
/// resolve each step. All validation errors fire here, before anything
/// spawns: what [`view`] refuses, an `--arg` in `args` the task never reads, a
/// required one missing from it, and a port reference the render turns up.
#[allow(clippy::too_many_arguments)]
pub fn resolve(
    cfg: &Config,
    catalog: &HashMap<String, App>,
    worktree_root: &Path,
    holder: &str,
    name: &str,
    user_env: &BTreeMap<String, String>,
    args: &BTreeMap<String, String>,
    caller: Caller,
) -> Result<Resolved> {
    let v = view(cfg, catalog, name)?;
    let checked = v.runnable()?;
    check_args(cfg, name, &checked.reads, args, caller)?;
    let vars = variables(cfg, worktree_root, args);
    let command = |name: &str, t: &TaskConfig| {
        resolve_command(
            &vars,
            catalog,
            worktree_root,
            &Ports::Registry {
                holder,
                live: false,
            },
            name,
            t,
            user_env,
        )
    };
    match checked.kind {
        TaskKind::Command => Ok(Resolved::Command(command(name, v.task)?)),
        TaskKind::Sequence => v
            .task
            .steps
            .iter()
            .map(|step| match step {
                Step::Task(r) => Ok(SeqItem::Run(command(r, &cfg.tasks[r])?)),
                Step::Up(app) => Ok(SeqItem::Up(app.clone())),
            })
            .collect::<Result<_>>()
            .map(Resolved::Sequence),
    }
}

/// Env templates a command task will render: `static_env` overlaid by the
/// task's `env`, minus any key the user's `--env` supplies. An overridden
/// value is neither scanned for port references nor rendered, so a port it
/// references neither allocates a reservation nor arms the liveness gate.
fn effective_env<'a>(
    static_env: &'a HashMap<String, String>,
    t: &'a TaskConfig,
    user_env: &BTreeMap<String, String>,
) -> BTreeMap<&'a str, &'a str> {
    let mut m: BTreeMap<&'a str, &'a str> = BTreeMap::new();
    for (k, v) in static_env {
        m.insert(k.as_str(), v.as_str());
    }
    for (k, v) in &t.env {
        m.insert(k.as_str(), v.as_str());
    }
    m.retain(|k, _| !user_env.contains_key(*k));
    m
}

/// Where a command task's port references take their ports from.
enum Ports<'a> {
    /// Reserved in the registry for `holder`, with `require_live` enforced
    /// when `live` is set.
    Registry { holder: &'a str, live: bool },
    /// Each app's base port, with no registry touched: what an
    /// [`ExecutionPlan`] renders, since the ports a run binds are not part
    /// of what the task is. `require_live` is left unchecked and carried
    /// in the plan.
    Base,
}

impl Ports<'_> {
    fn take(&self, wanted: &[(String, u16)]) -> Result<BTreeMap<String, u16>> {
        match self {
            Ports::Registry { holder, .. } if !wanted.is_empty() => {
                Ok(registry::alloc(holder, wanted, Role::Workspace)?
                    .into_iter()
                    .collect())
            }
            Ports::Registry { .. } | Ports::Base => Ok(wanted.iter().cloned().collect()),
        }
    }
}

/// Discovery + allocation for one command task [`view`] accepted, then
/// delegate to the pure renderer. `ports[...]` references and (if `{{ port }}`
/// is used) the task's own app are allocated in one `registry::alloc` call.
#[allow(clippy::too_many_arguments)]
fn resolve_command(
    vars: &BTreeMap<String, String>,
    catalog: &HashMap<String, App>,
    worktree_root: &Path,
    ports: &Ports<'_>,
    name: &str,
    t: &TaskConfig,
    user_env: &BTreeMap<String, String>,
) -> Result<CommandPlan> {
    let app = t.app.as_deref().map(|a| &catalog[a]);
    let static_env = app.map(|a| a.static_env.clone()).unwrap_or_default();
    let env_templates = effective_env(&static_env, t, user_env);

    let empty_user_env = BTreeMap::new();
    let unfiltered_env = effective_env(&static_env, t, &empty_user_env);
    let mut all_templates: Vec<&str> = t.run.iter().map(RunArg::template).collect();
    all_templates.extend(unfiltered_env.values().copied());
    let all_refs = template::referenced_ports(&all_templates, vars)
        .with_context(|| format!("scanning require_live templates of task `{name}`"))?;
    for r in &t.require_live {
        ensure!(
            all_refs.apps.contains(r),
            "task `{name}` lists `{r}` in require_live but never references `ports['{r}']`"
        );
    }

    let mut templates: Vec<&str> = t.run.iter().map(RunArg::template).collect();
    templates.extend(env_templates.values().copied());
    let refs = template::referenced_ports(&templates, vars)
        .with_context(|| format!("scanning templates of task `{name}`"))?;

    ensure!(
        !refs.own_port || app.is_some(),
        "task `{name}` uses `{{{{ port }}}}` but has no `app`"
    );
    let mut names: Vec<String> = refs.apps.iter().cloned().collect();
    for r in &names {
        ensure!(
            catalog.contains_key(r),
            "task `{name}` references unknown app `{r}` via ports[...]"
        );
    }
    if refs.own_port {
        let own = app.expect("checked above").name.clone();
        if !names.contains(&own) {
            names.push(own);
        }
    }

    let gated: Vec<String> = t
        .require_live
        .iter()
        .filter(|r| refs.apps.contains(*r))
        .cloned()
        .collect();
    if let Ports::Registry { holder, live: true } = ports
        && !gated.is_empty()
    {
        let data = registry::snapshot()?;
        for r in &gated {
            ensure!(
                registry::live_port(&data, holder, r).is_some(),
                "require_live: `{r}` has no live server in this worktree (devrun up {r})"
            );
        }
    }

    let reqs: Vec<(String, u16)> = names
        .iter()
        .map(|n| (n.clone(), catalog[n].base_port))
        .collect();
    let ports = ports.take(&reqs)?;
    let own_port = refs
        .own_port
        .then(|| ports[&app.expect("checked above").name]);

    let plan = resolve_command_with_ports(
        name,
        t,
        &env_templates,
        worktree_root,
        app.map(|a| a.path.as_str()),
        &ports,
        own_port,
        vars,
        user_env,
    )?;
    Ok(CommandPlan {
        require_live: gated,
        ..plan
    })
}

/// Resolve command task `name` for immediate execution: fresh allocation,
/// fresh render, `require_live` enforced. Sequences call this per step at
/// execution time so a long-running earlier step cannot expire the ports an
/// upfront render used; standalone commands call it right before exec. `args`
/// were already checked by [`resolve`].
pub fn resolve_step(
    cfg: &Config,
    catalog: &HashMap<String, App>,
    worktree_root: &Path,
    holder: &str,
    name: &str,
    user_env: &BTreeMap<String, String>,
    args: &BTreeMap<String, String>,
) -> Result<CommandPlan> {
    let v = view(cfg, catalog, name)?;
    ensure!(
        v.runnable()?.kind == TaskKind::Command,
        "task `{name}` is not a command task"
    );
    let vars = variables(cfg, worktree_root, args);
    resolve_command(
        &vars,
        catalog,
        worktree_root,
        &Ports::Registry { holder, live: true },
        name,
        v.task,
        user_env,
    )
}

/// What a run of a task executes once every config layer has merged and
/// every template rendered: per step, the command task's app, argv, cwd,
/// env and the servers it requires live, or the servers an `up` step
/// launches; and which callers its args satisfy, since a run's arg check
/// depends on who runs it. Ports render as each app's base port, so no
/// registry is touched. Rendered env values may be secrets, so a plan has
/// no `Debug`, and [`plan_diff`] compares them without printing them.
pub struct ExecutionPlan {
    /// Whether the args pass the check `devrun task` makes of a human
    /// caller and of an agent caller, in that order.
    accepts: [bool; 2],
    steps: Vec<PlannedStep>,
}

enum PlannedStep {
    Run {
        task: String,
        app: Option<String>,
        argv: Vec<String>,
        cwd: String,
        env: BTreeMap<String, PlannedEnv>,
        require_live: Vec<String>,
    },
    Up {
        app: String,
        launches: Vec<PlannedLaunch>,
    },
}

struct PlannedLaunch {
    app: String,
    port: u16,
    argv: Vec<String>,
    cwd: String,
    env: BTreeMap<String, PlannedEnv>,
    url: String,
}

/// One env variable of a plan: the template config gave it, when one did,
/// and the value it rendered to, which may be a secret.
struct PlannedEnv {
    template: Option<String>,
    value: String,
}

/// One line of a plan's readable form. `secret` holds the rendered env
/// value behind it, compared but never printed.
#[derive(PartialEq, Eq)]
struct PlanLine {
    text: String,
    secret: Option<String>,
}

impl ExecutionPlan {
    fn lines(&self) -> Vec<PlanLine> {
        let plain = |text: String| PlanLine { text, secret: None };
        let mut out = Vec::new();
        let callers: Vec<&str> = ["human", "agent"]
            .into_iter()
            .zip(self.accepts)
            .filter_map(|(who, ok)| ok.then_some(who))
            .collect();
        out.push(plain(format!(
            "args accepted from: {}",
            if callers.is_empty() {
                "nobody".to_string()
            } else {
                callers.join(", ")
            }
        )));
        let quoted = |argv: &[String]| serde_json::to_string(argv).expect("strings serialize");
        let env_lines = |indent: &str, env: &BTreeMap<String, PlannedEnv>| -> Vec<PlanLine> {
            env.iter()
                .map(|(key, e)| PlanLine {
                    text: match &e.template {
                        Some(t) => format!(
                            "{indent}env {key} = {}",
                            serde_json::to_string(t).expect("a string serializes")
                        ),
                        None => format!("{indent}env {key}"),
                    },
                    secret: Some(e.value.clone()),
                })
                .collect()
        };
        for (i, step) in self.steps.iter().enumerate() {
            let n = i + 1;
            match step {
                PlannedStep::Run {
                    task,
                    app,
                    argv,
                    cwd,
                    env,
                    require_live,
                } => {
                    out.push(plain(format!("step {n}: task {task}")));
                    if let Some(app) = app {
                        out.push(plain(format!("  app: {app}")));
                    }
                    out.push(plain(format!("  argv: {}", quoted(argv))));
                    out.push(plain(format!("  cwd: {cwd}")));
                    out.extend(env_lines("  ", env));
                    if !require_live.is_empty() {
                        out.push(plain(format!(
                            "  require_live: {}",
                            require_live.join(", ")
                        )));
                    }
                }
                PlannedStep::Up { app, launches } => {
                    out.push(plain(format!("step {n}: up {app}")));
                    for l in launches {
                        out.push(plain(format!("  launch {} on port {}", l.app, l.port)));
                        out.push(plain(format!("    argv: {}", quoted(&l.argv))));
                        out.push(plain(format!("    cwd: {}", l.cwd)));
                        out.extend(env_lines("    ", &l.env));
                        out.push(plain(format!("    url: {}", l.url)));
                    }
                }
            }
        }
        out
    }
}

/// The readable difference between what a task executes under `base` and
/// under `head`, one line each, or nothing when they execute the same.
/// A line `head` drops starts `- `, one it adds `+ `, and a step header
/// they share `  `, for context. An env variable whose template is the
/// same but whose rendered value differs reads `~ ` with the template and
/// `(rendered value changed)`: rendered values are never printed.
pub fn plan_diff(base: &ExecutionPlan, head: &ExecutionPlan) -> Vec<String> {
    let (old, new) = (base.lines(), head.lines());
    if old == new {
        return Vec::new();
    }
    let mut lcs = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut ops: Vec<(char, &PlanLine)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < old.len() || j < new.len() {
        if i < old.len() && j < new.len() && old[i] == new[j] {
            ops.push((' ', &old[i]));
            i += 1;
            j += 1;
        } else if i < old.len() && (j == new.len() || lcs[i + 1][j] >= lcs[i][j + 1]) {
            ops.push(('-', &old[i]));
            i += 1;
        } else {
            ops.push(('+', &new[j]));
            j += 1;
        }
    }
    // Within each run of changed lines, a dropped and an added line with
    // the same text differ only in their hidden rendered value.
    let mut paired = vec![false; ops.len()];
    let mut start = 0;
    while start < ops.len() {
        let end = (start..ops.len())
            .find(|&k| ops[k].0 == ' ')
            .unwrap_or(ops.len());
        for k in start..end {
            if ops[k].0 != '+' {
                continue;
            }
            if let Some(d) = (start..end)
                .find(|&d| ops[d].0 == '-' && !paired[d] && ops[d].1.text == ops[k].1.text)
            {
                paired[d] = true;
                paired[k] = true;
            }
        }
        start = end + 1;
    }
    let mut out = Vec::new();
    for (k, &(op, line)) in ops.iter().enumerate() {
        match (op, paired[k]) {
            (' ', _) if line.text.starts_with(' ') => {}
            ('-', true) => {}
            ('+', true) => out.push(format!("~ {} (rendered value changed)", line.text)),
            (op, _) => out.push(format!("{op} {}", line.text)),
        }
    }
    out
}

/// `path` relative to `root`, with `/` between components, or `.` for
/// `root` itself; `path` whole when it lies outside `root`.
fn relative(path: &Path, root: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".to_string(),
        Ok(rel) => rel
            .iter()
            .map(|c| c.to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => path.display().to_string(),
    }
}

/// The [`ExecutionPlan`] of task `name` given `args` at `worktree_root`,
/// rendered by the code `devrun task` and `devrun up` run it through.
fn execution_plan(
    cfg: &Config,
    catalog: &HashMap<String, App>,
    worktree_root: &Path,
    name: &str,
    args: &BTreeMap<String, String>,
) -> Result<ExecutionPlan> {
    let v = view(cfg, catalog, name)?;
    let checked = v.runnable()?;
    let accepts = [Caller::Human, Caller::Agent]
        .map(|caller| check_args(cfg, name, &checked.reads, args, caller).is_ok());
    let vars = variables(cfg, worktree_root, args);
    let command = |step: &str, t: &TaskConfig| -> Result<PlannedStep> {
        let plan = resolve_command(
            &vars,
            catalog,
            worktree_root,
            &Ports::Base,
            step,
            t,
            &BTreeMap::new(),
        )?;
        let static_env = t
            .app
            .as_deref()
            .map(|a| catalog[a].static_env.clone())
            .unwrap_or_default();
        let templates = effective_env(&static_env, t, &BTreeMap::new());
        Ok(PlannedStep::Run {
            task: plan.name,
            app: t.app.clone(),
            argv: plan.argv,
            cwd: relative(&plan.cwd, worktree_root),
            env: planned_env(plan.env, |k| templates.get(k).map(|t| t.to_string())),
            require_live: plan.require_live,
        })
    };
    let steps = match checked.kind {
        TaskKind::Command => vec![command(name, v.task)?],
        TaskKind::Sequence => v
            .task
            .steps
            .iter()
            .map(|step| match step {
                Step::Task(r) => command(r, &cfg.tasks[r]),
                Step::Up(app) => planned_up(cfg, catalog, worktree_root, app),
            })
            .collect::<Result<_>>()?,
    };
    Ok(ExecutionPlan { accepts, steps })
}

/// `env` with each value paired with the template `template` gives its key.
fn planned_env(
    env: BTreeMap<String, String>,
    template: impl Fn(&str) -> Option<String>,
) -> BTreeMap<String, PlannedEnv> {
    env.into_iter()
        .map(|(k, value)| {
            let template = template(&k);
            (k, PlannedEnv { template, value })
        })
        .collect()
}

/// The launches an `up` step of `app` makes, as `devrun up` plans them for
/// the workspace role.
fn planned_up(
    cfg: &Config,
    catalog: &HashMap<String, App>,
    worktree_root: &Path,
    app: &str,
) -> Result<PlannedStep> {
    let mut apps = vec![app.to_string()];
    run::ensure_provider(catalog, &mut apps);
    let vars = cfg.templates.defaults();
    let ports: BTreeMap<String, u16> = run::wanted_ports(catalog, &apps, &vars)?
        .into_iter()
        .collect();
    let provider = run::url_provider(catalog);
    let launches = run::plan_group(
        catalog,
        &apps,
        &ports,
        provider,
        worktree_root,
        Role::Workspace,
        &BTreeMap::new(),
        &vars,
    )?;
    Ok(PlannedStep::Up {
        app: app.to_string(),
        launches: launches
            .into_iter()
            .map(|p| {
                let static_env = &catalog[&p.app].static_env;
                PlannedLaunch {
                    port: p.port,
                    argv: p.argv,
                    cwd: relative(&p.cwd, worktree_root),
                    env: planned_env(p.env, |k| static_env.get(k).cloned()),
                    url: p.url,
                    app: p.app,
                }
            })
            .collect(),
    })
}

/// The [`ExecutionPlan`] of task `name` given `args` at `worktree_root`, as
/// the tracked `devkit.toml` layers committed at `rev` define it for a run
/// started in `dir` ([`crate::ledger::committed_config`]), with no other
/// layer: not the working tree's, the primary checkout's, the home config
/// or a `devkit.local.toml`. `checkout` is any checkout of the repository.
pub fn committed_plan(
    checkout: &Path,
    rev: &str,
    dir: &str,
    worktree_root: &Path,
    name: &str,
    args: &BTreeMap<String, String>,
) -> Result<ExecutionPlan> {
    let cfg = crate::ledger::committed_config(checkout, rev, dir)?;
    let (catalog, _) = crate::apps::catalog(&cfg, &crate::load::path_to_project(&cfg)?)?;
    execution_plan(&cfg, &catalog, worktree_root, name, args)
}

/// The argv `run` of command task `name` renders to.
fn render_argv(
    name: &str,
    t: &TaskConfig,
    own_port: Option<u16>,
    ports: &BTreeMap<String, u16>,
    variables: &BTreeMap<String, String>,
) -> Result<Vec<String>> {
    let mut argv = Vec::new();
    for entry in &t.run {
        let rendered = template::render_launch(entry.template(), own_port, ports, variables)
            .with_context(|| format!("rendering `run` of task `{name}`"))?;
        match entry {
            RunArg::Scalar(_) => argv.push(rendered),
            // An empty render is an empty list, not a list holding one empty
            // argument: a task staging a caller-supplied set of paths has to
            // be able to express "none".
            RunArg::Action(RunAction::Split { on, .. }) if !rendered.is_empty() => {
                argv.extend(rendered.split(on.as_str()).map(str::to_string));
            }
            RunArg::Action(RunAction::Split { .. }) => {}
        }
    }
    ensure!(
        argv.first().is_some_and(|p| !p.is_empty()),
        "task `{name}` has an empty program"
    );
    Ok(argv)
}

/// Render one command task against an already-resolved port map. Registry-free
/// so tests exercise rendering, layering, and the prd guard directly.
#[allow(clippy::too_many_arguments)]
fn resolve_command_with_ports(
    name: &str,
    t: &TaskConfig,
    env_templates: &BTreeMap<&str, &str>,
    worktree_root: &Path,
    app_path: Option<&str>,
    ports: &BTreeMap<String, u16>,
    own_port: Option<u16>,
    variables: &BTreeMap<String, String>,
    user_env: &BTreeMap<String, String>,
) -> Result<CommandPlan> {
    let argv = render_argv(name, t, own_port, ports, variables)?;

    let mut env = BTreeMap::new();
    for (k, v) in env_templates {
        env.insert(
            (*k).to_string(),
            template::render_launch(v, own_port, ports, variables)
                .with_context(|| format!("rendering env `{k}` of task `{name}`"))?,
        );
    }
    for (k, v) in user_env {
        env.insert(k.clone(), v.clone());
    }

    let cwd = match app_path {
        Some(p) => worktree_root.join(p),
        None => worktree_root.to_path_buf(),
    };
    run::assert_not_prd(name, &argv, &env, &cwd)?;
    Ok(CommandPlan {
        name: name.into(),
        argv,
        cwd,
        env,
        require_live: Vec::new(),
    })
}

/// Run a command plan in the foreground, its env overlaid on the process
/// environment, keeping the tail of its output in `tail`. An output stream
/// not attached to a terminal is captured into a file under the state
/// directory, followed while the task runs and echoed to the same stream of
/// devrun's; a terminal stream is inherited, so the task still sees a
/// terminal. When stdout and stderr are one file, both streams share one
/// capture echoed to stdout. Returns the child's exit status.
///
/// A file rather than a pipe, so a process the task leaves in the background
/// keeps a writable output after devrun exits instead of dying on a closed
/// pipe, and devrun returns as soon as the task itself does.
pub fn exec(
    plan: &CommandPlan,
    tail: &std::sync::Arc<crate::ledger::Tail>,
) -> Result<std::process::ExitStatus> {
    use std::{
        io::IsTerminal,
        process::Command,
        sync::{Arc, atomic::AtomicBool},
    };

    use crate::ledger::Capture;

    let mut cmd = Command::new(&plan.argv[0]);
    cmd.args(&plan.argv[1..])
        .current_dir(&plan.cwd)
        .envs(&plan.env);
    let exited = Arc::new(AtomicBool::new(false));
    let (out_tty, err_tty) = (
        std::io::stdout().is_terminal(),
        std::io::stderr().is_terminal(),
    );
    let shared = !out_tty && !err_tty && outputs_share_a_file();
    let out = (!out_tty).then(Capture::open).flatten();
    let err = (!err_tty && !shared).then(Capture::open).flatten();
    if let Some(c) = &out {
        cmd.stdout(c.writer()?);
        if shared {
            cmd.stderr(c.writer()?);
        }
    }
    if let Some(c) = &err {
        cmd.stderr(c.writer()?);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("running task `{}` ({})", plan.name, plan.argv.join(" ")))?;
    let followers = [
        out.map(|c| c.follow(tail, &exited, std::io::stdout)),
        err.map(|c| c.follow(tail, &exited, std::io::stderr)),
    ];
    let status = child.wait();
    exited.store(true, std::sync::atomic::Ordering::Release);
    for follower in followers.into_iter().flatten() {
        let _ = follower.join();
    }
    status.with_context(|| format!("waiting for task `{}`", plan.name))
}

/// Whether devrun's stdout and stderr are one file, as `2>&1` leaves them.
/// The task then writes both streams into one capture, so they keep their
/// order.
#[cfg(unix)]
fn outputs_share_a_file() -> bool {
    use std::os::{
        fd::{AsFd, BorrowedFd},
        unix::fs::MetadataExt,
    };
    let id = |fd: BorrowedFd<'_>| {
        let meta = std::fs::File::from(fd.try_clone_to_owned().ok()?)
            .metadata()
            .ok()?;
        Some((meta.dev(), meta.ino()))
    };
    let out = id(std::io::stdout().as_fd());
    out.is_some() && out == id(std::io::stderr().as_fd())
}

#[cfg(not(unix))]
fn outputs_share_a_file() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use devkit_config::{Config, Step, TaskConfig};

    use super::*;

    #[test]
    fn tasks_text_renders_rows_or_hint() {
        let rows = vec![
            TaskRow {
                name: "check".into(),
                kind: Ok(TaskKind::Sequence),
                app: "-".into(),
                args: vec![],
                require_live: vec![],
                description: "lint then test".into(),
            },
            TaskRow {
                name: "lint".into(),
                kind: Ok(TaskKind::Command),
                app: "api".into(),
                args: vec![],
                require_live: vec![],
                description: String::new(),
            },
            TaskRow {
                name: "typo".into(),
                kind: Err("task `typo` sets both `run` and `steps`".into()),
                app: "-".into(),
                args: vec![],
                require_live: vec![],
                description: "never shown".into(),
            },
        ];
        let text = tasks_text(&rows);
        assert!(text.contains("NAME") && text.contains("KIND"), "{text}");
        assert!(text.contains("check") && text.contains("lint"), "{text}");
        let typo = text.lines().find(|l| l.contains("typo")).unwrap();
        assert!(
            typo.contains("invalid") && typo.contains("sets both"),
            "{typo}"
        );
        let empty = tasks_text(&[]);
        assert!(empty.contains("no tasks configured"), "{empty}");
    }

    fn cfg_with(tasks: &[(&str, TaskConfig)]) -> Config {
        let mut c = Config::default();
        for (n, t) in tasks {
            c.tasks.insert(n.to_string(), t.clone());
        }
        c
    }

    fn command_task(app: Option<&str>, run: &[&str], env: &[(&str, &str)]) -> TaskConfig {
        TaskConfig {
            app: app.map(String::from),
            run: run.iter().map(|s| (*s).into()).collect(),
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..TaskConfig::default()
        }
    }

    fn api_catalog() -> HashMap<String, App> {
        let mut m = HashMap::new();
        m.insert("api-prod".to_string(), App {
            name: "api-prod".into(),
            base_port: 9101,
            path: "apps/api".into(),
            launch: vec![],
            url: None,
            url_env: None,
            provides_url: false,
            static_env: [("FROM_APP".to_string(), "static".to_string())].into(),
            prep_files: vec![],
            setup: vec![],
        });
        m
    }

    #[test]
    fn command_env_layering_static_then_task_then_user() {
        let t = command_task(Some("api-prod"), &["git", "version"], &[(
            "FROM_APP", "task",
        )]);
        let user: BTreeMap<String, String> = [("FROM_APP".to_string(), "user".to_string())].into();
        let cat = api_catalog();
        let env_templates = effective_env(&cat["api-prod"].static_env, &t, &user);
        let plan = resolve_command_with_ports(
            "t",
            &t,
            &env_templates,
            Path::new("/wt"),
            Some("apps/api"),
            &BTreeMap::new(),
            None,
            &BTreeMap::new(),
            &user,
        )
        .unwrap();
        assert_eq!(plan.env["FROM_APP"], "user");
        assert_eq!(plan.cwd, Path::new("/wt").join("apps/api"));

        let no_user = BTreeMap::new();
        let env_templates2 = effective_env(&cat["api-prod"].static_env, &t, &no_user);
        let plan2 = resolve_command_with_ports(
            "t",
            &t,
            &env_templates2,
            Path::new("/wt"),
            Some("apps/api"),
            &BTreeMap::new(),
            None,
            &BTreeMap::new(),
            &no_user,
        )
        .unwrap();
        assert_eq!(plan2.env["FROM_APP"], "task");
    }

    #[test]
    fn command_renders_ports_in_env_and_argv() {
        let t = command_task(
            None,
            &["git", "--url", "http://localhost:{{ ports['api-prod'] }}"],
            &[("BASE", "http://localhost:{{ ports['api-prod'] }}")],
        );
        let ports: BTreeMap<String, u16> = [("api-prod".to_string(), 9101)].into();
        let no_static = HashMap::new();
        let no_user = BTreeMap::new();
        let env_templates = effective_env(&no_static, &t, &no_user);
        let plan = resolve_command_with_ports(
            "t",
            &t,
            &env_templates,
            Path::new("/wt"),
            None,
            &ports,
            None,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(plan.argv[2], "http://localhost:9101");
        assert_eq!(plan.env["BASE"], "http://localhost:9101");
        assert_eq!(plan.cwd, Path::new("/wt"));
    }

    #[test]
    fn command_prd_doppler_is_rejected() {
        let t = command_task(None, &["doppler", "run", "-c", "prd", "--", "x"], &[]);
        let no_static = HashMap::new();
        let no_user = BTreeMap::new();
        let env_templates = effective_env(&no_static, &t, &no_user);
        let err = resolve_command_with_ports(
            "t",
            &t,
            &env_templates,
            Path::new("/wt"),
            None,
            &BTreeMap::new(),
            None,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("prd"));
    }

    #[test]
    fn the_listing_refuses_exactly_what_resolve_refuses_and_says_why() {
        let both = TaskConfig {
            run: vec!["git".into()],
            steps: vec![Step::Up("api-prod".into())],
            ..TaskConfig::default()
        };
        let neither = TaskConfig::default();
        let seq_with_app = TaskConfig {
            app: Some("api-prod".into()),
            steps: vec![Step::Up("api-prod".into())],
            ..TaskConfig::default()
        };
        let step_to_sequence = TaskConfig {
            steps: vec![Step::Task("seq".into())],
            ..TaskConfig::default()
        };
        let seq = TaskConfig {
            steps: vec![Step::Up("api-prod".into())],
            ..TaskConfig::default()
        };
        let seq_with_require_live = TaskConfig {
            require_live: vec!["api-prod".into()],
            steps: vec![Step::Up("api-prod".into())],
            ..TaskConfig::default()
        };
        let empty_split = TaskConfig {
            run: vec![
                "git".into(),
                RunArg::Action(RunAction::Split {
                    split: "{{ files }}".into(),
                    on: String::new(),
                }),
            ],
            ..TaskConfig::default()
        };
        let unknown_step = TaskConfig {
            steps: vec![Step::Task("nope".into())],
            ..TaskConfig::default()
        };
        let bad_step = TaskConfig {
            steps: vec![Step::Task("empty-split".into())],
            ..TaskConfig::default()
        };
        let self_step = TaskConfig {
            steps: vec![Step::Task("self-step".into())],
            ..TaskConfig::default()
        };
        let cfg = cfg_with(&[
            ("both", both),
            ("neither", neither),
            ("seq-with-app", seq_with_app),
            ("nested", step_to_sequence),
            ("seq", seq),
            ("seq-require-live", seq_with_require_live),
            ("empty-split", empty_split),
            ("unknown-step", unknown_step),
            ("bad-step", bad_step),
            ("self-step", self_step),
        ]);
        let cat = api_catalog();
        let u = BTreeMap::new();
        let rows = list(&cfg, &cat, Caller::Agent);
        for row in &rows {
            let resolved = resolve(
                &cfg,
                &cat,
                Path::new("/wt"),
                "/wt",
                &row.name,
                &u,
                &u,
                Caller::Agent,
            );
            match (&row.kind, resolved) {
                (Ok(_), Ok(_)) => assert_eq!(row.name, "seq"),
                (Err(why), Err(e)) => assert_eq!(why, &format!("{e:#}"), "{}", row.name),
                (kind, resolved) => panic!("{}: listed {kind:?}, resolved {resolved:?}", row.name),
            }
        }
        assert!(
            resolve(
                &cfg,
                &cat,
                Path::new("/wt"),
                "/wt",
                "missing",
                &u,
                &u,
                Caller::Agent
            )
            .is_err()
        );
    }

    #[test]
    fn resolve_sequence_maps_steps() {
        let build = command_task(None, &["git", "version"], &[]);
        let seq = TaskConfig {
            steps: vec![Step::Task("build".into()), Step::Up("api-prod".into())],
            ..TaskConfig::default()
        };
        let cfg = cfg_with(&[("build", build), ("seq", seq)]);
        let r = resolve(
            &cfg,
            &api_catalog(),
            Path::new("/wt"),
            "/wt",
            "seq",
            &BTreeMap::new(),
            &BTreeMap::new(),
            Caller::Agent,
        )
        .unwrap();
        match r {
            Resolved::Sequence(items) => {
                assert!(matches!(&items[0], SeqItem::Run(p) if p.argv == ["git", "version"]));
                assert!(matches!(&items[1], SeqItem::Up(a) if a == "api-prod"));
            }
            _ => panic!("expected sequence"),
        }
    }

    #[test]
    fn effective_env_merges_and_drops_overridden_keys() {
        let static_env: HashMap<String, String> = [
            ("A".to_string(), "from-static".to_string()),
            ("B".to_string(), "from-static".to_string()),
        ]
        .into();
        let t = command_task(None, &["git"], &[("B", "from-task"), ("C", "from-task")]);
        let user: BTreeMap<String, String> = [("C".to_string(), "x".to_string())].into();
        let m = effective_env(&static_env, &t, &user);
        assert_eq!(m["A"], "from-static");
        assert_eq!(m["B"], "from-task");
        assert!(!m.contains_key("C"));
    }

    #[test]
    fn overridden_env_key_is_not_rendered() {
        // BASE references a port that is NOT in the ports map; rendering it
        // would error. The user override must make that value irrelevant.
        let t = command_task(None, &["git"], &[(
            "BASE",
            "http://localhost:{{ ports['api-prod'] }}",
        )]);
        let user: BTreeMap<String, String> =
            [("BASE".to_string(), "https://preview".to_string())].into();
        let no_static = HashMap::new();
        let env_templates = effective_env(&no_static, &t, &user);
        let plan = resolve_command_with_ports(
            "t",
            &t,
            &env_templates,
            Path::new("/wt"),
            None,
            &BTreeMap::new(),
            None,
            &BTreeMap::new(),
            &user,
        )
        .unwrap();
        assert_eq!(plan.env["BASE"], "https://preview");
    }

    #[test]
    fn list_reports_kind_and_sorts() {
        let cfg = cfg_with(&[
            ("b-cmd", command_task(None, &["git"], &[])),
            ("a-seq", TaskConfig {
                description: Some("d".into()),
                steps: vec![Step::Up("api-prod".into())],
                ..TaskConfig::default()
            }),
        ]);
        let rows = list(&cfg, &api_catalog(), Caller::Agent);
        assert_eq!(rows[0].name, "a-seq");
        assert_eq!(rows[0].kind, Ok(TaskKind::Sequence));
        assert_eq!(rows[0].description, "d");
        assert_eq!(rows[1].kind, Ok(TaskKind::Command));
    }

    #[test]
    fn require_live_unknown_app_errors() {
        let mut t = command_task(None, &["git", "version"], &[]);
        t.require_live = vec!["nope".into()];
        let cfg = cfg_with(&[("t", t)]);
        let err = resolve(
            &cfg,
            &api_catalog(),
            Path::new("/wt"),
            "/wt",
            "t",
            &BTreeMap::new(),
            &BTreeMap::new(),
            Caller::Agent,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("unknown app `nope`"));
    }

    #[test]
    fn resolve_step_rejects_non_command_tasks() {
        let seq = TaskConfig {
            steps: vec![Step::Up("api-prod".into())],
            ..TaskConfig::default()
        };
        let cfg = cfg_with(&[("seq", seq)]);
        let err = resolve_step(
            &cfg,
            &api_catalog(),
            Path::new("/wt"),
            "/wt",
            "seq",
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("not a command task"));
    }

    #[test]
    fn require_live_shadowed_static_ref_errors() {
        // static_env has a port reference, but the task's own `env` shadows
        // the same key with a plain literal. The rendered value never
        // touches the port, so require_live must not be satisfied by it.
        let mut cat = api_catalog();
        cat.get_mut("api-prod").unwrap().static_env.insert(
            "URL".to_string(),
            "http://localhost:{{ ports['api-prod'] }}".to_string(),
        );
        let mut t = command_task(Some("api-prod"), &["git", "version"], &[("URL", "static")]);
        t.require_live = vec!["api-prod".into()];
        let cfg = cfg_with(&[("t", t)]);
        let err = resolve(
            &cfg,
            &cat,
            Path::new("/wt"),
            "/wt",
            "t",
            &BTreeMap::new(),
            &BTreeMap::new(),
            Caller::Agent,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("never references"));
    }

    #[test]
    fn an_up_step_wires_the_first_provider_by_name_when_several_are_marked() {
        let app = |name: &str, port: u16, provides_url: bool| App {
            name: name.into(),
            base_port: port,
            path: format!("apps/{name}"),
            launch: vec![],
            url: None,
            url_env: Some("API_URL".into()),
            provides_url,
            static_env: HashMap::new(),
            prep_files: vec![],
            setup: vec![],
        };
        let mut cat = HashMap::new();
        cat.insert("web".to_string(), app("web", 9200, false));
        for (i, name) in [
            "api-h", "api-c", "api-a", "api-f", "api-b", "api-g", "api-d", "api-e",
        ]
        .into_iter()
        .enumerate()
        {
            cat.insert(name.to_string(), app(name, 9300 + i as u16, true));
        }
        let seq = TaskConfig {
            steps: vec![Step::Up("web".into())],
            ..TaskConfig::default()
        };
        let cfg = cfg_with(&[("seq", seq)]);
        let plan = execution_plan(&cfg, &cat, Path::new("/wt"), "seq", &BTreeMap::new()).unwrap();
        let PlannedStep::Up { launches, .. } = &plan.steps[0] else {
            panic!("an up step plans launches");
        };
        let apps: Vec<&str> = launches.iter().map(|l| l.app.as_str()).collect();
        assert_eq!(apps, ["api-a", "web"]);
        assert_eq!(launches[1].env["API_URL"].value, "http://localhost:9302");
    }

    #[test]
    fn require_live_unreferenced_app_errors() {
        let mut t = command_task(None, &["git", "version"], &[]);
        t.require_live = vec!["api-prod".into()];
        let cfg = cfg_with(&[("t", t)]);
        let err = resolve(
            &cfg,
            &api_catalog(),
            Path::new("/wt"),
            "/wt",
            "t",
            &BTreeMap::new(),
            &BTreeMap::new(),
            Caller::Agent,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("never references"));
    }
}
