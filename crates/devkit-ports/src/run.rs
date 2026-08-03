//! `devrun` server-lifecycle operations as a library facade, shared by the
//! `devrun` CLI and the MCP `devrun.*` handlers. Keeping the logic here (not in
//! the binary) is what lets the MCP server call it directly instead of shelling
//! out to `devrun`.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::apps::App;
use crate::registry::{self, Data, Role};
use devkit_common::{paths, supervise};

/// Env layering (low→high): static_env → url-wiring → user overrides.
/// `provider_url` is the URL-providing app's rendered `url`, if it shares the run.
pub fn env_for(
    app: &App,
    provider_url: Option<&str>,
    user: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for (k, v) in &app.static_env {
        env.insert(k.clone(), v.clone());
    }
    if let (Some(var), Some(url)) = (url_consumer_var(app), provider_url) {
        env.insert(var, url.to_string());
    }
    for (k, v) in user {
        env.insert(k.clone(), v.clone());
    }
    env
}

/// Resolve the Doppler config a launch would use *from inputs devkit already
/// holds*: an explicit `-c`/`--config` flag in the launch argv (highest
/// precedence, scanned only up to the `--` separator), else `DOPPLER_CONFIG` in
/// the resolved env. Returns `None` when the launch is not a Doppler invocation
/// or specifies no inline config.
pub fn config_from_argv_env(argv: &[String], env: &BTreeMap<String, String>) -> Option<String> {
    let prog = argv.first()?;
    if Path::new(prog).file_name().and_then(|s| s.to_str()) != Some("doppler") {
        return None;
    }
    let mut it = argv.iter().skip(1);
    while let Some(a) = it.next() {
        if a == "--" {
            break;
        }
        if a == "-c" || a == "--config" {
            if let Some(v) = it.next() {
                return Some(v.clone());
            }
        } else if let Some(v) = a
            .strip_prefix("-c=")
            .or_else(|| a.strip_prefix("--config="))
        {
            return Some(v.to_string());
        }
    }
    env.get("DOPPLER_CONFIG").cloned()
}

/// Best-effort read of the locally-scoped Doppler config for `cwd` via
/// `doppler configure get config --plain --scope <cwd>`. This reads the persisted
/// scope (`~/.doppler/.doppler.yaml`) and does *not* fetch secrets. Returns `None`
/// if `doppler` is absent, exits non-zero, or prints nothing.
fn doppler_scoped_config(cwd: &Path) -> Option<String> {
    let out = std::process::Command::new("doppler")
        .args(["configure", "get", "config", "--plain", "--scope"])
        .arg(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Refuse to run a Doppler invocation against the `prd` config. `label` names
/// the app or task in the error. A non-doppler `argv` is unguarded. The config
/// is resolved in Doppler's own precedence order — explicit flag, then
/// `DOPPLER_CONFIG`, then the local scope for `cwd` — and an invocation whose
/// config resolves to `prd`, or cannot be resolved at all, is rejected
/// (fail-safe).
pub fn assert_not_prd(
    label: &str,
    argv: &[String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<()> {
    let prog = argv.first().map(String::as_str).unwrap_or_default();
    if Path::new(prog).file_name().and_then(|s| s.to_str()) != Some("doppler") {
        return Ok(());
    }
    let config = config_from_argv_env(argv, env).or_else(|| doppler_scoped_config(cwd));
    match config.as_deref() {
        Some("prd") => anyhow::bail!(
            "refusing to launch `{label}`: doppler config resolves to `prd` (production secrets)"
        ),
        Some(_) => Ok(()),
        None => anyhow::bail!(
            "refusing to launch `{label}`: cannot determine its doppler config (no -c/--config, \
             no DOPPLER_CONFIG, no local scope). Add an explicit `-c <config>` to its launch."
        ),
    }
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

/// Classify from already-probed signals: listening → Ready; else live pid →
/// Starting; else Crashed. Pure, so the mapping is testable without binding ports.
fn classify(listening: bool, pid_alive: bool) -> ServerState {
    if listening {
        ServerState::Ready
    } else if pid_alive {
        ServerState::Starting
    } else {
        ServerState::Crashed
    }
}

/// Classify a tracked server: listening → Ready; else live pid → Starting; else Crashed.
fn server_state(port: u16, pid: Option<u32>) -> ServerState {
    classify(
        registry::listening(port),
        pid.is_some_and(registry::pid_alive),
    )
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

/// The directory leaf of a holder path, used to namespace a worktree's log dir.
pub fn holder_slug(holder: &str) -> String {
    Path::new(holder)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("wt")
        .to_string()
}

/// Ensure the URL-providing app (the API) is present whenever a selected app
/// consumes its URL, so the consumer can be wired. The provider is identified by
/// config (`provides_url`), not by name.
pub fn ensure_provider(catalog: &HashMap<String, App>, apps: &mut Vec<String>) {
    let provider = catalog
        .iter()
        .find(|(_, a)| a.provides_url)
        .map(|(n, _)| n.clone());
    let needs_provider = apps
        .iter()
        .any(|a| catalog[a].url_env.is_some() && !catalog[a].provides_url);
    if needs_provider
        && let Some(p) = provider
        && !apps.contains(&p)
    {
        apps.insert(0, p);
    }
}

/// A fully-resolved launch command for one app: ready to print (dry-run) or spawn.
#[derive(Debug, Clone)]
pub struct LaunchPlan {
    pub app: String,
    pub port: u16,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub log: PathBuf,
    /// The app's rendered `url`, as reported to the user.
    pub url: String,
}

/// Ports one (role, holder) group needs: each selected app's own port plus
/// every app referenced via `ports[...]` in the group's launch argv and
/// static_env templates. One `registry::alloc` covers them all, so a
/// reference to an app that isn't running writes the normal pid-less
/// reservation a later `up` claims.
pub fn resolve_ports(
    catalog: &HashMap<String, App>,
    apps: &[String],
    holder: &str,
    role: Role,
    variables: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, u16>> {
    let mut names: Vec<String> = apps.to_vec();
    for a in apps {
        let app = &catalog[a];
        let mut templates: Vec<&str> = app.launch.iter().map(String::as_str).collect();
        templates.extend(app.static_env.values().map(String::as_str));
        templates.push(app.url_template());
        let refs = devkit_common::template::referenced_ports(&templates, variables)
            .with_context(|| format!("scanning templates of app `{a}`"))?;
        for r in refs.apps {
            anyhow::ensure!(
                catalog.contains_key(&r),
                "app `{a}` references unknown app `{r}` via ports[...]"
            );
            if !names.contains(&r) {
                names.push(r);
            }
        }
    }
    let reqs: Vec<(String, u16)> = names
        .iter()
        .map(|n| (n.clone(), catalog[n].base_port))
        .collect();
    Ok(registry::alloc(holder, &reqs, role)?.into_iter().collect())
}

/// Build a launch plan per app for one (role, holder) group. `ports` maps each app
/// to its allocated port; `provider` names the URL-providing app if it shares the run.
#[allow(clippy::too_many_arguments)]
pub fn plan_group(
    catalog: &HashMap<String, App>,
    apps: &[String],
    ports: &BTreeMap<String, u16>,
    provider: Option<&str>,
    base_dir: &Path,
    role: Role,
    user_env: &BTreeMap<String, String>,
    variables: &BTreeMap<String, String>,
) -> Result<Vec<LaunchPlan>> {
    use devkit_common::template::render_launch;
    let provider_url =
        match provider.and_then(|p| Some((p, catalog.get(p)?, ports.get(p).copied()?))) {
            Some((name, app, port)) => Some(
                render_launch(app.url_template(), Some(port), ports, variables)
                    .with_context(|| format!("rendering url of `{name}`"))?,
            ),
            None => None,
        };
    let mut plans = Vec::with_capacity(apps.len());
    for a in apps {
        let app = &catalog[a];
        let port = ports[a];
        let argv = app
            .launch
            .iter()
            .map(|t| render_launch(t, Some(port), ports, variables))
            .collect::<Result<Vec<_>>>()
            .with_context(|| format!("rendering launch argv of `{a}`"))?;
        let mut rendered = app.clone();
        rendered.static_env = app
            .static_env
            .iter()
            .map(|(k, v)| {
                Ok((
                    k.clone(),
                    render_launch(v, Some(port), ports, variables)
                        .with_context(|| format!("rendering static_env `{k}` of `{a}`"))?,
                ))
            })
            .collect::<Result<_>>()?;
        let cwd = base_dir.join(&app.path);
        let env = env_for(&rendered, provider_url.as_deref(), user_env);
        let url = render_launch(app.url_template(), Some(port), ports, variables)
            .with_context(|| format!("rendering url of `{a}`"))?;
        let log = paths::logs_dir()
            .join(holder_slug(base_dir.to_str().unwrap_or("wt")))
            .join(format!("{}-{}.log", role.as_str(), a));
        plans.push(LaunchPlan {
            app: a.clone(),
            port,
            argv,
            cwd,
            env,
            log,
            url,
        });
    }
    Ok(plans)
}

/// Result of stopping + releasing a holder's servers.
#[derive(Debug, Clone, Serialize)]
pub struct DownOutcome {
    /// Processes that received SIGTERM (0 on the daemon path, which stops them itself).
    pub stopped: usize,
    /// Ports released.
    pub freed: Vec<u16>,
    /// Whether a running daemon handled the stop.
    pub via_daemon: bool,
}

/// Stop every server for `holder` (optionally one role) and release its ports.
/// Prefers a running daemon; otherwise stops + releases directly under one lock,
/// without pruning first (a still-running server whose reservation looks stale
/// must still receive SIGTERM).
pub fn bring_down(holder: &str, role: Option<Role>) -> Result<DownOutcome> {
    #[cfg(feature = "daemon")]
    if let Some(mut client) = crate::daemon::client::try_existing() {
        let resp = client.request(&crate::daemon::proto::Request::Down {
            holder: holder.to_string(),
            role,
        })?;
        if let crate::daemon::proto::Response::Freed(freed) = resp {
            return Ok(DownOutcome {
                stopped: freed.len(),
                freed,
                via_daemon: true,
            });
        }
    }
    let mut stopped = 0;
    let freed = registry::with_lock(|d| {
        for e in d.entries.values() {
            if e.holder == holder
                && role.is_none_or(|r| e.role == r)
                && let Some(pid) = e.pid
            {
                supervise::stop(pid);
                stopped += 1;
            }
        }
        Ok(d.release(holder, role))
    })?;
    Ok(DownOutcome {
        stopped,
        freed,
        via_daemon: false,
    })
}

/// Stop + release exactly the listed ports. Prefers a running daemon (precise
/// `DownPorts`); otherwise SIGTERMs each port's pid and removes its row under one
/// lock, without pruning first (the still-running-but-stale invariant).
pub fn bring_down_ports(ports: &[u16]) -> Result<DownOutcome> {
    #[cfg(feature = "daemon")]
    if let Some(mut client) = crate::daemon::client::try_existing() {
        let resp = client.request(&crate::daemon::proto::Request::DownPorts {
            ports: ports.to_vec(),
        })?;
        if let crate::daemon::proto::Response::Freed(freed) = resp {
            return Ok(DownOutcome {
                stopped: freed.len(),
                freed,
                via_daemon: true,
            });
        }
    }
    let want: std::collections::BTreeSet<u16> = ports.iter().copied().collect();
    let mut stopped = 0;
    let freed = registry::with_lock(|d| {
        for (port, e) in d.entries.iter() {
            if want.contains(port)
                && let Some(pid) = e.pid
            {
                supervise::stop(pid);
                stopped += 1;
            }
        }
        Ok(d.release_ports(ports))
    })?;
    Ok(DownOutcome {
        stopped,
        freed,
        via_daemon: false,
    })
}

/// Return the last `lines` lines of a tracked app's logfile for this worktree.
pub fn read_log(holder: &str, app: &str, role: Option<Role>, lines: usize) -> Result<String> {
    let data = registry::snapshot()?;
    let log = data
        .entries
        .values()
        .find(|e| e.holder == holder && e.app == app && role.is_none_or(|r| e.role == r))
        .and_then(|e| e.logfile.clone())
        .ok_or_else(|| anyhow::anyhow!("no tracked log for app `{app}` in this worktree"))?;
    Ok(supervise::tail(&log, lines))
}

/// Is a supervisor daemon already running? Used to decide whether `up` hands
/// servers to the daemon. Never starts one.
pub fn daemon_running() -> bool {
    #[cfg(feature = "daemon")]
    {
        crate::daemon::client::try_existing().is_some()
    }
    #[cfg(not(feature = "daemon"))]
    {
        false
    }
}

/// The plan's port row when it already tracks a live process for the same
/// holder+app+role. `up` reports such a server instead of respawning it:
/// a duplicate would fail to bind, and on the daemon path would repoint the
/// supervision table at the doomed pid.
fn existing_server(
    data: &Data,
    plan: &LaunchPlan,
    holder: &str,
    role: Role,
) -> Option<ServerStatus> {
    let e = data.entries.get(&plan.port)?;
    if e.holder != holder || e.app != plan.app || e.role != role {
        return None;
    }
    let pid = e.pid?;
    if !registry::pid_alive(pid) {
        return None;
    }
    Some(ServerStatus {
        app: plan.app.clone(),
        role,
        port: plan.port,
        pid: Some(pid),
        logfile: e.logfile.clone(),
        state: server_state(plan.port, Some(pid)),
    })
}

/// Spawn (or hand to the daemon) every plan in one group and record each pid.
/// A plan whose `(holder, app, role)` row already has a live pid is skipped
/// and reported as existing rather than spawned again. `wait = true` blocks
/// up to 120 s per port for readiness (the CLI path); `wait = false` returns
/// immediately with each server in its current state.
pub fn launch(
    plans: &[LaunchPlan],
    holder: &str,
    role: Role,
    supervise_daemon: bool,
    wait: bool,
) -> Result<Vec<ServerStatus>> {
    for p in plans {
        assert_not_prd(&p.app, &p.argv, &p.env, &p.cwd)?;
    }
    #[cfg(feature = "daemon")]
    if supervise_daemon {
        return supervise_via_daemon(plans, holder, role);
    }
    #[cfg(not(feature = "daemon"))]
    let _ = supervise_daemon;

    let data = registry::snapshot()?;
    let mut existing = Vec::new();
    let mut pending: Vec<&LaunchPlan> = Vec::new();
    for p in plans {
        match existing_server(&data, p, holder, role) {
            Some(s) => existing.push(s),
            None => pending.push(p),
        }
    }

    let mut spawned = Vec::with_capacity(pending.len());
    for p in pending {
        let pid = supervise::spawn_detached(
            &p.argv,
            p.cwd.to_str().context("app cwd not UTF-8")?,
            &p.env,
            &p.log,
            None,
        )?;
        registry::record_pid(p.port, &p.app, holder, role, pid, p.log.clone())?;
        spawned.push((p.app.clone(), p.port, p.log.clone(), pid));
    }

    if wait {
        let ready: BTreeMap<String, bool> = std::thread::scope(|s| {
            let handles: Vec<_> = spawned
                .iter()
                .map(|(a, port, _, _)| {
                    let (a, port) = (a.clone(), *port);
                    s.spawn(move || {
                        (
                            a,
                            supervise::wait_ready(port, std::time::Duration::from_secs(120)),
                        )
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let mut out: Vec<ServerStatus> = spawned
            .into_iter()
            .map(|(a, port, log, pid)| ServerStatus {
                app: a.clone(),
                role,
                port,
                pid: Some(pid),
                logfile: Some(log),
                state: if ready[&a] {
                    ServerState::Ready
                } else {
                    ServerState::Starting
                },
            })
            .collect();
        out.extend(existing);
        Ok(out)
    } else {
        let mut out: Vec<ServerStatus> = spawned
            .into_iter()
            .map(|(a, port, log, pid)| ServerStatus {
                app: a,
                role,
                port,
                pid: Some(pid),
                logfile: Some(log),
                state: server_state(port, Some(pid)),
            })
            .collect();
        out.extend(existing);
        Ok(out)
    }
}

#[cfg(feature = "daemon")]
fn supervise_via_daemon(
    plans: &[LaunchPlan],
    holder: &str,
    role: Role,
) -> Result<Vec<ServerStatus>> {
    let mut client =
        crate::daemon::client::ensure_running().context("starting supervisor daemon")?;
    let mut out = Vec::with_capacity(plans.len());
    for p in plans {
        let resp = client.request(&crate::daemon::proto::Request::Supervise {
            holder: holder.to_string(),
            app: p.app.clone(),
            role,
            argv: p.argv.clone(),
            cwd: p.cwd.to_str().context("app cwd not UTF-8")?.to_string(),
            env: p.env.clone(),
            logfile: p.log.clone(),
            base_port: p.port,
        })?;
        let ready = match &resp {
            crate::daemon::proto::Response::Supervised(v) => {
                v.first().map(|(_, r)| *r).unwrap_or(false)
            }
            crate::daemon::proto::Response::Err(msg) => {
                eprintln!("daemon could not supervise {}: {msg}", p.app);
                false
            }
            _ => false,
        };
        out.push(ServerStatus {
            app: p.app.clone(),
            role,
            port: p.port,
            pid: None,
            logfile: Some(p.log.clone()),
            state: if ready {
                ServerState::Ready
            } else {
                ServerState::Starting
            },
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn app(name: &str, url_env: Option<&str>) -> App {
        App {
            name: name.into(),
            base_port: 1,
            path: "apps/x".into(),
            launch: vec![
                "next".into(),
                "dev".into(),
                "-p".into(),
                "{{ port }}".into(),
            ],
            url: None,
            url_env: url_env.map(Into::into),
            provides_url: false,
            static_env: HashMap::new(),
            prep_files: vec![],
            setup: vec![],
        }
    }

    #[test]
    fn provider_does_not_wire_its_own_url() {
        let mut api = app("api", Some("FOUNDRY_API_BASE_URL"));
        api.provides_url = true;
        let e = env_for(&api, Some("http://localhost:9100"), &BTreeMap::new());
        assert!(!e.contains_key("FOUNDRY_API_BASE_URL"));
    }

    #[test]
    fn wires_api_url_for_consumer() {
        let e = env_for(
            &app("lab-os", Some("FOUNDRY_API_BASE_URL")),
            Some("http://localhost:9103"),
            &BTreeMap::new(),
        );
        assert_eq!(e["FOUNDRY_API_BASE_URL"], "http://localhost:9103");
    }

    #[test]
    fn config_from_explicit_flag() {
        let env = BTreeMap::new();
        let v = |a: &[&str]| {
            config_from_argv_env(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>(), &env)
        };
        assert_eq!(
            v(&["doppler", "run", "-c", "prd", "--", "x"]).as_deref(),
            Some("prd")
        );
        assert_eq!(
            v(&["doppler", "run", "-c=stg", "--", "x"]).as_deref(),
            Some("stg")
        );
        assert_eq!(
            v(&["doppler", "run", "--config", "dev", "--", "x"]).as_deref(),
            Some("dev")
        );
        assert_eq!(
            v(&["doppler", "run", "--config=dev", "--", "x"]).as_deref(),
            Some("dev")
        );
    }

    #[test]
    fn config_flag_after_separator_is_ignored() {
        // `-c prod` belongs to the wrapped command, not doppler.
        let argv: Vec<String> = ["doppler", "run", "--", "tool", "-c", "prod"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(config_from_argv_env(&argv, &BTreeMap::new()), None);
    }

    #[test]
    fn config_from_env_when_no_flag() {
        let mut env = BTreeMap::new();
        env.insert("DOPPLER_CONFIG".to_string(), "prd".to_string());
        let argv: Vec<String> = ["doppler", "run", "--", "x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(config_from_argv_env(&argv, &env).as_deref(), Some("prd"));
    }

    #[test]
    fn non_doppler_launch_resolves_to_none() {
        let argv: Vec<String> = ["next", "dev", "-c", "prd"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(config_from_argv_env(&argv, &BTreeMap::new()), None);
    }

    #[test]
    fn guard_rejects_prd_and_unresolvable_doppler() {
        // explicit prd → reject
        assert!(
            assert_not_prd(
                "web",
                &["doppler", "run", "-c", "prd", "--", "x"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>(),
                &BTreeMap::new(),
                std::path::Path::new("/nonexistent-app-dir"),
            )
            .is_err()
        );
        // explicit safe config → ok
        assert!(
            assert_not_prd(
                "web",
                &["doppler", "run", "-c", "dev", "--", "x"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>(),
                &BTreeMap::new(),
                std::path::Path::new("/nonexistent-app-dir"),
            )
            .is_ok()
        );
        // non-doppler launch → ok (unguarded)
        assert!(
            assert_not_prd(
                "web",
                &["next", "dev"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>(),
                &BTreeMap::new(),
                std::path::Path::new("/nonexistent-app-dir"),
            )
            .is_ok()
        );
        // doppler launch with no flag/env, cwd has no scope → unresolvable → reject
        assert!(
            assert_not_prd(
                "web",
                &["doppler", "run", "--", "x"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>(),
                &BTreeMap::new(),
                std::path::Path::new("/nonexistent-app-dir"),
            )
            .is_err()
        );
    }

    #[test]
    fn user_override_wins() {
        let mut u = BTreeMap::new();
        u.insert("FOUNDRY_API_BASE_URL".into(), "http://x".into());
        let e = env_for(
            &app("lab-os", Some("FOUNDRY_API_BASE_URL")),
            Some("http://localhost:9103"),
            &u,
        );
        assert_eq!(e["FOUNDRY_API_BASE_URL"], "http://x");
    }

    fn test_app(launch: &[&str], static_env: &[(&str, &str)]) -> App {
        App {
            name: "api".into(),
            base_port: 9100,
            path: "apps/api".into(),
            launch: launch.iter().map(|s| s.to_string()).collect(),
            url: None,
            url_env: None,
            provides_url: false,
            static_env: static_env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            prep_files: vec![],
            setup: vec![],
        }
    }

    #[test]
    fn plan_group_renders_argv_and_static_env() {
        let mut catalog = HashMap::new();
        catalog.insert(
            "api".to_string(),
            test_app(
                &["nitro", "dev", "--port", "{{ port }}"],
                &[("PEER", "http://localhost:{{ ports['api-prod'] }}")],
            ),
        );
        let ports: BTreeMap<String, u16> =
            [("api".to_string(), 9100), ("api-prod".to_string(), 9101)].into();
        let plans = plan_group(
            &catalog,
            &["api".to_string()],
            &ports,
            None,
            Path::new("/wt"),
            Role::Issue,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(plans[0].argv, vec!["nitro", "dev", "--port", "9100"]);
        assert_eq!(plans[0].env["PEER"], "http://localhost:9101");
    }

    #[test]
    fn plan_group_rejects_leftover_brace_port() {
        let mut catalog = HashMap::new();
        catalog.insert(
            "api".to_string(),
            test_app(&["nitro", "--port", "{port}"], &[]),
        );
        let ports: BTreeMap<String, u16> = [("api".to_string(), 9100)].into();
        let err = plan_group(
            &catalog,
            &["api".to_string()],
            &ports,
            None,
            Path::new("/wt"),
            Role::Issue,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap_err();
        // {:#} prints the whole context chain; the hint lives in the root cause.
        assert!(format!("{err:#}").contains("retired"), "got: {err:#}");
    }

    #[test]
    fn classify_maps_signals_to_state() {
        // A pidless, unbound entry is crashed; a live pid without a bound port is
        // still starting; a bound port is ready regardless of pid.
        assert_eq!(classify(false, false), ServerState::Crashed);
        assert_eq!(classify(false, true), ServerState::Starting);
        assert_eq!(classify(true, false), ServerState::Ready);
        assert_eq!(classify(true, true), ServerState::Ready);
    }

    #[test]
    fn server_rows_filters_by_holder() {
        let mut data = Data::default();
        data.entries.insert(
            45123,
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

    #[test]
    fn plan_group_runs_launch_verbatim() {
        let mut catalog = HashMap::new();
        catalog.insert("web".to_string(), app("web", None));
        let mut ports = BTreeMap::new();
        ports.insert("web".to_string(), 4321u16);
        let plans = plan_group(
            &catalog,
            &["web".to_string()],
            &ports,
            None,
            std::path::Path::new("/root"),
            Role::Issue,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(plans.len(), 1);
        let p = &plans[0];
        assert_eq!(p.app, "web");
        assert_eq!(p.port, 4321);
        // No prefix is built: argv is the port-substituted launch, verbatim.
        assert_eq!(p.argv, vec!["next", "dev", "-p", "4321"]);
        assert!(p.cwd.ends_with("apps/x"));
    }

    /// Command prefix that runs a *real* python interpreter, or None (then the test
    /// skips). Mirrors the supervise helper: a direct `["python3"]`-style prefix, or
    /// a uv invocation when the only bare `python` is the Windows Store app-alias
    /// shim, which answers `--version` with "Python was not found …".
    fn python_cmd() -> Option<Vec<String>> {
        for cand in ["python3", "python", "py"] {
            let prefix = vec![cand.to_string()];
            if is_real_python(&prefix) {
                return Some(prefix);
            }
        }
        // uv fallbacks: `uv python find` yields a bare interpreter path (no wrapper
        // process); `uv run python` is the last resort.
        if let Some(path) = uv_python_path() {
            let prefix = vec![path];
            if is_real_python(&prefix) {
                return Some(prefix);
            }
        }
        let uv_run: Vec<String> = ["uv", "run", "python"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        if is_real_python(&uv_run) {
            return Some(uv_run);
        }
        None
    }

    /// Path of an interpreter `uv python find` resolves, if uv is installed and
    /// finds one. The caller validates the path before use.
    fn uv_python_path() -> Option<String> {
        use std::process::{Command, Stdio};
        let out = Command::new("uv")
            .args(["python", "find"])
            .stdin(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!path.is_empty()).then_some(path)
    }

    /// True when running `prefix --version` exits successfully and prints
    /// `Python <digit>…`. A real interpreter writes its version (stdout on 3.4+,
    /// stderr on older); the Store shim writes "Python was not found …", so
    /// requiring a digit right after "Python " rejects the shim despite it
    /// borrowing the "Python" prefix.
    fn is_real_python(prefix: &[String]) -> bool {
        use std::process::{Command, Stdio};
        let Some((prog, rest)) = prefix.split_first() else {
            return false;
        };
        let Ok(out) = Command::new(prog)
            .args(rest)
            .arg("--version")
            .stdin(Stdio::null())
            .output()
        else {
            return false;
        };
        if !out.status.success() {
            return false;
        }
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        combined
            .trim_start()
            .strip_prefix("Python ")
            .and_then(|rest| rest.trim_start().chars().next())
            .is_some_and(|c| c.is_ascii_digit())
    }

    #[test]
    fn bring_down_releases_a_pidless_reservation() {
        // A real holder dir: prune judges a reservation dead the moment its
        // holder path is gone (grace applies only after that check), so a
        // nonexistent holder lets a concurrent test's prune free this row
        // before bring_down runs.
        let holderdir = std::env::temp_dir().join(format!("down-test-{}", std::process::id()));
        std::fs::create_dir_all(&holderdir).unwrap();
        let holder = holderdir.to_str().unwrap().to_string();
        registry::alloc(&holder, &[("web".to_string(), 7000)], Role::Issue).unwrap();
        let out = bring_down(&holder, None).unwrap();
        assert_eq!(out.stopped, 0, "no pid recorded, nothing to stop");
        assert_eq!(out.freed.len(), 1, "the reservation is freed");
        // Idempotent: a second down frees nothing.
        let again = bring_down(&holder, None).unwrap();
        assert!(again.freed.is_empty());
        let _ = std::fs::remove_dir_all(&holderdir);
    }

    #[test]
    fn bring_down_ports_releases_listed_reservations() {
        // A real holder dir so a concurrent prune (which drops reservations whose
        // holder path is gone) can't steal the still-reserved second port out from
        // under this test before it asserts.
        let holderdir =
            std::env::temp_dir().join(format!("down-ports-test-{}", std::process::id()));
        std::fs::create_dir_all(&holderdir).unwrap();
        let holder = holderdir.to_str().unwrap().to_string();
        let got = registry::alloc(
            &holder,
            &[("api".to_string(), 7300), ("web".to_string(), 7400)],
            Role::Issue,
        )
        .unwrap();
        let ports: Vec<u16> = got.into_iter().map(|(_, p)| p).collect();

        // Down just the first port.
        let out = bring_down_ports(&[ports[0]]).unwrap();
        assert_eq!(out.stopped, 0, "pidless reservation, nothing to SIGTERM");
        assert_eq!(out.freed, vec![ports[0]]);

        // The second is still reserved; clean it up.
        let rest = registry::release(&holder, None).unwrap();
        assert_eq!(rest, vec![ports[1]]);
        let _ = std::fs::remove_dir_all(&holderdir);
    }

    #[test]
    fn read_log_tails_a_tracked_logfile() {
        // Use logdir as the holder so holder_alive returns true (snapshot prunes
        // entries whose holder path does not exist).
        let logdir = std::env::temp_dir().join(format!("devrun-log-{}", std::process::id()));
        std::fs::create_dir_all(&logdir).unwrap();
        let holder = logdir.to_str().unwrap().to_string();
        let logfile = logdir.join("issue-web.log");
        std::fs::write(&logfile, "line1\nline2\nline3\n").unwrap();

        // Track an entry pointing at the log, then read it back.
        registry::with_lock(|d| {
            d.entries.insert(
                7100,
                crate::registry::Entry {
                    app: "web".into(),
                    holder: holder.clone(),
                    role: Role::Issue,
                    pid: None,
                    logfile: Some(logfile.clone()),
                    ts: crate::registry::now(),
                },
            );
            Ok(())
        })
        .unwrap();

        let text = read_log(&holder, "web", None, 2).unwrap();
        assert_eq!(text, "line2\nline3");

        // Unknown app errors.
        assert!(read_log(&holder, "ghost", None, 10).is_err());

        let _ = registry::release(&holder, None);
        let _ = std::fs::remove_dir_all(&logdir);
    }

    #[test]
    fn launch_non_blocking_returns_before_readiness_then_status_flips() {
        let Some(py) = python_cmd() else {
            eprintln!("skipping launch_non_blocking: no launchable python");
            return;
        };
        // A free port for the test server.
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);

        let tmp = std::env::temp_dir().join(format!("devrun-run-{}.log", std::process::id()));
        let mut argv = py;
        // Inline accept-loop listener rather than `-m http.server`: http.server's
        // `server_bind` resolves the bound address with `socket.getfqdn()`, a
        // reverse-DNS lookup that stalls ~35s on hosts without reverse resolution
        // (macOS GitHub runners) — past this test's 10s readiness poll. The poll
        // only needs a TCP accept on IPv4 loopback.
        argv.extend([
            "-u".to_string(),
            "-c".to_string(),
            format!(
                "import socket\n\
                 s = socket.socket()\n\
                 s.bind((\"127.0.0.1\", {port}))\n\
                 s.listen(16)\n\
                 print(\"listening on\", {port}, flush=True)\n\
                 while True: s.accept()[0].close()\n"
            ),
        ]);
        let plan = LaunchPlan {
            app: "web".into(),
            port,
            argv,
            cwd: std::env::temp_dir(),
            env: BTreeMap::new(),
            log: tmp.clone(),
            url: format!("http://localhost:{port}"),
        };
        // Non-blocking: returns immediately; the just-spawned server is "starting".
        let out = launch(&[plan], "/w-launch-test", Role::Issue, false, false).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].port, port);
        assert!(out[0].pid.is_some());
        assert!(
            matches!(out[0].state, ServerState::Starting | ServerState::Ready),
            "freshly spawned server is starting (or already ready), got {:?}",
            out[0].state
        );

        // Poll (do not sleep-then-assert) until it accepts connections.
        let mut ready = false;
        for _ in 0..100 {
            if registry::listening(port) {
                ready = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(ready, "server never started listening");

        // Cleanup: stop the spawned pid and release the reservation.
        if let Some(pid) = out[0].pid {
            devkit_common::supervise::stop(pid);
        }
        let _ = registry::release("/w-launch-test", None);
        let _ = std::fs::remove_file(&tmp);
    }

    fn entry(app: &str, holder: &str, role: Role, pid: Option<u32>) -> registry::Entry {
        registry::Entry {
            app: app.into(),
            holder: holder.into(),
            role,
            pid,
            logfile: None,
            ts: registry::now(),
        }
    }

    fn plan(app: &str, port: u16) -> LaunchPlan {
        LaunchPlan {
            app: app.into(),
            port,
            argv: vec!["true".into()],
            cwd: PathBuf::from("."),
            env: BTreeMap::new(),
            log: PathBuf::from("x.log"),
            url: format!("http://localhost:{port}"),
        }
    }

    /// A dead pid: spawn a trivial child and wait for it, so the pid is reaped.
    fn dead_pid() -> u32 {
        let mut c = if cfg!(windows) {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", "exit"]);
            c
        } else {
            std::process::Command::new("true")
        };
        let mut child = c.spawn().expect("spawn trivial child");
        let pid = child.id();
        child.wait().expect("wait trivial child");
        pid
    }

    #[test]
    fn existing_server_reports_live_pid() {
        let mut data = Data::default();
        let me = std::process::id();
        data.entries
            .insert(49811, entry("api", "/wt", Role::Issue, Some(me)));
        let s = existing_server(&data, &plan("api", 49811), "/wt", Role::Issue)
            .expect("live pid on matching row must be reported");
        assert_eq!(s.pid, Some(me));
        assert_eq!(s.port, 49811);
    }

    #[test]
    fn existing_server_ignores_dead_pid() {
        let mut data = Data::default();
        data.entries
            .insert(49812, entry("api", "/wt", Role::Issue, Some(dead_pid())));
        assert!(existing_server(&data, &plan("api", 49812), "/wt", Role::Issue).is_none());
    }

    #[test]
    fn resolve_ports_includes_an_app_referenced_via_ports_template() {
        // A real holder dir: prune judges a reservation dead the moment its
        // holder path is gone, so a nonexistent holder would let a concurrent
        // test's prune free these rows before the assertions run.
        let holderdir =
            std::env::temp_dir().join(format!("resolve-ports-ok-{}", std::process::id()));
        std::fs::create_dir_all(&holderdir).unwrap();
        let holder = holderdir.to_str().unwrap().to_string();

        let mut catalog = HashMap::new();
        let mut primary = test_app(
            &["nitro", "dev", "--port", "{{ port }}"],
            &[(
                "PEER_URL",
                "http://localhost:{{ ports['resolve-ports-secondary'] }}",
            )],
        );
        primary.base_port = 48210;
        catalog.insert("resolve-ports-primary".to_string(), primary);
        let mut secondary = test_app(&["nitro", "dev", "--port", "{{ port }}"], &[]);
        secondary.base_port = 48310;
        catalog.insert("resolve-ports-secondary".to_string(), secondary);

        let ports = resolve_ports(
            &catalog,
            &["resolve-ports-primary".to_string()],
            &holder,
            Role::Issue,
            &BTreeMap::new(),
        )
        .expect("selected app plus its referenced app resolve");

        assert_eq!(
            ports.keys().cloned().collect::<Vec<_>>(),
            vec![
                "resolve-ports-primary".to_string(),
                "resolve-ports-secondary".to_string()
            ],
            "both the selected app and the app it references via ports[...] must be allocated"
        );

        let _ = registry::release(&holder, None);
        let _ = std::fs::remove_dir_all(&holderdir);
    }

    #[test]
    fn resolve_ports_rejects_a_reference_to_an_unknown_app() {
        let mut catalog = HashMap::new();
        let mut selected = test_app(
            &["nitro", "dev", "--port", "{{ port }}"],
            &[(
                "PEER_URL",
                "http://localhost:{{ ports['resolve-ports-ghost'] }}",
            )],
        );
        selected.base_port = 48410;
        catalog.insert("resolve-ports-selected".to_string(), selected);

        let err = resolve_ports(
            &catalog,
            &["resolve-ports-selected".to_string()],
            "/wt",
            Role::Issue,
            &BTreeMap::new(),
        )
        .expect_err("a ports[...] reference to an app absent from the catalog must error");

        assert!(
            format!("{err:#}").contains("resolve-ports-ghost"),
            "error must name the unknown app: {err:#}"
        );
    }

    #[test]
    fn existing_server_ignores_pidless_reservation_and_foreign_rows() {
        let mut data = Data::default();
        let me = std::process::id();
        data.entries
            .insert(49813, entry("api", "/wt", Role::Issue, None));
        data.entries
            .insert(49814, entry("web", "/wt", Role::Issue, Some(me)));
        data.entries
            .insert(49815, entry("api", "/other", Role::Issue, Some(me)));
        data.entries
            .insert(49816, entry("api", "/wt", Role::Baseline, Some(me)));
        assert!(existing_server(&data, &plan("api", 49813), "/wt", Role::Issue).is_none());
        assert!(existing_server(&data, &plan("api", 49814), "/wt", Role::Issue).is_none());
        assert!(existing_server(&data, &plan("api", 49815), "/wt", Role::Issue).is_none());
        assert!(existing_server(&data, &plan("api", 49816), "/wt", Role::Issue).is_none());
    }
}
