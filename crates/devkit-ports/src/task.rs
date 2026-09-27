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
const ISSUE_FIELDS: [&str; 3] = ["issue", "slug", "branch"];

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
/// `[templates.variables]`, `issue`/`slug` from `.devkit/issue.toml` and
/// `branch` from the repository, then `--arg`. An issue field with no source
/// stays undefined rather than empty.
fn variables(
    cfg: &Config,
    worktree_root: &Path,
    args: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut vars = cfg.templates.defaults();
    if let Some(r) = record::read(worktree_root) {
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
/// their port references allocated (issue role, pid-less reservations for
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
            holder,
            name,
            t,
            user_env,
            false,
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

/// Discovery + allocation for one command task [`view`] accepted, then
/// delegate to the pure renderer. `ports[...]` references and (if `{{ port }}`
/// is used) the task's own app are allocated in one `registry::alloc` call.
#[allow(clippy::too_many_arguments)]
fn resolve_command(
    vars: &BTreeMap<String, String>,
    catalog: &HashMap<String, App>,
    worktree_root: &Path,
    holder: &str,
    name: &str,
    t: &TaskConfig,
    user_env: &BTreeMap<String, String>,
    enforce_live: bool,
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

    if enforce_live {
        let gated: Vec<&String> = t
            .require_live
            .iter()
            .filter(|r| refs.apps.contains(*r))
            .collect();
        if !gated.is_empty() {
            let data = registry::snapshot()?;
            for r in gated {
                ensure!(
                    registry::live_port(&data, holder, r).is_some(),
                    "require_live: `{r}` has no live server in this worktree (devrun up {r})"
                );
            }
        }
    }

    let ports: BTreeMap<String, u16> = if names.is_empty() {
        BTreeMap::new()
    } else {
        let reqs: Vec<(String, u16)> = names
            .iter()
            .map(|n| (n.clone(), catalog[n].base_port))
            .collect();
        registry::alloc(holder, &reqs, Role::Issue)?
            .into_iter()
            .collect()
    };
    let own_port = refs
        .own_port
        .then(|| ports[&app.expect("checked above").name]);

    resolve_command_with_ports(
        name,
        t,
        &env_templates,
        worktree_root,
        app.map(|a| a.path.as_str()),
        &ports,
        own_port,
        vars,
        user_env,
    )
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
        holder,
        name,
        v.task,
        user_env,
        true,
    )
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
    })
}

/// Run a command plan in the foreground with inherited stdio, its env overlaid
/// on the process environment. Returns the child's exit status.
pub fn exec(plan: &CommandPlan) -> Result<std::process::ExitStatus> {
    std::process::Command::new(&plan.argv[0])
        .args(&plan.argv[1..])
        .current_dir(&plan.cwd)
        .envs(&plan.env)
        .status()
        .with_context(|| format!("running task `{}` ({})", plan.name, plan.argv.join(" ")))
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
