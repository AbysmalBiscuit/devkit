//! Canned oneshot tasks (`[tasks]`): resolve a named task against the config,
//! app catalog, and port registry into runnable plans, and execute command
//! plans in the foreground. Shared seam for the `devrun task` CLI (and any
//! future MCP surface).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail, ensure};
use devkit_common::template;

use crate::apps::App;
use crate::config::{Config, Step, TaskConfig};
use crate::registry::{self, Role};
use crate::run;

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

/// One row for the `devrun task` listing.
pub struct TaskRow {
    pub name: String,
    pub kind: &'static str,
    pub app: String,
    pub description: String,
}

/// Configured tasks sorted by name. `kind` reflects the shape on disk; an
/// invalid shape (both or neither of `run`/`steps`) is listed as `invalid`
/// rather than hidden, so a typo is visible in the listing.
pub fn list(cfg: &Config) -> Vec<TaskRow> {
    let mut rows: Vec<TaskRow> = cfg
        .tasks
        .iter()
        .map(|(name, t)| TaskRow {
            name: name.clone(),
            kind: match (!t.run.is_empty(), !t.steps.is_empty()) {
                (true, false) => "command",
                (false, true) => "sequence",
                _ => "invalid",
            },
            app: t.app.clone().unwrap_or_else(|| "-".into()),
            description: t.description.clone().unwrap_or_default(),
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

/// Resolve task `name` for execution in `worktree_root`. Command tasks get
/// their port references allocated (issue role, pid-less reservations for
/// apps not yet running) and their templates rendered; sequence tasks
/// validate and resolve each step. All validation errors fire here, before
/// anything spawns.
pub fn resolve(
    cfg: &Config,
    catalog: &HashMap<String, App>,
    worktree_root: &Path,
    holder: &str,
    name: &str,
    user_env: &BTreeMap<String, String>,
) -> Result<Resolved> {
    let t = cfg
        .tasks
        .get(name)
        .ok_or_else(|| anyhow!("unknown task `{name}` (run `devrun task` to list)"))?;
    match (!t.run.is_empty(), !t.steps.is_empty()) {
        (true, true) => bail!("task `{name}` sets both `run` and `steps`"),
        (false, false) => bail!("task `{name}` sets neither `run` nor `steps`"),
        (true, false) => Ok(Resolved::Command(resolve_command(
            cfg,
            catalog,
            worktree_root,
            holder,
            name,
            t,
            user_env,
            false,
        )?)),
        (false, true) => {
            ensure!(
                t.app.is_none() && t.env.is_empty() && t.require_live.is_empty(),
                "sequence task `{name}` may only set `description` and `steps`"
            );
            let mut items = Vec::with_capacity(t.steps.len());
            for step in &t.steps {
                match step {
                    Step::Task(r) => {
                        let sub = cfg.tasks.get(r).ok_or_else(|| {
                            anyhow!("task `{name}` references unknown task `{r}`")
                        })?;
                        ensure!(
                            !sub.run.is_empty() && sub.steps.is_empty(),
                            "task `{name}` references `{r}`, which is not a command task \
                             (sequences cannot nest)"
                        );
                        items.push(SeqItem::Run(resolve_command(
                            cfg,
                            catalog,
                            worktree_root,
                            holder,
                            r,
                            sub,
                            user_env,
                            false,
                        )?));
                    }
                    Step::Up(app) => {
                        ensure!(
                            catalog.contains_key(app),
                            "task `{name}` brings up unknown app `{app}`"
                        );
                        items.push(SeqItem::Up(app.clone()));
                    }
                }
            }
            Ok(Resolved::Sequence(items))
        }
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

/// Discovery + allocation for one command task, then delegate to the pure
/// renderer. `ports[...]` references and (if `{{ port }}` is used) the task's
/// own app are allocated in one `registry::alloc` call.
#[allow(clippy::too_many_arguments)]
fn resolve_command(
    cfg: &Config,
    catalog: &HashMap<String, App>,
    worktree_root: &Path,
    holder: &str,
    name: &str,
    t: &TaskConfig,
    user_env: &BTreeMap<String, String>,
    enforce_live: bool,
) -> Result<CommandPlan> {
    let app = t
        .app
        .as_deref()
        .map(|a| {
            catalog
                .get(a)
                .ok_or_else(|| anyhow!("task `{name}` names unknown app `{a}`"))
        })
        .transpose()?;
    let static_env = app.map(|a| a.static_env.clone()).unwrap_or_default();
    let env_templates = effective_env(&static_env, t, user_env);
    let vars = &cfg.templates.variables;

    let mut all_templates: Vec<&str> = t.run.iter().map(String::as_str).collect();
    all_templates.extend(static_env.values().map(String::as_str));
    all_templates.extend(t.env.values().map(String::as_str));
    let all_refs = template::referenced_ports(&all_templates, vars)
        .with_context(|| format!("scanning templates of task `{name}`"))?;
    for r in &t.require_live {
        ensure!(
            catalog.contains_key(r),
            "task `{name}` lists unknown app `{r}` in require_live"
        );
        ensure!(
            all_refs.apps.contains(r),
            "task `{name}` lists `{r}` in require_live but never references `ports['{r}']`"
        );
    }

    let mut templates: Vec<&str> = t.run.iter().map(String::as_str).collect();
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
/// upfront render used; standalone commands call it right before exec.
pub fn resolve_step(
    cfg: &Config,
    catalog: &HashMap<String, App>,
    worktree_root: &Path,
    holder: &str,
    name: &str,
    user_env: &BTreeMap<String, String>,
) -> Result<CommandPlan> {
    let t = cfg
        .tasks
        .get(name)
        .ok_or_else(|| anyhow!("unknown task `{name}` (run `devrun task` to list)"))?;
    ensure!(
        !t.run.is_empty() && t.steps.is_empty(),
        "task `{name}` is not a command task"
    );
    resolve_command(cfg, catalog, worktree_root, holder, name, t, user_env, true)
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
    let argv = t
        .run
        .iter()
        .map(|s| template::render_launch(s, own_port, ports, variables))
        .collect::<Result<Vec<_>>>()
        .with_context(|| format!("rendering `run` of task `{name}`"))?;
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
    use super::*;
    use crate::config::{Config, Step, TaskConfig};

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
            run: run.iter().map(|s| s.to_string()).collect(),
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..TaskConfig::default()
        }
    }

    fn api_catalog() -> HashMap<String, App> {
        let mut m = HashMap::new();
        m.insert(
            "api-prod".to_string(),
            App {
                name: "api-prod".into(),
                base_port: 9101,
                path: "apps/api".into(),
                launch: vec![],
                url_env: None,
                provides_url: false,
                static_env: [("FROM_APP".to_string(), "static".to_string())].into(),
                prep_files: vec![],
                setup: vec![],
            },
        );
        m
    }

    #[test]
    fn command_env_layering_static_then_task_then_user() {
        let t = command_task(
            Some("api-prod"),
            &["git", "version"],
            &[("FROM_APP", "task")],
        );
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
    fn resolve_rejects_bad_shapes() {
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
        let cfg = cfg_with(&[
            ("both", both),
            ("neither", neither),
            ("seq-with-app", seq_with_app),
            ("nested", step_to_sequence),
            ("seq", seq),
            ("seq-require-live", seq_with_require_live),
        ]);
        let cat = api_catalog();
        let u = BTreeMap::new();
        for bad in [
            "both",
            "neither",
            "seq-with-app",
            "nested",
            "seq-require-live",
            "missing",
        ] {
            assert!(
                resolve(&cfg, &cat, Path::new("/wt"), "/wt", bad, &u).is_err(),
                "task `{bad}` must fail validation"
            );
        }
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
        let t = command_task(
            None,
            &["git"],
            &[("BASE", "http://localhost:{{ ports['api-prod'] }}")],
        );
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
            (
                "a-seq",
                TaskConfig {
                    description: Some("d".into()),
                    steps: vec![Step::Up("api-prod".into())],
                    ..TaskConfig::default()
                },
            ),
        ]);
        let rows = list(&cfg);
        assert_eq!(rows[0].name, "a-seq");
        assert_eq!(rows[0].kind, "sequence");
        assert_eq!(rows[0].description, "d");
        assert_eq!(rows[1].kind, "command");
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
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("not a command task"));
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
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("never references"));
    }
}
