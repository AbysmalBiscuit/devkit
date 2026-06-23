use crate::Cli;
use anyhow::{Context, Result};
use devkit_ports::config::{self, Config, Provenance};
use devkit_ports::load;
use std::collections::BTreeMap;
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

/// `devrun config apps [--json]` — catalog readout. (placeholder; real listing wired separately)
pub fn apps(_cli: &Cli, _cwd: &str, _json: bool) -> Result<()> {
    anyhow::bail!("config apps not yet implemented")
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
    use devkit_ports::config::{Config, Provenance};
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
}
