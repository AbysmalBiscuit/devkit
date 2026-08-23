use anyhow::Result;
use devkit_config::{Config, DEFAULT_APP_URL};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct App {
    pub name: String,
    pub base_port: u16,
    pub path: String,
    pub launch: Vec<String>,
    pub url: Option<String>,
    pub url_env: Option<String>,
    pub provides_url: bool,
    pub static_env: HashMap<String, String>,
    pub prep_files: Vec<devkit_config::PrepFile>,
    pub setup: Vec<Vec<String>>,
}

impl App {
    /// The app's URL template, or the localhost default when it declares none.
    pub fn url_template(&self) -> &str {
        self.url.as_deref().unwrap_or(DEFAULT_APP_URL)
    }
}

/// Build the catalog: an app's path comes from its explicit `path` or is inferred from
/// doppler.yaml; an app whose path resolves to neither is skipped with a warning.
///
/// An app whose path can be resolved neither from config nor from doppler.yaml is
/// skipped with a warning rather than failing the whole catalog — a config may list
/// apps that aren't present in every checkout. Requesting such an app surfaces a
/// plain "unknown app" error at the call site.
pub fn catalog(
    cfg: &Config,
    path_to_project: &HashMap<String, String>,
) -> Result<HashMap<String, App>> {
    let mut out = HashMap::new();
    for (name, a) in &cfg.apps {
        let Some(path) = a
            .path
            .clone()
            .or_else(|| guess_path(&cfg.defaults.apps_dir, name, path_to_project))
        else {
            eprintln!(
                "note: skipping app `{name}` — no path in config and none inferrable from doppler.yaml"
            );
            continue;
        };
        out.insert(
            name.clone(),
            App {
                name: name.clone(),
                base_port: a.base_port,
                path,
                launch: a.launch.clone(),
                url: a.url.clone(),
                url_env: a.url_env.clone(),
                provides_url: a.provides_url,
                static_env: a.static_env.clone(),
                prep_files: a.prep_files.clone(),
                setup: a.setup.clone(),
            },
        );
    }
    Ok(out)
}

fn guess_path(apps_dir: &str, name: &str, p2p: &HashMap<String, String>) -> Option<String> {
    let cand = format!("{apps_dir}/{name}");
    p2p.contains_key(&cand).then_some(cand)
}

#[cfg(test)]
mod tests {
    use super::*;
    use devkit_config::Config;

    // `devkit_config::tests_sample()` is `#[cfg(test)]` in devkit-config, so it
    // is NOT compiled into the crate when devkit-ports builds its own tests (a
    // dependency builds without its own test cfg) — inline the same sample.
    const SAMPLE: &str = r#"
[defaults]
worktree_root = "~/Git/example"
branch_prefix = "lev/"
baseline_ref = "origin/staging"
baseline_path = "~/Git/example/_baseline"
doppler_yaml = "~/Git/example/monorepo/doppler.yaml"
[apps.api]
base_port = 9100
launch = ["doppler", "run", "-c", "dev_local", "--", "nitro", "dev", "--port", "{{ port }}"]
url_env = "FOUNDRY_API_BASE_URL"
static_env = { SUPABASE_JWT_SECRET = "s" }
"#;

    #[test]
    fn infers_path_from_doppler_yaml() {
        let cfg = Config::parse(SAMPLE).unwrap();
        let mut p2p = HashMap::new();
        p2p.insert("apps/api".to_string(), "api-foundry".to_string());
        let cat = catalog(&cfg, &p2p).unwrap();
        // `api` has no explicit `path`; it is inferred from the doppler.yaml key.
        assert_eq!(cat["api"].path, "apps/api");
    }

    #[test]
    fn skips_apps_with_unresolvable_path() {
        // `api` has no `path` in the sample; without a doppler entry for it, it is
        // skipped rather than erroring the whole catalog.
        let cfg = Config::parse(SAMPLE).unwrap();
        let cat = catalog(&cfg, &HashMap::new()).unwrap();
        assert!(cat.is_empty());
    }
}
