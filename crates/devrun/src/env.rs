use devkit_ports::apps::App;
use std::collections::BTreeMap;

/// Build the doppler argv prefix: `doppler run -p <project> -c <config> [--preserve-env=K]... --`
pub fn doppler_prefix(app: &App, config: &str) -> Vec<String> {
    let mut v = vec!["doppler".into(), "run".into()];
    if let Some(p) = &app.doppler_project { v.push("-p".into()); v.push(p.clone()); }
    v.push("-c".into()); v.push(config.into());
    for k in &app.preserve_env { v.push(format!("--preserve-env={k}")); }
    v.push("--".into());
    v
}

/// Resolve `{port}` in the launch argv.
pub fn launch_argv(app: &App, port: u16) -> Vec<String> {
    app.launch.iter().map(|a| a.replace("{port}", &port.to_string())).collect()
}

/// Env layering (low→high): static_env → url-wiring → user overrides.
/// `api_port` is this role's api port, if api is in the same run.
pub fn env_for(
    app: &App, api_port: Option<u16>, user: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for (k, v) in &app.static_env { env.insert(k.clone(), v.clone()); }
    if let (Some(var), Some(p)) = (api_url_consumer(app), api_port) {
        env.insert(var, format!("http://localhost:{p}"));
    }
    for (k, v) in user { env.insert(k.clone(), v.clone()); }
    env
}

/// The env var THIS app reads to reach the api (its own url_env is for OTHERS;
/// a consumer's wiring var is configured the same — we set it when api shares the run).
fn api_url_consumer(app: &App) -> Option<String> {
    // Convention: a webapp's url_env names the var it *also consumes* to reach the api.
    // For api itself url_env is set but it doesn't consume itself, so skip name == api.
    if app.name == "api" { None } else { app.url_env.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    fn app(name: &str, url_env: Option<&str>) -> App {
        App { name: name.into(), base_port: 1, doppler_project: Some("proj".into()),
            path: "apps/x".into(), launch: vec!["next".into(),"dev".into(),"-p".into(),"{port}".into()],
            url_env: url_env.map(Into::into), preserve_env: vec![], static_env: HashMap::new() }
    }
    #[test]
    fn wires_api_url_for_consumer() {
        let e = env_for(&app("lab-os", Some("FOUNDRY_API_BASE_URL")), Some(9103), &BTreeMap::new());
        assert_eq!(e["FOUNDRY_API_BASE_URL"], "http://localhost:9103");
    }
    #[test]
    fn user_override_wins() {
        let mut u = BTreeMap::new();
        u.insert("FOUNDRY_API_BASE_URL".into(), "http://x".into());
        let e = env_for(&app("lab-os", Some("FOUNDRY_API_BASE_URL")), Some(9103), &u);
        assert_eq!(e["FOUNDRY_API_BASE_URL"], "http://x");
    }
    #[test]
    fn launch_substitutes_port() {
        assert_eq!(launch_argv(&app("lab-os", None), 4103), vec!["next","dev","-p","4103"]);
    }
}
