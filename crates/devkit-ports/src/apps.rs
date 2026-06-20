use crate::config::Config;
use anyhow::Result;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct App {
    pub name: String,
    pub base_port: u16,
    pub doppler_project: Option<String>,
    pub path: String,
    pub launch: Vec<String>,
    pub url_env: Option<String>,
    pub provides_url: bool,
    pub preserve_env: Vec<String>,
    pub static_env: HashMap<String, String>,
    pub prep_env: HashMap<String, String>,
    pub setup: Vec<Vec<String>>,
}

/// Build the catalog: project+path come from doppler.yaml unless the app overrides them.
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
        let project = a
            .doppler_project
            .clone()
            .or_else(|| path_to_project.get(&path).cloned());
        out.insert(
            name.clone(),
            App {
                name: name.clone(),
                base_port: a.base_port,
                doppler_project: project,
                path,
                launch: a.launch.clone(),
                url_env: a.url_env.clone(),
                provides_url: a.provides_url,
                preserve_env: a.preserve_env.clone(),
                static_env: a.static_env.clone(),
                prep_env: a.prep_env.clone(),
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
    use crate::config::Config;
    #[test]
    fn pulls_project_from_doppler() {
        let cfg = Config::parse(crate::config::tests_sample()).unwrap();
        let mut p2p = HashMap::new();
        p2p.insert("apps/api".to_string(), "api-foundry".to_string());
        let cat = catalog(&cfg, &p2p).unwrap();
        assert_eq!(cat["api"].doppler_project.as_deref(), Some("api-foundry"));
        assert_eq!(cat["api"].path, "apps/api");
    }

    #[test]
    fn skips_apps_with_unresolvable_path() {
        // `api` has no `path` in the sample; without a doppler entry for it, it is
        // skipped rather than erroring the whole catalog.
        let cfg = Config::parse(crate::config::tests_sample()).unwrap();
        let cat = catalog(&cfg, &HashMap::new()).unwrap();
        assert!(cat.is_empty());
    }
}
