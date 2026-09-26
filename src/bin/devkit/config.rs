use std::{
    collections::{BTreeMap, HashMap},
    io::Write,
    path::Path,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use devkit_common::{caller::Caller, required, ui};
use devkit_config::{
    self as config, Config, Provenance, Required, RunAction, RunArg, Step, TaskConfig,
};
use devkit_ports::{
    apps::App,
    load,
    task::{self, TaskArg, TaskRow},
};

/// Show the resolved config, or list configured apps, tasks or template
/// variables.
#[derive(Parser)]
pub struct ConfigCli {
    #[command(subcommand)]
    cmd: Option<ConfigCmd>,
    /// Run as if this command had started in DIR instead of the current
    /// directory.
    #[arg(short = 'C', long = "dir", global = true)]
    dir: Option<String>,
    /// devkit.toml to load instead of the one discovered from the start
    /// directory.
    #[arg(long, global = true)]
    config: Option<String>,
    /// Annotate each value with the file it was resolved from.
    #[arg(long)]
    origin: bool,
    /// Emit JSON instead of TOML.
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Print the effective merged config (TOML by default).
    Show {
        /// Annotate each value with the file it was resolved from.
        #[arg(long)]
        origin: bool,
        /// Emit JSON instead of TOML.
        #[arg(long)]
        json: bool,
    },
    /// List the configured apps from the merged config.
    Apps {
        /// Emit JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// List the configured tasks, or describe one and the args it takes.
    ///
    /// A described task shows what it runs, the servers it needs live, the
    /// command that runs it with every `--arg` this caller must pass, and,
    /// for each arg, which callers must pass it, its default, and its
    /// description.
    Tasks {
        /// Task to describe; omit to list them all.
        name: Option<String>,
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// List the template variables, with defaults and descriptions.
    ///
    /// These are the `[templates.variables]` entries every template renders
    /// over, and the names `--arg` may set.
    Variables {
        /// Emit JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
}

/// Bare `devkit config` is `devkit config show`: the resolved config is what
/// people reach for, and making them name the subcommand buys nothing.
///
/// A flag may be spelled before or after the subcommand, so each one is the
/// union of the two positions.
pub fn run(cli: ConfigCli) -> Result<()> {
    let explicit = cli.config.as_deref().map(Path::new);
    let cwd = cli.dir.as_deref().unwrap_or(".");
    match cli.cmd {
        None => show(explicit, cwd, cli.origin, cli.json),
        Some(ConfigCmd::Show { origin, json }) => {
            show(explicit, cwd, cli.origin || origin, cli.json || json)
        }
        Some(ConfigCmd::Apps { json }) => apps(explicit, cwd, cli.json || json),
        Some(ConfigCmd::Tasks { name: None, json }) => tasks(explicit, cwd, cli.json || json),
        Some(ConfigCmd::Tasks {
            name: Some(name),
            json,
        }) => describe_task(explicit, cwd, &name, cli.json || json),
        Some(ConfigCmd::Variables { json }) => variables(explicit, cwd, cli.json || json),
    }
}

/// `devkit config show [--origin] [--json]`
fn show(explicit: Option<&Path>, cwd: &str, origin: bool, json: bool) -> Result<()> {
    let loaded = load::load(explicit, Path::new(cwd))?;
    let cfg = &loaded.config;
    let prov = &loaded.provenance;
    let lines: Vec<String> = match (origin, json) {
        (true, false) => layer_header(prov)
            .into_iter()
            .chain(origin_lines(cfg, prov)?)
            .collect(),
        (true, true) => vec![serde_json::to_string_pretty(&origin_json(cfg, prov)?)?],
        // Plain `--json` stays a bare config object: programmatic callers
        // deserialize it directly, and a wrapper would break them.
        (false, true) => vec![serde_json::to_string_pretty(cfg)?],
        (false, false) => layer_header(prov)
            .into_iter()
            .chain(std::iter::once(toml::to_string_pretty(cfg)?))
            .collect(),
    };
    print_lines(lines)
}

/// `devkit config apps [--json]` — a pure readout of the merged app catalog.
fn apps(explicit: Option<&Path>, cwd: &str, json: bool) -> Result<()> {
    let loaded = load::load(explicit, Path::new(cwd))?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&apps_json(&loaded.catalog))?
        );
    } else {
        println!("{}", apps_table(&loaded.catalog));
    }
    Ok(())
}

/// `devkit config tasks [--json]` — a pure readout of the merged `[tasks]`.
fn tasks(explicit: Option<&Path>, cwd: &str, json: bool) -> Result<()> {
    let loaded = load::load(explicit, Path::new(cwd))?;
    let rows = task::list(&loaded.config, devkit_common::caller::caller());
    if json {
        println!("{}", serde_json::to_string_pretty(&tasks_json(&rows))?);
    } else {
        print!("{}", task::tasks_text(&rows));
    }
    Ok(())
}

/// Configured tasks as a JSON array of their listing fields. `required` is
/// whether this caller must pass the arg.
fn tasks_json(rows: &[TaskRow]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let args: Vec<serde_json::Value> = r
                .args
                .iter()
                .map(|a| {
                    serde_json::json!({
                        "name": a.name,
                        "required": a.required,
                        "default": a.default,
                        "description": a.description,
                    })
                })
                .collect();
            serde_json::json!({
                "name": r.name,
                "kind": r.kind,
                "app": r.app,
                "args": args,
                "description": r.description,
            })
        })
        .collect();
    serde_json::Value::Array(items)
}

/// Args with `required` as the marking naming who must pass each.
fn args_json(args: &[TaskArg]) -> serde_json::Value {
    args.iter()
        .map(|a| {
            serde_json::json!({
                "name": a.name,
                "required": a.required_of,
                "default": a.default,
                "description": a.description,
            })
        })
        .collect()
}

/// `devkit config tasks <name> [--json]`: one task's templates, gates and
/// args, so a caller learns what to pass and start without a failing
/// `--dry-run`.
fn describe_task(explicit: Option<&Path>, cwd: &str, name: &str, json: bool) -> Result<()> {
    let loaded = load::load(explicit, Path::new(cwd))?;
    let cfg = &loaded.config;
    let row = task::describe(cfg, name, devkit_common::caller::caller())?;
    let t = &cfg.tasks[name];
    if json {
        let steps: Vec<serde_json::Value> = t
            .steps
            .iter()
            .map(|s| match (s, step_command(cfg, s)) {
                (Step::Task(r), Some(sub)) => serde_json::json!({
                    "task": r,
                    "run": sub.run,
                    "require_live": live_json(&sub.require_live),
                }),
                _ => serde_json::json!(s),
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "name": row.name,
                "kind": row.kind,
                "app": t.app,
                "description": row.description,
                "run": t.run,
                "steps": steps,
                "env": t.env,
                "require_live": live_json(&t.require_live),
                "usage": usage(&row),
                "args": args_json(&row.args),
            }))?
        );
    } else {
        print!("{}", task_text(cfg, &row, t));
    }
    Ok(())
}

/// The command task a `task` step runs, if it names one.
fn step_command<'a>(cfg: &'a Config, s: &Step) -> Option<&'a TaskConfig> {
    match s {
        Step::Task(r) => cfg.tasks.get(r).filter(|sub| !sub.run.is_empty()),
        Step::Up(_) => None,
    }
}

fn live_json(apps: &[String]) -> serde_json::Value {
    apps.iter()
        .map(|a| serde_json::json!({ "app": a, "start": format!("devrun up {a}") }))
        .collect()
}

/// The command that runs the task, with each `--arg` this caller must pass.
fn usage(row: &TaskRow) -> String {
    std::iter::once(format!("devrun task {}", row.name))
        .chain(
            row.args
                .iter()
                .filter(|a| a.required)
                .map(|a| format!("--arg {}=...", a.name)),
        )
        .collect::<Vec<_>>()
        .join(" ")
}

/// A task's fields as label/value lines, then its args as a table.
fn task_text(cfg: &Config, row: &TaskRow, t: &TaskConfig) -> String {
    let mut head = vec![("name", row.name.clone())];
    if !row.description.is_empty() {
        head.push(("description", row.description.clone()));
    }
    head.push(("kind", row.kind.to_string()));
    if t.app.is_some() {
        head.push(("app", row.app.clone()));
    }
    if !t.run.is_empty() {
        head.push(("run", run_text(&t.run)));
    }
    push_list(
        &mut head,
        "steps",
        t.steps.iter().map(|s| match (s, step_command(cfg, s)) {
            (Step::Task(r), Some(sub)) => format!(
                "task {r}: {}{}",
                run_text(&sub.run),
                if sub.require_live.is_empty() {
                    String::new()
                } else {
                    format!(" (needs live {})", sub.require_live.join(", "))
                }
            ),
            (Step::Task(r), None) => format!("task {r}"),
            (Step::Up(app), _) => format!("up {app}"),
        }),
    );
    push_list(
        &mut head,
        "env",
        t.env.iter().map(|(k, v)| format!("{k}={v}")),
    );
    push_list(
        &mut head,
        "needs live",
        t.require_live
            .iter()
            .map(|a| format!("{a} (devrun up {a})")),
    );
    head.push(("usage", usage(row)));
    let args = if row.args.is_empty() {
        "takes no args\n".to_string()
    } else {
        args_table(&row.args)
    };
    format!("{}\n\n{args}", ui::kv_table(&head))
}

/// One row per value, the label on the first only.
fn push_list(
    rows: &mut Vec<(&'static str, String)>,
    label: &'static str,
    values: impl Iterator<Item = String>,
) {
    for (i, v) in values.enumerate() {
        rows.push((if i == 0 { label } else { "" }, v));
    }
}

/// A `run` array as one line, a split entry spelled out as what it produces.
fn run_text(run: &[RunArg]) -> String {
    run.iter()
        .map(|a| match a {
            RunArg::Scalar(s) => s.clone(),
            RunArg::Action(RunAction::Split { split, on }) => {
                format!("{split}(split on {on:?} into separate args)")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `devkit config variables [--json]`: the declared template variables,
/// required or not for this caller outside any one task.
fn variables(explicit: Option<&Path>, cwd: &str, json: bool) -> Result<()> {
    let loaded = load::load(explicit, Path::new(cwd))?;
    let vars = declared_variables(&loaded.config, devkit_common::caller::caller());
    if json {
        println!("{}", serde_json::to_string_pretty(&args_json(&vars))?);
    } else if vars.is_empty() {
        println!("no variables declared (add [templates.variables] to devkit.toml)");
    } else {
        print!("{}", args_table(&vars));
    }
    Ok(())
}

fn declared_variables(cfg: &Config, caller: Caller) -> Vec<TaskArg> {
    cfg.templates
        .variables
        .iter()
        .map(|(name, d)| TaskArg {
            name: name.clone(),
            required: required::is_required(cfg, None, name, caller),
            required_of: required::required_of(cfg, None, name),
            default: d.default_value().map(str::to_string),
            description: d.description().map(str::to_string),
        })
        .collect()
}

/// Args as a table, REQUIRED naming who must pass each. An empty default is
/// quoted so it reads apart from none.
fn args_table(args: &[TaskArg]) -> String {
    let mut t = ui::table(&["NAME", "REQUIRED", "DEFAULT", "DESCRIPTION"]);
    for a in args {
        t.add_row(vec![
            a.name.clone(),
            match a.required_of {
                Required::Always => "always",
                Required::Agents => "agents",
                Required::Humans => "humans",
                Required::Never => "no",
            }
            .to_string(),
            match a.default.as_deref() {
                None => "none".to_string(),
                Some("") => "\"\"".to_string(),
                Some(d) => d.to_string(),
            },
            a.description.clone().unwrap_or_default(),
        ]);
    }
    format!("{t}\n")
}

/// Catalog apps sorted by name, as a JSON array of their resolved fields.
fn apps_json(catalog: &HashMap<String, App>) -> serde_json::Value {
    let mut names: Vec<&String> = catalog.keys().collect();
    names.sort();
    let rows: Vec<serde_json::Value> = names
        .iter()
        .map(|n| {
            let a = &catalog[*n];
            serde_json::json!({
                "name": a.name,
                "base_port": a.base_port,
                "path": a.path,
                "url": a.url_template(),
                "provides_url": a.provides_url,
                "url_env": a.url_env,
                "launch": a.launch,
            })
        })
        .collect();
    serde_json::Value::Array(rows)
}

/// Catalog apps sorted by name, rendered as a text table.
fn apps_table(catalog: &HashMap<String, App>) -> String {
    let mut names: Vec<&String> = catalog.keys().collect();
    names.sort();
    let mut t = ui::table(&[
        "NAME",
        "PORT",
        "PATH",
        "URL",
        "PROVIDES_URL",
        "URL_ENV",
        "LAUNCH",
    ]);
    for n in names {
        let a = &catalog[n];
        t.add_row(vec![
            a.name.clone(),
            a.base_port.to_string(),
            a.path.clone(),
            a.url_template().to_string(),
            a.provides_url.to_string(),
            a.url_env.clone().unwrap_or_else(|| "-".into()),
            a.launch.join(" "),
        ]);
    }
    t.to_string()
}

/// Flattened `path = value  # from <file>` (or `# (default)`) lines, sorted by
/// path. Print `lines`, treating a reader that closed early (`devkit config |
/// head`) as done rather than a crash: `println!` panics on a broken pipe, and
/// this output is long enough that piping it into a pager or `head` is the
/// norm.
fn print_lines(lines: impl IntoIterator<Item = String>) -> Result<()> {
    let mut out = std::io::stdout().lock();
    for line in lines {
        match writeln!(out, "{line}") {
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
            other => other?,
        }
    }
    Ok(())
}

/// The layer files as TOML comments, lowest precedence first, so the output
/// stays a valid config while saying what produced it. Empty when nothing was
/// resolved.
fn layer_header(prov: &Provenance) -> Vec<String> {
    if prov.layers.is_empty() {
        return Vec::new();
    }
    let mut out = vec!["# layers, lowest to highest precedence:".to_string()];
    out.extend(prov.layers.iter().map(|p| format!("#   {}", p.display())));
    out.push(String::new());
    out
}

fn origin_lines(cfg: &Config, prov: &Provenance) -> Result<Vec<String>> {
    let val = toml::Value::try_from(cfg).context("serializing config to toml")?;
    let mut leaves = Vec::new();
    config::flatten(&val, "", &mut leaves);
    leaves.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(leaves
        .iter()
        .map(|(path, value)| match prov.origin.get(path) {
            Some(f) => format!(
                "{path} = {value}  # from {}{}",
                f.display(),
                overrides_clause(prov, path)
            ),
            None => format!("{path} = {value}  # (default)"),
        })
        .collect())
}

/// What a value displaced, as ` (overrides <file>: <value>, ...)`, in the same
/// lowest-to-highest order the layer header lists. Empty for a leaf only one
/// layer sets, which is most of them.
fn overrides_clause(prov: &Provenance, path: &str) -> String {
    match prov.shadowed.get(path) {
        None => String::new(),
        Some(shadows) => {
            let parts: Vec<String> = shadows
                .iter()
                .map(|s| format!("{}: {}", s.file.display(), s.value))
                .collect();
            format!(" (overrides {})", parts.join(", "))
        }
    }
}

/// `{ "config": <cfg>, "origins": { dotted-path: file } }` for `--origin
/// --json`.
fn origin_json(cfg: &Config, prov: &Provenance) -> Result<serde_json::Value> {
    let origins: BTreeMap<String, String> = prov
        .origin
        .iter()
        .map(|(k, v)| (k.clone(), v.display().to_string()))
        .collect();
    let layers: Vec<String> = prov
        .layers
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    let overrides: BTreeMap<String, Vec<serde_json::Value>> = prov
        .shadowed
        .iter()
        .map(|(k, shadows)| {
            let v = shadows
                .iter()
                .map(|s| serde_json::json!({ "file": s.file.display().to_string(), "value": s.value.to_string() }))
                .collect();
            (k.clone(), v)
        })
        .collect();
    Ok(serde_json::json!({
        "config": cfg,
        "layers": layers,
        "origins": origins,
        "overrides": overrides,
    }))
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use devkit_config::{Config, Provenance};
    use devkit_ports::apps::App;

    use super::*;

    // Build the sample inline: `config::tests_sample()` is `#[cfg(test)]` in
    // devkit-config, so it is NOT compiled into the crate when the devrun
    // binary builds its tests (a dependency builds without its own test cfg).
    fn sample_cfg() -> Config {
        Config::parse(
            "[defaults]\nworktree_root='/w'\nbranch_prefix='x/'\nbaseline_ref='r'\n[apps.api]\nbase_port=1\nlaunch=['a']\n",
        )
        .unwrap()
    }

    #[test]
    fn origin_lines_annotate_source_and_default() {
        let cfg = sample_cfg();
        let mut prov = Provenance::default();
        prov.origin.insert(
            "defaults.worktree_root".into(),
            PathBuf::from("/home/u/.config/devkit/config.toml"),
        );
        let lines = origin_lines(&cfg, &prov).unwrap();
        // a value present in the origin map is attributed to its file
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("defaults.worktree_root =")
                    && l.contains("# from /home/u/.config/devkit/config.toml"))
        );
        // a serde-defaulted value (pr_base) has no origin -> marked (default)
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("defaults.pr_base =") && l.contains("# (default)"))
        );
        // output is sorted by path
        let mut sorted = lines.clone();
        sorted.sort();
        assert_eq!(lines, sorted);
    }

    #[test]
    fn layer_header_names_every_file_in_precedence_order() {
        let prov = Provenance {
            layers: vec![
                PathBuf::from("/etc/devkit.toml"),
                PathBuf::from("/r/devkit.toml"),
            ],
            ..Default::default()
        };
        let h = layer_header(&prov);
        assert_eq!(h[0], "# layers, lowest to highest precedence:");
        assert_eq!(h[1], "#   /etc/devkit.toml");
        assert_eq!(h[2], "#   /r/devkit.toml");
        // a blank line separates the header from the config it describes
        assert_eq!(h[3], "");
        // every line is a comment, so the output is still valid TOML
        assert!(h[..3].iter().all(|l| l.starts_with('#')));
    }

    #[test]
    fn no_layers_means_no_header() {
        assert!(layer_header(&Provenance::default()).is_empty());
    }

    /// A leaf several layers set says which it displaced and what each held;
    /// a leaf only one layer sets keeps the bare `# from <file>`.
    #[test]
    fn origin_lines_report_what_a_value_overrides() {
        let cfg = sample_cfg();
        let mut prov = Provenance::default();
        prov.origin.insert(
            "defaults.worktree_root".into(),
            PathBuf::from("/r/local.toml"),
        );
        prov.shadowed.insert("defaults.worktree_root".into(), vec![
            devkit_config::Shadow {
                file: PathBuf::from("/r/devkit.toml"),
                value: toml::Value::String("wts".into()),
            },
        ]);
        let lines = origin_lines(&cfg, &prov).unwrap();
        let line = lines
            .iter()
            .find(|l| l.starts_with("defaults.worktree_root ="))
            .unwrap();
        assert!(line.contains("# from /r/local.toml"), "{line}");
        assert!(
            line.contains(r#"(overrides /r/devkit.toml: "wts")"#),
            "{line}"
        );
        // a leaf with no shadow carries no clause
        assert!(
            lines
                .iter()
                .filter(|l| !l.starts_with("defaults.worktree_root ="))
                .all(|l| !l.contains("overrides")),
        );
    }

    #[test]
    fn origin_json_has_config_and_origins() {
        let cfg = sample_cfg();
        let mut prov = Provenance::default();
        prov.origin.insert(
            "defaults.worktree_root".into(),
            PathBuf::from("/x/devkit.toml"),
        );
        let v = origin_json(&cfg, &prov).unwrap();
        assert!(v.get("config").is_some());
        assert_eq!(
            v["origins"]["defaults.worktree_root"].as_str(),
            Some("/x/devkit.toml")
        );
        // the layer list travels with the JSON so a consumer need not
        // re-resolve it
        assert_eq!(v["layers"].as_array().unwrap().len(), 0);
        assert!(v.get("overrides").is_some());
    }

    fn sample_catalog() -> HashMap<String, App> {
        let mut m = HashMap::new();
        m.insert("api".to_string(), App {
            name: "api".into(),
            base_port: 9100,
            path: "apps/api".into(),
            launch: vec!["nitro".into(), "dev".into()],
            url: Some("https://localhost:{{ port }}/x".into()),
            url_env: Some("FOUNDRY_API_BASE_URL".into()),
            provides_url: true,
            static_env: HashMap::new(),
            prep_files: vec![],
            setup: Vec::new(),
        });
        m
    }

    fn sample_task_rows() -> Vec<TaskRow> {
        vec![
            TaskRow {
                name: "check".into(),
                kind: "sequence",
                app: "-".into(),
                args: vec![],
                description: "lint then test".into(),
            },
            TaskRow {
                name: "lint".into(),
                kind: "command",
                app: "api".into(),
                args: vec![devkit_ports::task::TaskArg {
                    name: "path".into(),
                    required: true,
                    required_of: Required::Always,
                    default: None,
                    description: Some("file or directory to lint".into()),
                }],
                description: String::new(),
            },
        ]
    }

    #[test]
    fn tasks_json_lists_fields() {
        let v = tasks_json(&sample_task_rows());
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["name"].as_str(), Some("check"));
        assert_eq!(arr[0]["kind"].as_str(), Some("sequence"));
        assert_eq!(arr[1]["app"].as_str(), Some("api"));
        assert_eq!(
            arr[1]["args"],
            serde_json::json!([{
                "name": "path",
                "required": true,
                "default": null,
                "description": "file or directory to lint",
            }])
        );
        assert_eq!(arr[1]["description"].as_str(), Some(""));
    }

    #[test]
    fn apps_json_lists_resolved_fields() {
        let v = apps_json(&sample_catalog());
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"].as_str(), Some("api"));
        assert_eq!(arr[0]["base_port"].as_u64(), Some(9100));
        assert_eq!(arr[0]["path"].as_str(), Some("apps/api"));
        assert_eq!(
            arr[0]["url"].as_str(),
            Some("https://localhost:{{ port }}/x")
        );
        assert_eq!(arr[0]["provides_url"].as_bool(), Some(true));
        assert_eq!(arr[0]["url_env"].as_str(), Some("FOUNDRY_API_BASE_URL"));
    }

    #[test]
    fn apps_table_renders_sorted_names() {
        let mut cat = sample_catalog();
        cat.insert("lab-os".to_string(), App {
            name: "lab-os".into(),
            base_port: 9200,
            path: "apps/lab-os".into(),
            launch: vec!["next".into()],
            url: None,
            url_env: None,
            provides_url: false,
            static_env: HashMap::new(),
            prep_files: vec![],
            setup: Vec::new(),
        });
        let t = apps_table(&cat);
        let api_at = t.find("api").unwrap();
        let lab_at = t.find("lab-os").unwrap();
        assert!(api_at < lab_at); // sorted by name
    }
}
