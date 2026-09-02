use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use devkit_common::ui;
use devkit_config::{self as config, Config, Provenance};
use devkit_ports::apps::App;
use devkit_ports::load;
use devkit_ports::task::{self, TaskRow};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::Path;

/// Show the resolved config, or list configured apps or tasks.
#[derive(Parser)]
pub struct ConfigCli {
    #[command(subcommand)]
    cmd: Option<ConfigCmd>,
    /// Run as if this command had started in DIR instead of the current directory.
    #[arg(short = 'C', long = "dir", global = true)]
    dir: Option<String>,
    /// devkit.toml to load instead of the one discovered from the start directory.
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
    /// List the configured tasks from the merged config.
    Tasks {
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
        Some(ConfigCmd::Tasks { json }) => tasks(explicit, cwd, cli.json || json),
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
    let rows = task::list(&loaded.config);
    if json {
        println!("{}", serde_json::to_string_pretty(&tasks_json(&rows))?);
    } else {
        print!("{}", task::tasks_text(&rows));
    }
    Ok(())
}

/// Configured tasks as a JSON array of their listing fields.
fn tasks_json(rows: &[TaskRow]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "name": r.name,
                "kind": r.kind,
                "app": r.app,
                "description": r.description,
            })
        })
        .collect();
    serde_json::Value::Array(items)
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

/// Flattened `path = value  # from <file>` (or `# (default)`) lines, sorted by path.
/// Print `lines`, treating a reader that closed early (`devkit config | head`)
/// as done rather than a crash: `println!` panics on a broken pipe, and this
/// output is long enough that piping it into a pager or `head` is the norm.
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

/// `{ "config": <cfg>, "origins": { dotted-path: file } }` for `--origin --json`.
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
    use super::*;
    use devkit_config::{Config, Provenance};
    use devkit_ports::apps::App;
    use std::collections::HashMap;
    use std::path::PathBuf;

    // Build the sample inline: `config::tests_sample()` is `#[cfg(test)]` in
    // devkit-config, so it is NOT compiled into the crate when the devrun
    // binary builds its tests (a dependency builds without its own test cfg).
    fn sample_cfg() -> Config {
        Config::parse(
            "[defaults]\nworktree_root='/w'\nbranch_prefix='x/'\nbaseline_ref='r'\nbaseline_path='/b'\n[apps.api]\nbase_port=1\nlaunch=['a']\n",
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
        // a serde-defaulted value (pr_base) has no origin → marked (default)
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
        prov.shadowed.insert(
            "defaults.worktree_root".into(),
            vec![devkit_config::Shadow {
                file: PathBuf::from("/r/devkit.toml"),
                value: toml::Value::String("wts".into()),
            }],
        );
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
        // the layer list travels with the JSON so a consumer need not re-resolve it
        assert_eq!(v["layers"].as_array().unwrap().len(), 0);
        assert!(v.get("overrides").is_some());
    }

    fn sample_catalog() -> HashMap<String, App> {
        let mut m = HashMap::new();
        m.insert(
            "api".to_string(),
            App {
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
            },
        );
        m
    }

    fn sample_task_rows() -> Vec<TaskRow> {
        vec![
            TaskRow {
                name: "check".into(),
                kind: "sequence",
                app: "-".into(),
                description: "lint then test".into(),
            },
            TaskRow {
                name: "lint".into(),
                kind: "command",
                app: "api".into(),
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
        cat.insert(
            "lab-os".to_string(),
            App {
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
            },
        );
        let t = apps_table(&cat);
        let api_at = t.find("api").unwrap();
        let lab_at = t.find("lab-os").unwrap();
        assert!(api_at < lab_at); // sorted by name
    }
}
