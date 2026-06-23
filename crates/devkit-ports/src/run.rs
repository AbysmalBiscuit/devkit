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

/// Refuse a launch that would run Doppler against the `prd` config. A launch
/// whose program is not `doppler` is unguarded. For a Doppler launch the config
/// is resolved in Doppler's own precedence order — explicit flag, then
/// `DOPPLER_CONFIG`, then the local scope — and a launch whose config resolves to
/// `prd`, or cannot be resolved at all, is rejected (fail-safe).
pub fn assert_not_prd(plan: &LaunchPlan) -> Result<()> {
    let prog = plan.argv.first().map(String::as_str).unwrap_or_default();
    if Path::new(prog).file_name().and_then(|s| s.to_str()) != Some("doppler") {
        return Ok(());
    }
    let config =
        config_from_argv_env(&plan.argv, &plan.env).or_else(|| doppler_scoped_config(&plan.cwd));
    match config.as_deref() {
        Some("prd") => anyhow::bail!(
            "refusing to launch `{}`: doppler config resolves to `prd` (production secrets)",
            plan.app
        ),
        Some(_) => Ok(()),
        None => anyhow::bail!(
            "refusing to launch `{}`: cannot determine its doppler config (no -c/--config, \
             no DOPPLER_CONFIG, no local scope). Add an explicit `-c <config>` to its launch.",
            plan.app
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
) -> Vec<LaunchPlan> {
    let provider_port = provider.and_then(|p| ports.get(p).copied());
    let mut plans = Vec::with_capacity(apps.len());
    for a in apps {
        let app = &catalog[a];
        let port = ports[a];
        let argv = launch_argv(app, port);
        let cwd = base_dir.join(&app.path);
        let env = env_for(app, provider_port, user_env);
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
        });
    }
    plans
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

/// Spawn (or hand to the daemon) every plan in one group and record each pid.
/// `wait = true` blocks up to 120 s per port for readiness (the CLI path);
/// `wait = false` returns immediately with each server in its current state.
pub fn launch(
    plans: &[LaunchPlan],
    holder: &str,
    role: Role,
    supervise_daemon: bool,
    wait: bool,
) -> Result<Vec<ServerStatus>> {
    for p in plans {
        assert_not_prd(p)?;
    }
    #[cfg(feature = "daemon")]
    if supervise_daemon {
        return supervise_via_daemon(plans, holder, role);
    }
    #[cfg(not(feature = "daemon"))]
    let _ = supervise_daemon;

    let mut spawned = Vec::with_capacity(plans.len());
    for p in plans {
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
        Ok(spawned
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
            .collect())
    } else {
        Ok(spawned
            .into_iter()
            .map(|(a, port, log, pid)| ServerStatus {
                app: a,
                role,
                port,
                pid: Some(pid),
                logfile: Some(log),
                state: server_state(port, Some(pid)),
            })
            .collect())
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
            launch: vec!["next".into(), "dev".into(), "-p".into(), "{port}".into()],
            url_env: url_env.map(Into::into),
            provides_url: false,
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
        let plan = |argv: &[&str], env: BTreeMap<String, String>| LaunchPlan {
            app: "web".into(),
            port: 1,
            argv: argv.iter().map(|s| s.to_string()).collect(),
            cwd: std::path::PathBuf::from("/nonexistent-app-dir"),
            env,
            log: std::path::PathBuf::from("/dev/null"),
        };
        // explicit prd → reject
        assert!(
            assert_not_prd(&plan(
                &["doppler", "run", "-c", "prd", "--", "x"],
                BTreeMap::new()
            ))
            .is_err()
        );
        // explicit safe config → ok
        assert!(
            assert_not_prd(&plan(
                &["doppler", "run", "-c", "dev", "--", "x"],
                BTreeMap::new()
            ))
            .is_ok()
        );
        // non-doppler launch → ok (unguarded)
        assert!(assert_not_prd(&plan(&["next", "dev"], BTreeMap::new())).is_ok());
        // doppler launch with no flag/env, cwd has no scope → unresolvable → reject
        assert!(assert_not_prd(&plan(&["doppler", "run", "--", "x"], BTreeMap::new())).is_err());
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
        );
        assert_eq!(plans.len(), 1);
        let p = &plans[0];
        assert_eq!(p.app, "web");
        assert_eq!(p.port, 4321);
        // No prefix is built: argv is the port-substituted launch, verbatim.
        assert_eq!(p.argv, vec!["next", "dev", "-p", "4321"]);
        assert!(p.cwd.ends_with("apps/x"));
    }

    /// First launchable python interpreter, or None (then the test skips). Mirrors
    /// the supervise test so CI hosts without a real python3 don't fail.
    fn python_cmd() -> Option<&'static str> {
        use std::process::{Command, Stdio};
        ["python3", "python", "py"].into_iter().find(|c| {
            Command::new(c)
                .arg("--version")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok()
        })
    }

    #[test]
    fn bring_down_releases_a_pidless_reservation() {
        let holder = format!("/down-test-{}", std::process::id());
        registry::alloc(&holder, &[("web".to_string(), 7000)], Role::Issue).unwrap();
        let out = bring_down(&holder, None).unwrap();
        assert_eq!(out.stopped, 0, "no pid recorded, nothing to stop");
        assert_eq!(out.freed.len(), 1, "the reservation is freed");
        // Idempotent: a second down frees nothing.
        let again = bring_down(&holder, None).unwrap();
        assert!(again.freed.is_empty());
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
        let plan = LaunchPlan {
            app: "web".into(),
            port,
            argv: [py, "-m", "http.server", &port.to_string()]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            cwd: std::env::temp_dir(),
            env: BTreeMap::new(),
            log: tmp.clone(),
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
}
