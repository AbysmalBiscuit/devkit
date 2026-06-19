use crate::config::{AppConfig, Config};
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
    pub preserve_env: Vec<String>,
    pub static_env: HashMap<String, String>,
}

/// Build the catalog: project+path come from doppler.yaml unless the app overrides them.
pub fn catalog(cfg: &Config, path_to_project: &HashMap<String, String>) -> Result<HashMap<String, App>> {
    let mut out = HashMap::new();
    for (name, a) in &cfg.apps {
        let path = a.path.clone().or_else(|| guess_path(name, path_to_project))
            .ok_or_else(|| anyhow::anyhow!("app `{name}`: no path in config and none inferrable from doppler.yaml"))?;
        let project = a.doppler_project.clone().or_else(|| path_to_project.get(&path).cloned());
        out.insert(name.clone(), App {
            name: name.clone(),
            base_port: a.base_port,
            doppler_project: project,
            path,
            launch: a.launch.clone(),
            url_env: a.url_env.clone(),
            preserve_env: a.preserve_env.clone(),
            static_env: a.static_env.clone(),
        });
    }
    Ok(out)
}

fn guess_path(name: &str, p2p: &HashMap<String, String>) -> Option<String> {
    let cand = format!("apps/{name}");
    p2p.contains_key(&cand).then_some(cand)
}

#[allow(dead_code)]
fn _use(_: &AppConfig) {}

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
}
