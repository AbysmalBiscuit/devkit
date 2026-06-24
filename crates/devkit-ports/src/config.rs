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

/// A file written into an app's directory during `issue setup`, before the app's
/// `setup` commands run. `content` is written verbatim — no format assembly or
/// newline injection. Parent directories are created. Existing files are left
/// untouched unless `overwrite` is set.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PrepFile {
    /// Target path, relative to the app's directory.
    pub path: String,
    /// File contents, written byte-for-byte.
    pub content: String,
    /// Overwrite an existing file rather than skipping it.
    #[serde(default)]
    pub overwrite: bool,
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
    // Table-like fields (`static_env`, `prep_files`) kept last so the serialized
    // TOML groups scalars/arrays before the nested table and array-of-tables —
    // readable, stable output. (toml 0.8 also orders values before tables on its
    // own, so this is for source layout, not a serializer requirement.)
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
    let body = std::fs::read_to_string(p)
        .with_context(|| format!("reading config layer {}", p.display()))?;
    let table: toml::Table =
        toml::from_str(&body).with_context(|| format!("parsing config layer {}", p.display()))?;
    Ok((p.to_path_buf(), table))
}

fn home_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/devkit/config.toml"))
}

/// Whether a parsed layer declares `[config] root = true` (stop walking upward).
fn is_root_layer(t: &toml::Table) -> bool {
    t.get("config")
        .and_then(|c| c.as_table())
        .and_then(|c| c.get("root"))
        .and_then(|r| r.as_bool())
        .unwrap_or(false)
}

/// Build the ordered layer list (lowest→highest precedence): the home config (unless
/// a `root = true` marker cuts it off), then each `devkit.toml` from the filesystem
/// root down to `start`. An explicit path or `$DEVKIT_CONFIG` is the sole layer.
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

    // Walk upward collecting devkit.toml files (deepest first); stop at a root marker.
    let mut stack: Vec<(PathBuf, toml::Table)> = Vec::new();
    let mut rooted = false;
    let mut dir = Some(start);
    while let Some(d) = dir {
        let c = d.join("devkit.toml");
        if c.is_file() {
            let layer = read_layer(&c)?;
            let is_root = is_root_layer(&layer.1);
            stack.push(layer);
            if is_root {
                rooted = true;
                break;
            }
        }
        dir = d.parent();
    }

    let mut layers: Vec<(PathBuf, toml::Table)> = Vec::new();
    if !rooted
        && let Some(h) = home
        && h.is_file()
    {
        layers.push(read_layer(h)?);
    }
    stack.reverse(); // deepest-first → shallowest-first (lowest precedence first)
    layers.extend(stack);

    if layers.is_empty() {
        anyhow::bail!(
            "no devkit.toml found (--config / $DEVKIT_CONFIG / ./devkit.toml walking up / ~/.config/devkit/config.toml)"
        );
    }
    Ok(layers)
}

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
    let layers = discover(explicit, start, home)?;
    let order: Vec<PathBuf> = layers.iter().map(|(p, _)| p.clone()).collect();
    let (merged, origin) = merge_layers(&layers);
    let cfg: Config = toml::Value::Table(merged)
        .try_into()
        .context("deserializing merged devkit config")?;
    Ok((
        cfg,
        Provenance {
            layers: order,
            origin,
        },
    ))
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

    fn unique_tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("devkit-cfg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const FULL_DEFAULTS: &str =
        "worktree_root='/w'\nbranch_prefix='x/'\nbaseline_ref='r'\nbaseline_path='/b'\n";

    #[test]
    fn resolve_merges_parent_and_child() {
        let root = unique_tmp("merge");
        let child = root.join("repo");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(
            root.join("devkit.toml"),
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
        assert_eq!(cfg.defaults.worktree_root, "/w"); // inherited from parent
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
        let root = unique_tmp("rooted");
        let child = root.join("repo");
        std::fs::create_dir_all(&child).unwrap();
        let home = root.join("home.toml");
        std::fs::write(&home, "[defaults]\nbranch_prefix='HOME/'\n").unwrap();
        std::fs::write(
            root.join("devkit.toml"),
            "[defaults]\nworktree_root='/PARENT'\n",
        )
        .unwrap();
        std::fs::write(
            child.join("devkit.toml"),
            format!("[config]\nroot=true\n[defaults]\n{FULL_DEFAULTS}[apps.api]\nbase_port=2\nlaunch=['a']\n"),
        )
        .unwrap();
        let (cfg, prov) = resolve_with_home(None, &child, Some(&home)).unwrap();
        assert_eq!(cfg.defaults.worktree_root, "/w"); // parent's /PARENT dropped
        assert_eq!(cfg.defaults.branch_prefix, "x/"); // home's HOME/ dropped
        assert_eq!(prov.layers, vec![child.join("devkit.toml")]);
    }

    #[test]
    fn home_layer_is_lowest_precedence() {
        let root = unique_tmp("home");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let home = root.join("home.toml");
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
    fn explicit_config_bypasses_layering() {
        let root = unique_tmp("explicit");
        let child = root.join("repo");
        std::fs::create_dir_all(&child).unwrap();
        let explicit = root.join("custom.toml");
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
        let root = unique_tmp("empty");
        let err = resolve_with_home(None, &root, None).unwrap_err();
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
}
