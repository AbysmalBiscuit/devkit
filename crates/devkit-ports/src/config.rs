use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct Config {
    pub defaults: Defaults,
    pub apps: HashMap<String, AppConfig>,
    #[serde(default)]
    pub people: HashMap<String, Person>,
}

#[derive(Debug, Deserialize)]
pub struct Defaults {
    pub worktree_root: String,
    pub branch_prefix: String,
    pub baseline_ref: String,
    pub baseline_path: String,
    pub doppler_config: String,
    pub doppler_yaml: String,
    /// Repo-relative directory apps live under (e.g. "apps"). Used to infer app
    /// paths from doppler.yaml and to detect changed apps in a diff.
    #[serde(default = "default_apps_dir")]
    pub apps_dir: String,
    /// Base branch used when opening PRs (e.g. "staging", "main").
    #[serde(default = "default_pr_base")]
    pub pr_base: String,
}

fn default_apps_dir() -> String {
    "apps".to_string()
}

fn default_pr_base() -> String {
    "staging".to_string()
}

/// A team member's handle mapping (Slack user-id, GitHub login, etc.).
#[derive(Debug, Deserialize)]
pub struct Person {
    pub slack: String,
    #[serde(default)]
    pub github: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub base_port: u16,
    pub launch: Vec<String>,
    #[serde(default)]
    pub url_env: Option<String>,
    /// This app serves the URL that consumer apps wire to via their `url_env`.
    /// Exactly one app (the API) is normally marked; consumers reference it by role,
    /// not by a hardcoded name.
    #[serde(default)]
    pub provides_url: bool,
    #[serde(default)]
    pub preserve_env: Vec<String>,
    #[serde(default)]
    pub static_env: HashMap<String, String>,
    /// Env written to `<app>/.env.local` during `issue setup` (e.g. dummy workflow ids).
    #[serde(default)]
    pub prep_env: HashMap<String, String>,
    /// Optional overrides; normally derived from doppler.yaml.
    #[serde(default)]
    pub doppler_project: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

impl Config {
    pub fn parse(s: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(s).context("parsing devkit.toml")?;
        anyhow::ensure!(
            cfg.defaults.doppler_config != "prd",
            "refusing config with doppler_config = prd"
        );
        Ok(cfg)
    }
}

/// `--config` → `$DEVKIT_CONFIG` → `./devkit.toml` walking up → `~/.config/devkit/config.toml`.
pub fn locate(explicit: Option<&Path>, start: &Path) -> Option<PathBuf> {
    if let Some(p) = explicit { return Some(p.to_path_buf()); }
    if let Some(p) = std::env::var_os("DEVKIT_CONFIG") { return Some(PathBuf::from(p)); }
    let mut dir = Some(start);
    while let Some(d) = dir {
        let c = d.join("devkit.toml");
        if c.is_file() { return Some(c); }
        dir = d.parent();
    }
    let home = std::env::var_os("HOME")?;
    let fallback = PathBuf::from(home).join(".config/devkit/config.toml");
    fallback.is_file().then_some(fallback)
}

pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(h) = std::env::var_os("HOME") {
            return PathBuf::from(h).join(rest);
        }
    PathBuf::from(p)
}

#[cfg(test)]
pub fn tests_sample() -> &'static str { tests::SAMPLE }

#[cfg(test)]
mod tests {
    use super::*;
    pub(crate) const SAMPLE: &str = r#"
[defaults]
worktree_root = "~/Git/example"
branch_prefix = "lev/"
baseline_ref = "origin/staging"
baseline_path = "~/Git/example/_baseline"
doppler_config = "dev_local"
doppler_yaml = "~/Git/example/monorepo/doppler.yaml"
[apps.api]
base_port = 9100
launch = ["nitro", "dev", "--port", "{port}"]
url_env = "FOUNDRY_API_BASE_URL"
preserve_env = ["SUPABASE_JWT_SECRET"]
static_env = { SUPABASE_JWT_SECRET = "s" }
"#;
    #[test]
    fn parses_sample() {
        let c = Config::parse(SAMPLE).unwrap();
        assert_eq!(c.apps["api"].base_port, 9100);
        assert_eq!(c.apps["api"].url_env.as_deref(), Some("FOUNDRY_API_BASE_URL"));
    }
    #[test]
    fn rejects_prd() {
        let bad = SAMPLE.replace("dev_local", "prd");
        assert!(Config::parse(&bad).is_err());
    }
    #[test]
    fn parses_people_and_pr_base() {
        let src = r#"
[defaults]
worktree_root = "~/Git/example"
branch_prefix = "lev/"
baseline_ref = "origin/staging"
baseline_path = "~/Git/example/_baseline"
doppler_config = "dev_local"
doppler_yaml = "~/Git/example/monorepo/doppler.yaml"
pr_base = "staging"
[apps.api]
base_port = 9100
launch = ["nitro", "dev", "--port", "{port}"]
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
}
