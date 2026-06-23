use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub defaults: Defaults,
    pub apps: HashMap<String, AppConfig>,
    #[serde(default)]
    pub people: HashMap<String, Person>,
    #[serde(default)]
    pub daemon: DaemonConfig,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct DaemonConfig {
    /// Run gate: autostart the daemon only when true (or via DEVKIT_DAEMON=1 / --supervise).
    pub enabled: bool,
    /// Exit after this many idle seconds with zero clients AND zero supervised children.
    pub idle_timeout_secs: u64,
    /// Crash-loop guard: restarts allowed within `restart_window_secs`.
    pub max_restarts: u32,
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

#[derive(Debug, Deserialize, Serialize)]
pub struct Defaults {
    pub worktree_root: String,
    pub branch_prefix: String,
    pub baseline_ref: String,
    pub baseline_path: String,
    #[serde(default)]
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
#[derive(Debug, Deserialize, Serialize)]
pub struct Person {
    pub slack: String,
    #[serde(default)]
    pub github: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
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
    /// Commands run in the app's directory during `issue setup`, in order. Each
    /// inner array is one argv (program + args), e.g.
    /// `[["doppler","run","-c","local","--","bun","install"]]`.
    #[serde(default)]
    pub setup: Vec<Vec<String>>,
    #[serde(default)]
    pub path: Option<String>,
    // Map fields kept last so the serialized TOML groups scalars/arrays before the
    // nested env tables — readable, stable output. (toml 0.8 also orders values
    // before tables on its own, so this is for layout, not a serializer requirement.)
    #[serde(default)]
    pub static_env: HashMap<String, String>,
    /// Env written to `<app>/.env.local` during `issue setup` (e.g. dummy workflow ids).
    #[serde(default)]
    pub prep_env: HashMap<String, String>,
}

impl Config {
    pub fn parse(s: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(s).context("parsing devkit.toml")?;
        Ok(cfg)
    }
}

/// `--config` → `$DEVKIT_CONFIG` → `./devkit.toml` walking up → `~/.config/devkit/config.toml`.
pub fn locate(explicit: Option<&Path>, start: &Path) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(p.to_path_buf());
    }
    if let Some(p) = std::env::var_os("DEVKIT_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let mut dir = Some(start);
    while let Some(d) = dir {
        let c = d.join("devkit.toml");
        if c.is_file() {
            return Some(c);
        }
        dir = d.parent();
    }
    let home = std::env::var_os("HOME")?;
    let fallback = PathBuf::from(home).join(".config/devkit/config.toml");
    fallback.is_file().then_some(fallback)
}

pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(h) = std::env::var_os("HOME")
    {
        return PathBuf::from(h).join(rest);
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
    pub(crate) const SAMPLE: &str = r#"
[defaults]
worktree_root = "~/Git/example"
branch_prefix = "lev/"
baseline_ref = "origin/staging"
baseline_path = "~/Git/example/_baseline"
doppler_yaml = "~/Git/example/monorepo/doppler.yaml"
[apps.api]
base_port = 9100
launch = ["doppler", "run", "-c", "dev_local", "--", "nitro", "dev", "--port", "{port}"]
url_env = "FOUNDRY_API_BASE_URL"
static_env = { SUPABASE_JWT_SECRET = "s" }
"#;
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
    fn config_serializes_app_with_static_env_and_trailing_scalars() {
        // An app carrying both a map field (static_env) and scalar/array fields
        // (setup, path) serializes cleanly with all keys present.
        let src = format!("{SAMPLE}setup = [[\"bun\", \"install\"]]\npath = \"apps/api\"\n");
        let c = Config::parse(&src).unwrap();
        let s = toml::to_string_pretty(&c).expect("serialize app with trailing scalars");
        assert!(s.contains("setup"));
        assert!(s.contains("path"));
    }
}
