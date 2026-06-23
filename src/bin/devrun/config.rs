use crate::Cli;
use anyhow::{Context, Result};
use devkit_common::ui;
use devkit_ports::apps::App;
use devkit_ports::config::{self, Config, Provenance};
use devkit_ports::load;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// `devrun config show [--origin] [--json]`
pub fn show(cli: &Cli, cwd: &str, origin: bool, json: bool) -> Result<()> {
    let loaded = load::load(cli.config.as_deref().map(Path::new), Path::new(cwd))?;
    let cfg = &loaded.config;
    let prov = &loaded.provenance;
    match (origin, json) {
        (true, false) => {
            for line in origin_lines(cfg, prov)? {
                println!("{line}");
            }
        }
        (true, true) => {
            println!("{}", serde_json::to_string_pretty(&origin_json(cfg, prov)?)?);
        }
        (false, true) => println!("{}", serde_json::to_string_pretty(cfg)?),
        (false, false) => println!("{}", toml::to_string_pretty(cfg)?),
    }
    Ok(())
}

/// `devrun config apps [--json]` — a pure readout of the merged app catalog.
pub fn apps(cli: &Cli, cwd: &str, json: bool) -> Result<()> {
    let loaded = load::load(cli.config.as_deref().map(Path::new), Path::new(cwd))?;
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
    let mut t = ui::table(&["NAME", "PORT", "PATH", "PROVIDES_URL", "URL_ENV", "LAUNCH"]);
    for n in names {
        let a = &catalog[n];
        t.add_row(vec![
            a.name.clone(),
            a.base_port.to_string(),
            a.path.clone(),
            a.provides_url.to_string(),
            a.url_env.clone().unwrap_or_else(|| "-".into()),
            a.launch.join(" "),
        ]);
    }
    t.to_string()
}

/// Flattened `path = value  # from <file>` (or `# (default)`) lines, sorted by path.
fn origin_lines(cfg: &Config, prov: &Provenance) -> Result<Vec<String>> {
    let val = toml::Value::try_from(cfg).context("serializing config to toml")?;
    let mut leaves = Vec::new();
    config::flatten(&val, "", &mut leaves);
    leaves.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(leaves
        .iter()
        .map(|(path, value)| match prov.origin.get(path) {
            Some(f) => format!("{path} = {value}  # from {}", f.display()),
            None => format!("{path} = {value}  # (default)"),
        })
        .collect())
}

/// `{ "config": <cfg>, "origins": { dotted-path: file } }` for `--origin --json`.
fn origin_json(cfg: &Config, prov: &Provenance) -> Result<serde_json::Value> {
    let origins: BTreeMap<String, String> = prov
        .origin
        .iter()
        .map(|(k, v)| (k.clone(), v.display().to_string()))
        .collect();
    Ok(serde_json::json!({ "config": cfg, "origins": origins }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use devkit_ports::apps::App;
    use devkit_ports::config::{Config, Provenance};
    use std::collections::HashMap;
    use std::path::PathBuf;

    // Build the sample inline: `config::tests_sample()` is `#[cfg(test)]` in
    // devkit-ports, so it is NOT compiled into the crate when the devrun binary
    // builds its tests (a dependency builds without its own test cfg).
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
        assert!(lines.iter().any(|l| l.starts_with("defaults.worktree_root =")
            && l.contains("# from /home/u/.config/devkit/config.toml")));
        // a serde-defaulted value (pr_base) has no origin → marked (default)
        assert!(lines
            .iter()
            .any(|l| l.starts_with("defaults.pr_base =") && l.contains("# (default)")));
        // output is sorted by path
        let mut sorted = lines.clone();
        sorted.sort();
        assert_eq!(lines, sorted);
    }

    #[test]
    fn origin_json_has_config_and_origins() {
        let cfg = sample_cfg();
        let mut prov = Provenance::default();
        prov.origin
            .insert("defaults.worktree_root".into(), PathBuf::from("/x/devkit.toml"));
        let v = origin_json(&cfg, &prov).unwrap();
        assert!(v.get("config").is_some());
        assert_eq!(
            v["origins"]["defaults.worktree_root"].as_str(),
            Some("/x/devkit.toml")
        );
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
                url_env: Some("FOUNDRY_API_BASE_URL".into()),
                provides_url: true,
                static_env: HashMap::new(),
                prep_env: HashMap::new(),
                setup: Vec::new(),
            },
        );
        m
    }

    #[test]
    fn apps_json_lists_resolved_fields() {
        let v = apps_json(&sample_catalog());
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"].as_str(), Some("api"));
        assert_eq!(arr[0]["base_port"].as_u64(), Some(9100));
        assert_eq!(arr[0]["path"].as_str(), Some("apps/api"));
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
                url_env: None,
                provides_url: false,
                static_env: HashMap::new(),
                prep_env: HashMap::new(),
                setup: Vec::new(),
            },
        );
        let t = apps_table(&cat);
        let api_at = t.find("api").unwrap();
        let lab_at = t.find("lab-os").unwrap();
        assert!(api_at < lab_at); // sorted by name
    }
}
