//! `devrun` server-lifecycle operations as a library facade, shared by the
//! `devrun` CLI and the MCP `devrun.*` handlers. Keeping the logic here (not in
//! the binary) is what lets the MCP server call it directly instead of shelling
//! out to `devrun`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::apps::App;
use crate::registry::{self, Data, Role};

/// Build the doppler argv prefix: `doppler run -p <project> -c <config> [--preserve-env=K]... --`
pub fn doppler_prefix(app: &App, config: &str) -> Vec<String> {
    let mut v = vec!["doppler".into(), "run".into()];
    if let Some(p) = &app.doppler_project {
        v.push("-p".into());
        v.push(p.clone());
    }
    v.push("-c".into());
    v.push(config.into());
    for k in &app.preserve_env {
        v.push(format!("--preserve-env={k}"));
    }
    v.push("--".into());
    v
}

/// Resolve `{port}` in the launch argv.
pub fn launch_argv(app: &App, port: u16) -> Vec<String> {
    app.launch
        .iter()
        .map(|a| a.replace("{port}", &port.to_string()))
        .collect()
}

/// Env layering (low→high): static_env → url-wiring → user overrides.
/// `provider_port` is the port of the URL-providing app (the API), if it shares the run.
pub fn env_for(
    app: &App,
    provider_port: Option<u16>,
    user: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for (k, v) in &app.static_env {
        env.insert(k.clone(), v.clone());
    }
    if let (Some(var), Some(p)) = (url_consumer_var(app), provider_port) {
        env.insert(var, format!("http://localhost:{p}"));
    }
    for (k, v) in user {
        env.insert(k.clone(), v.clone());
    }
    env
}

/// The env var a consumer reads to reach the URL-providing app. The provider's own
/// `url_env` names the same var but it doesn't consume itself, so skip the provider.
fn url_consumer_var(app: &App) -> Option<String> {
    if app.provides_url {
        None
    } else {
        app.url_env.clone()
    }
}

/// Readiness of a tracked server.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerState {
    /// The port accepts connections.
    Ready,
    /// The pid is alive but the port is not yet accepting.
    Starting,
    /// The pid is gone (or absent) and the port is not accepting.
    Crashed,
}

/// One tracked server, machine-readable for the MCP `devrun.status`/`devrun.up`
/// results (the CLI keeps its own table rendering).
#[derive(Debug, Clone, Serialize)]
pub struct ServerStatus {
    pub app: String,
    pub role: Role,
    pub port: u16,
    pub pid: Option<u32>,
    pub logfile: Option<PathBuf>,
    pub state: ServerState,
}

/// Classify a tracked server: listening → Ready; else live pid → Starting; else Crashed.
fn server_state(port: u16, pid: Option<u32>) -> ServerState {
    if registry::listening(port) {
        ServerState::Ready
    } else if pid.is_some_and(registry::pid_alive) {
        ServerState::Starting
    } else {
        ServerState::Crashed
    }
}

/// Structured per-server rows from a registry snapshot, optionally limited to one holder.
pub fn server_rows(data: &Data, only_holder: Option<&str>) -> Vec<ServerStatus> {
    let mut rows = Vec::new();
    for (port, e) in &data.entries {
        if let Some(h) = only_holder
            && e.holder != h
        {
            continue;
        }
        rows.push(ServerStatus {
            app: e.app.clone(),
            role: e.role,
            port: *port,
            pid: e.pid,
            logfile: e.logfile.clone(),
            state: server_state(*port, e.pid),
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn app(name: &str, url_env: Option<&str>) -> App {
        App {
            name: name.into(),
            base_port: 1,
            doppler_project: Some("proj".into()),
            path: "apps/x".into(),
            launch: vec!["next".into(), "dev".into(), "-p".into(), "{port}".into()],
            url_env: url_env.map(Into::into),
            provides_url: false,
            preserve_env: vec![],
            static_env: HashMap::new(),
            prep_env: HashMap::new(),
            setup: vec![],
        }
    }

    #[test]
    fn provider_does_not_wire_its_own_url() {
        let mut api = app("api", Some("FOUNDRY_API_BASE_URL"));
        api.provides_url = true;
        let e = env_for(&api, Some(9100), &BTreeMap::new());
        assert!(!e.contains_key("FOUNDRY_API_BASE_URL"));
    }

    #[test]
    fn wires_api_url_for_consumer() {
        let e = env_for(
            &app("lab-os", Some("FOUNDRY_API_BASE_URL")),
            Some(9103),
            &BTreeMap::new(),
        );
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
        assert_eq!(
            launch_argv(&app("lab-os", None), 4103),
            vec!["next", "dev", "-p", "4103"]
        );
    }

    #[test]
    fn server_rows_marks_a_pidless_unbound_entry_crashed() {
        // Pick a definitely-free port by binding then dropping a listener.
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let mut data = Data::default();
        data.entries.insert(
            port,
            crate::registry::Entry {
                app: "web".into(),
                holder: "/w".into(),
                role: Role::Issue,
                pid: None,
                logfile: None,
                ts: crate::registry::now(),
            },
        );
        let rows = server_rows(&data, Some("/w"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, ServerState::Crashed);
        assert_eq!(rows[0].app, "web");

        // A different holder filter excludes it.
        assert!(server_rows(&data, Some("/other")).is_empty());
    }

    #[test]
    fn server_rows_marks_a_listening_entry_ready() {
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let mut data = Data::default();
        data.entries.insert(
            port,
            crate::registry::Entry {
                app: "web".into(),
                holder: "/w".into(),
                role: Role::Issue,
                pid: None,
                logfile: None,
                ts: crate::registry::now(),
            },
        );
        let rows = server_rows(&data, None);
        assert_eq!(
            rows[0].state,
            ServerState::Ready,
            "bound port reads as ready"
        );
        drop(l);
    }
}
