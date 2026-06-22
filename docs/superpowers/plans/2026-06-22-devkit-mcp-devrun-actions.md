# devkit MCP `devrun` Actions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add four `devrun` actions (`status`, `up`, `down`, `logs`) to the devkit MCP server so an agent can inspect, start (non-blocking), stop, and read logs of dev servers for a worktree.

**Architecture:** Extract `devrun`'s server-lifecycle logic out of the `src/bin/devrun` binary into a new `devkit-ports::run` library module that both the `devrun` CLI and the MCP handlers call — no shelling. The MCP `devrun.up` is non-blocking (kick-and-poll): it spawns and returns immediately with `state: "starting"`, and the agent polls `devrun.status` for readiness.

**Tech Stack:** Rust (edition 2024), sync (no tokio/async/rmcp), `anyhow`, `serde`/`serde_json`. No new dependencies.

## Global Constraints

- **No new dependencies.** Only `anyhow`, `serde`, `serde_json`, and the existing devkit crates.
- **Sync only.** No tokio, no async, no `rmcp`. Blocking facades called directly.
- **No shelling from MCP handlers.** Handlers call `devkit-ports` library functions, never spawn the `devrun` binary.
- **`anyhow` everywhere** with `.context()`; binaries install `report::install_panic_hook`.
- **Action naming is `binary.action`** (`devrun.status`, `devrun.up`, `devrun.down`, `devrun.logs`).
- **Targeting:** `root` is an explicit per-call argument; no CWD inference. Ports holder = `root`.
- **`up` is `issue`-role only and non-blocking** — never calls `wait_ready`; returns `state: "starting"`.
- **`up` uses daemon supervision only if a daemon is already running** (`daemon_running()`), else direct detached spawn. It never auto-starts `devkitd`.
- **TDD:** failing test first; `cargo test --workspace` is the merge gate; `cargo clippy --workspace --all-targets -- -D warnings` must stay clean; `cargo fmt --all`.
- **Tests poll for expected state, never sleep a fixed interval** (CI Windows runners spawn/reap late).
- **Commits:** Conventional Commits; end each commit body with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/devkit-ports/src/run.rs` | devrun server-lifecycle facade: env building, launch plans, spawn/supervise, status rows, down, log read | **Create** |
| `crates/devkit-ports/src/lib.rs` | add `pub mod run;` | Modify |
| `src/bin/devrun/env.rs` | env helpers (moved into `run.rs`) | **Delete** |
| `src/bin/devrun/main.rs` | CLI; calls `run::*` instead of inline logic | Modify |
| `crates/devkit-mcp/src/devrun.rs` | the four MCP `devrun.*` action handlers | **Create** |
| `crates/devkit-mcp/src/actions.rs` | register `devrun` actions | Modify |
| `crates/devkit-mcp/src/lib.rs` | add `mod devrun;` | Modify |
| `tests/mcp.rs` | MCP integration tests for `devrun.*` | Modify |
| `README.md`, `AGENTS.md`, `docs/next-steps.md` | docs | Modify |

The extracted `run.rs` is the seam: it depends on `registry`, `load`, `apps` (same crate) and `devkit_common::{supervise, paths}` (a dependency of `devkit-ports`). The MCP crate already depends on `devkit-ports`, so handlers reach `run::*` directly.

---

## Task 1: `devkit-ports::run` foundation — env helpers + status rows

Move the env-building helpers from the `devrun` binary into the new library module, and add the structured `ServerStatus`/`server_rows` the MCP `status` action will return. No behavior change to the CLI.

**Files:**
- Create: `crates/devkit-ports/src/run.rs`
- Modify: `crates/devkit-ports/src/lib.rs:4` (add `pub mod run;`)
- Modify: `src/bin/devrun/main.rs:2` (drop `mod env;`), `:330-333` (use `run::`)
- Delete: `src/bin/devrun/env.rs`

**Interfaces:**
- Produces:
  - `run::doppler_prefix(app: &App, config: &str) -> Vec<String>`
  - `run::launch_argv(app: &App, port: u16) -> Vec<String>`
  - `run::env_for(app: &App, provider_port: Option<u16>, user: &BTreeMap<String,String>) -> BTreeMap<String,String>`
  - `run::ServerState` = `{ Ready, Starting, Crashed }` (serde lowercase)
  - `run::ServerStatus { app: String, role: Role, port: u16, pid: Option<u32>, logfile: Option<PathBuf>, state: ServerState }` (Serialize)
  - `run::server_rows(data: &Data, only_holder: Option<&str>) -> Vec<ServerStatus>`

- [ ] **Step 1: Create `run.rs` with moved env helpers + status rows**

Create `crates/devkit-ports/src/run.rs`:

```rust
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
        assert_eq!(rows[0].state, ServerState::Ready, "bound port reads as ready");
        drop(l);
    }
}
```

- [ ] **Step 2: Register the module and run the new tests to verify they fail then pass**

Add to `crates/devkit-ports/src/lib.rs` after line 4 (`pub mod registry;`):

```rust
pub mod run;
```

Run: `cargo test -p devkit-ports run::tests`
Expected: PASS (6 tests). If `run.rs` were absent the module wouldn't compile — this confirms the move + new rows compile and behave.

- [ ] **Step 3: Re-point `devrun` at the moved helpers and delete `env.rs`**

In `src/bin/devrun/main.rs`, remove the module declaration line `mod env;` (line 2). The remaining `mod baseline;` stays.

In `src/bin/devrun/main.rs`, the three call sites inside `cmd_up` currently read:

```rust
            let mut argv = env::doppler_prefix(app, &cfg.defaults.doppler_config);
            argv.extend(env::launch_argv(app, port));
```
and
```rust
            let envmap = env::env_for(app, provider_port, &user);
```

Replace `env::doppler_prefix`, `env::launch_argv`, and `env::env_for` with `run::doppler_prefix`, `run::launch_argv`, `run::env_for`. Add `run` to the existing import on line 10:

```rust
use devkit_ports::registry::{self, Role};
use devkit_ports::run;
```

Delete the file `src/bin/devrun/env.rs`.

- [ ] **Step 4: Verify the full gate is green**

Run: `cargo test --workspace`
Expected: PASS (all suites; the env helper tests now run under `devkit-ports` instead of the `devrun` binary).

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

Run: `cargo fmt --all`

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/run.rs crates/devkit-ports/src/lib.rs src/bin/devrun/main.rs
git rm src/bin/devrun/env.rs
git commit -m "$(cat <<'EOF'
refactor(ports): move devrun env helpers into a run module

Hoist doppler_prefix/launch_argv/env_for out of the devrun binary into a
new devkit-ports::run facade, and add ServerStatus/server_rows so the MCP
devrun actions can return structured per-server state. No CLI behavior
change.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Extract the launch path into `run`; refactor `cmd_up`

Move the per-group spawn/supervise/wait logic out of `cmd_up` into reusable `run` functions with a `wait` flag, so the MCP `up` can launch without blocking on readiness. The CLI keeps blocking (`wait=true`) and its behavior is unchanged.

**Files:**
- Modify: `crates/devkit-ports/src/run.rs` (add launch API)
- Modify: `src/bin/devrun/main.rs:230-446` (`cmd_up` calls `run::*`)

**Interfaces:**
- Consumes: `run::{doppler_prefix, launch_argv, env_for}` (Task 1)
- Produces:
  - `run::holder_slug(holder: &str) -> String`
  - `run::ensure_provider(catalog: &HashMap<String,App>, apps: &mut Vec<String>)`
  - `run::LaunchPlan { pub app: String, pub port: u16, pub argv: Vec<String>, pub cwd: PathBuf, pub env: BTreeMap<String,String>, pub log: PathBuf }`
  - `run::plan_group(catalog, doppler_config, apps, ports, provider, base_dir, role, user_env) -> Vec<LaunchPlan>`
  - `run::launch(plans: &[LaunchPlan], holder: &str, role: Role, supervise: bool, wait: bool) -> Result<Vec<ServerStatus>>`
  - `run::daemon_running() -> bool`

- [ ] **Step 1: Write a failing unit test for `plan_group` and non-blocking `launch`**

Append to the `tests` module in `crates/devkit-ports/src/run.rs`:

```rust
    #[test]
    fn plan_group_builds_doppler_wrapped_argv() {
        let mut catalog = HashMap::new();
        catalog.insert("web".to_string(), app("web", None));
        let mut ports = BTreeMap::new();
        ports.insert("web".to_string(), 4321u16);
        let plans = plan_group(
            &catalog,
            "dev_local",
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
        // doppler prefix then the port-substituted launch argv.
        assert_eq!(p.argv[0], "doppler");
        assert_eq!(p.argv.last().unwrap(), "4321");
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
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p devkit-ports run::tests::plan_group_builds_doppler_wrapped_argv`
Expected: FAIL — `plan_group`, `LaunchPlan`, and `launch` do not exist yet (compile error).

- [ ] **Step 3: Implement the launch API in `run.rs`**

Add these imports at the top of `crates/devkit-ports/src/run.rs` (extend the existing `use` block):

```rust
use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

use devkit_common::{paths, supervise};
```

Add the implementation (after `server_rows`):

```rust
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
pub fn plan_group(
    catalog: &HashMap<String, App>,
    doppler_config: &str,
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
        let mut argv = doppler_prefix(app, doppler_config);
        argv.extend(launch_argv(app, port));
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
/// `wait = false` returns immediately with each server `starting` (the MCP path).
pub fn launch(
    plans: &[LaunchPlan],
    holder: &str,
    role: Role,
    supervise_daemon: bool,
    wait: bool,
) -> Result<Vec<ServerStatus>> {
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
    let mut client = crate::daemon::client::ensure_running().context("starting supervisor daemon")?;
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
```

Note: `plan_group` derives the log directory from `base_dir`'s leaf (it equals the holder for each group: `issue_holder` for issue, the baseline path for baseline). The CLI previously namespaced by `slug(holder)`; since `holder == base_dir` for both groups the directory is unchanged. The filename keeps its role prefix via `role.as_str()` (`issue-<app>.log` / `baseline-<app>.log`), so the CLI's baseline-role log paths are unchanged; the issue-only MCP `up` passes `Role::Issue`.

- [ ] **Step 4: Run the unit tests to verify they pass**

Run: `cargo test -p devkit-ports run::tests`
Expected: PASS (8 tests; `launch_non_blocking…` skips with a message if no python).

- [ ] **Step 5: Refactor `cmd_up` to call `run`, then verify the workspace gate**

In `src/bin/devrun/main.rs`, delete the local `slug` helper (lines 105-111) and the provider-resolution + spawn/wait blocks inside `cmd_up`. Replace the body of the `for (grp_role, holder, base_dir) in &groups` loop (the current lines 315-441) and the preceding provider block (lines 269-283) so the loop reads:

```rust
    run::ensure_provider(catalog, &mut apps);

    let user = parse_user_env(env_pairs, env_file)?;
    let issue_holder = toplevel(cwd)?;

    // (role, holder, base_dir) — base_dir is where <app.path> is rooted.
    let groups: Vec<(Role, String, PathBuf)> = {
        let baseline_path = expand_tilde(&cfg.defaults.baseline_path);
        let mut g = Vec::new();
        for r in role.roles() {
            match r {
                Role::Issue => {
                    g.push((
                        Role::Issue,
                        issue_holder.clone(),
                        PathBuf::from(&issue_holder),
                    ));
                }
                Role::Baseline => {
                    let bp = baseline_path
                        .to_str()
                        .context("baseline_path not UTF-8")?
                        .to_string();
                    baseline::ensure_fresh(&issue_holder, &bp, &cfg.defaults.baseline_ref)?;
                    g.push((Role::Baseline, bp.clone(), baseline_path.clone()));
                }
            }
        }
        g
    };

    let provider = catalog
        .iter()
        .find(|(_, a)| a.provides_url)
        .map(|(n, _)| n.clone());

    let mut rows: Vec<Row> = Vec::new();
    for (grp_role, holder, base_dir) in &groups {
        let reqs: Vec<(String, u16)> = apps
            .iter()
            .map(|a| (a.clone(), catalog[a].base_port))
            .collect();
        let ports: BTreeMap<String, u16> = registry::alloc(holder, &reqs, *grp_role)?
            .into_iter()
            .collect();
        let plans = run::plan_group(
            catalog,
            &cfg.defaults.doppler_config,
            &apps,
            &ports,
            provider.as_deref(),
            base_dir,
            *grp_role,
            &user,
        );

        if dry_run {
            for p in &plans {
                println!("[{}] {} :{}", grp_role.as_str(), p.app, p.port);
                println!("  cwd:  {}", p.cwd.display());
                println!("  argv: {}", p.argv.join(" "));
                let envs: Vec<String> = p.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
                println!("  env:  {}", envs.join(" "));
                println!("  log:  {}", p.log.display());
                rows.push(Row {
                    role: *grp_role,
                    app: p.app.clone(),
                    port: p.port,
                    pid: None,
                    log: p.log.clone(),
                    ready: None,
                });
            }
            continue;
        }

        let statuses = run::launch(&plans, holder, *grp_role, supervise, true)?;
        for s in statuses {
            if s.state != devkit_ports::run::ServerState::Ready {
                if let Some(log) = &s.logfile {
                    eprintln!(
                        "--- {} ({}) did not become ready; last 30 log lines: ---",
                        s.app,
                        grp_role.as_str()
                    );
                    eprintln!("{}", supervise::tail(log, 30));
                }
            }
            rows.push(Row {
                role: s.role,
                app: s.app,
                port: s.port,
                pid: s.pid,
                log: s.logfile.unwrap_or_default(),
                ready: Some(s.state == devkit_ports::run::ServerState::Ready),
            });
        }
    }

    print_summary(&rows);
    Ok(())
```

Then remove the now-unused `cfg(feature = "daemon")` supervise block and the direct-spawn block that previously lived in the loop (they are replaced by `run::launch`). The `supervise` flag is now passed straight to `run::launch`; keep the `#[cfg(not(feature = "daemon"))] let _ = supervise;` line near the top of `cmd_up` so the unused-variable lint stays satisfied when the daemon feature is off. Update imports: `use devkit_common::supervise;` is still needed (for `supervise::tail`); `paths` may no longer be used directly in `main.rs` — if clippy flags it, drop it from the `use devkit_common::{cmd::git, paths, ui};` line.

Run: `cargo test --workspace`
Expected: PASS (all suites; `devrun` behavior unchanged — dry-run output and the spawn path are equivalent).

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

Run: `cargo fmt --all`

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-ports/src/run.rs src/bin/devrun/main.rs
git commit -m "$(cat <<'EOF'
refactor(ports): extract devrun launch into run::launch

Move the per-group plan/spawn/supervise/wait logic out of cmd_up into
run::{plan_group,launch,ensure_provider}, with a wait flag so a caller can
spawn without blocking on readiness. cmd_up now calls them and keeps its
blocking behavior; this is the seam the non-blocking MCP up will use.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Extract `bring_down` + `read_log`; refactor `cmd_down`/`cmd_logs`

**Files:**
- Modify: `crates/devkit-ports/src/run.rs` (add `bring_down`, `read_log`, `DownOutcome`)
- Modify: `src/bin/devrun/main.rs` (`cmd_down`, `cmd_logs` call `run::*`)

**Interfaces:**
- Produces:
  - `run::DownOutcome { pub stopped: usize, pub freed: Vec<u16>, pub via_daemon: bool }` (Serialize)
  - `run::bring_down(holder: &str, role: Option<Role>) -> Result<DownOutcome>`
  - `run::read_log(holder: &str, app: &str, role: Option<Role>, lines: usize) -> Result<String>`

- [ ] **Step 1: Write failing unit tests for `bring_down` and `read_log`**

Append to the `tests` module in `crates/devkit-ports/src/run.rs`:

```rust
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
        let holder = format!("/log-test-{}", std::process::id());
        let logdir = std::env::temp_dir().join(format!("devrun-log-{}", std::process::id()));
        std::fs::create_dir_all(&logdir).unwrap();
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-ports run::tests::bring_down_releases_a_pidless_reservation`
Expected: FAIL — `bring_down`/`read_log`/`DownOutcome` do not exist (compile error).

- [ ] **Step 3: Implement `bring_down` and `read_log`**

Add to `crates/devkit-ports/src/run.rs`:

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p devkit-ports run::tests`
Expected: PASS (10 tests).

- [ ] **Step 5: Refactor `cmd_down` and `cmd_logs`, then verify the gate**

In `src/bin/devrun/main.rs`, replace the body of `cmd_down` (lines 448-478) with:

```rust
fn cmd_down(cwd: &str, role: Option<Role>) -> Result<()> {
    let holder = toplevel(cwd)?;
    let out = run::bring_down(&holder, role)?;
    if out.via_daemon {
        println!("stopped via daemon; released ports {:?}", out.freed);
    } else {
        println!(
            "stopped {} process(es); released ports {:?}",
            out.stopped, out.freed
        );
    }
    Ok(())
}
```

Replace the non-follow branch of `cmd_logs` (the final `println!("{}", supervise::tail(&log, 200));` and the `let data`/`let log` lookup) so `cmd_logs` reads:

```rust
fn cmd_logs(cwd: &str, app: &str, role: Option<Role>, follow: bool) -> Result<()> {
    let holder = toplevel(cwd)?;
    if follow {
        let data = registry::snapshot()?;
        let log = data
            .entries
            .values()
            .find(|e| e.holder == holder && e.app == app && role.is_none_or(|r| e.role == r))
            .and_then(|e| e.logfile.clone())
            .ok_or_else(|| anyhow::anyhow!("no tracked log for app `{app}` in this worktree"))?;
        let status = std::process::Command::new("tail")
            .arg("-f")
            .arg(&log)
            .status()
            .with_context(|| "running `tail -f`")?;
        std::process::exit(status.code().unwrap_or(1));
    }
    println!("{}", run::read_log(&holder, app, role, 200)?);
    Ok(())
}
```

Run: `cargo test --workspace`
Expected: PASS.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

Run: `cargo fmt --all`

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-ports/src/run.rs src/bin/devrun/main.rs
git commit -m "$(cat <<'EOF'
refactor(ports): extract devrun down/logs into run facade

Add run::bring_down (daemon-or-direct stop+release) and run::read_log
(tail a tracked logfile); cmd_down/cmd_logs now call them. This is the
shared path the MCP devrun.down/devrun.logs handlers reuse.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: MCP `devrun` module + `devrun.status` action

**Files:**
- Create: `crates/devkit-mcp/src/devrun.rs`
- Modify: `crates/devkit-mcp/src/lib.rs` (add `mod devrun;`)
- Modify: `crates/devkit-mcp/src/actions.rs:17-22` (register devrun actions)
- Modify: `tests/mcp.rs` (status integration test)

**Interfaces:**
- Consumes: `run::server_rows`, `registry::snapshot`, `crate::actions::Action`, `crate::ServerCtx`
- Produces: `crate::devrun::actions() -> Vec<Action>` exposing `devrun.status`

- [ ] **Step 1: Write a failing integration test for `devrun.status`**

Add to `tests/mcp.rs`:

```rust
#[test]
fn devrun_status_lists_tracked_servers_for_root() {
    let proj = project_with_config();
    let state = scratch("state");
    let root = proj.to_str().unwrap();
    let resps = mcp(
        &proj,
        &state,
        &[
            // Reserve a port so there is something to report.
            call_req(1, "ports.alloc", json!({ "root": root, "apps": ["web"] })),
            call_req(2, "devrun.status", json!({ "root": root })),
            call_req(3, "devrun.status", json!({ "all": true })),
        ],
    );
    tool_json(&resps[0], false);

    let rows = tool_json(&resps[1], false);
    let arr = rows.as_array().expect("status returns an array");
    assert_eq!(arr.len(), 1, "one tracked server for this root");
    assert_eq!(arr[0]["app"], "web");
    // Nothing is listening, no pid → crashed.
    assert_eq!(arr[0]["state"], "crashed");

    let all = tool_json(&resps[2], false);
    assert!(!all.as_array().unwrap().is_empty(), "all view is non-empty");
}

#[test]
fn devrun_status_without_root_or_all_is_an_error() {
    let proj = project();
    let state = scratch("state");
    let resps = mcp(&proj, &state, &[call_req(1, "devrun.status", json!({}))]);
    let payload = tool_json(&resps[0], true);
    assert!(payload.as_str().unwrap().contains("root"));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --test mcp devrun_status`
Expected: FAIL — `devrun.status` is an unknown action (`isError` with "unknown action").

- [ ] **Step 3: Create the devrun MCP module with the `status` handler**

Create `crates/devkit-mcp/src/devrun.rs`:

```rust
use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde_json::Value;

use devkit_ports::registry;
use devkit_ports::run;

use crate::ServerCtx;
use crate::actions::Action;

pub fn actions() -> Vec<Action> {
    vec![Action {
        name: "devrun.status",
        summary: "Show tracked dev servers for a worktree (or all worktrees).",
        schema: status_schema,
        handler: status,
    }]
}

#[derive(Deserialize)]
struct StatusArgs {
    #[serde(default)]
    root: Option<String>,
    #[serde(default)]
    all: bool,
}

fn status_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Absolute path to the worktree to scope to (required unless all=true)." },
            "all": { "type": "boolean", "description": "Show servers across every worktree (default false)." }
        },
        "additionalProperties": false
    })
}

fn status(_ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: StatusArgs = serde_json::from_value(args).context("invalid devrun.status arguments")?;
    let data = registry::snapshot()?;
    let rows = if a.all {
        run::server_rows(&data, None)
    } else {
        let root = a
            .root
            .ok_or_else(|| anyhow!("devrun.status requires `root` unless `all` is set"))?;
        run::server_rows(&data, Some(&root))
    };
    Ok(serde_json::to_value(rows)?)
}
```

Add `mod devrun;` to `crates/devkit-mcp/src/lib.rs` alongside the other module declarations (after `mod actions;`):

```rust
mod actions;
mod devrun;
mod jsonrpc;
mod locks;
mod ports;
```

Register the actions in `crates/devkit-mcp/src/actions.rs` — change `actions()`:

```rust
pub fn actions() -> Vec<Action> {
    let mut v = Vec::new();
    v.extend(crate::ports::actions());
    v.extend(crate::locks::actions());
    v.extend(crate::devrun::actions());
    v
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test mcp devrun_status`
Expected: PASS (2 tests).

Run: `cargo test -p devkit-mcp`
Expected: PASS (the `describe_returns_a_schema_for_each_action` unit test now also covers `devrun.status`).

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-mcp/src/devrun.rs crates/devkit-mcp/src/lib.rs crates/devkit-mcp/src/actions.rs tests/mcp.rs
git commit -m "$(cat <<'EOF'
feat(mcp): add devrun.status action

Register a devrun action module and the first action, devrun.status,
returning structured per-server rows (app, role, port, pid, state) for a
worktree or, with all=true, across worktrees.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: `devrun.logs` and `devrun.down` actions

**Files:**
- Modify: `crates/devkit-mcp/src/devrun.rs` (add `logs`, `down`)
- Modify: `tests/mcp.rs` (logs error-path + down integration tests)

**Interfaces:**
- Consumes: `run::read_log`, `run::bring_down`
- Produces: `devrun.logs`, `devrun.down` actions

- [ ] **Step 1: Write failing integration tests**

Add to `tests/mcp.rs`:

```rust
#[test]
fn devrun_logs_unknown_app_is_an_error() {
    let proj = project();
    let state = scratch("state");
    let root = proj.to_str().unwrap();
    let resps = mcp(
        &proj,
        &state,
        &[call_req(1, "devrun.logs", json!({ "root": root, "app": "ghost" }))],
    );
    let payload = tool_json(&resps[0], true);
    assert!(payload.as_str().unwrap().contains("ghost"));
}

#[test]
fn devrun_down_releases_reserved_ports() {
    let proj = project_with_config();
    let state = scratch("state");
    let root = proj.to_str().unwrap();
    let resps = mcp(
        &proj,
        &state,
        &[
            call_req(1, "ports.alloc", json!({ "root": root, "apps": ["web"] })),
            call_req(2, "devrun.down", json!({ "root": root })),
            call_req(3, "devrun.status", json!({ "root": root })),
        ],
    );
    tool_json(&resps[0], false);

    let down = tool_json(&resps[1], false);
    assert_eq!(down["freed"].as_array().unwrap().len(), 1);
    assert_eq!(down["stopped"], 0, "no pid was recorded");

    let rows = tool_json(&resps[2], false);
    assert!(
        rows.as_array().unwrap().is_empty(),
        "nothing tracked after down"
    );
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --test mcp devrun_logs_unknown_app_is_an_error devrun_down_releases_reserved_ports`
Expected: FAIL — `devrun.logs`/`devrun.down` are unknown actions.

- [ ] **Step 3: Implement `logs` and `down`**

In `crates/devkit-mcp/src/devrun.rs`, extend `actions()`:

```rust
pub fn actions() -> Vec<Action> {
    vec![
        Action {
            name: "devrun.status",
            summary: "Show tracked dev servers for a worktree (or all worktrees).",
            schema: status_schema,
            handler: status,
        },
        Action {
            name: "devrun.down",
            summary: "Stop a worktree's dev servers and release their ports.",
            schema: down_schema,
            handler: down,
        },
        Action {
            name: "devrun.logs",
            summary: "Read the last lines of a tracked app's log for a worktree.",
            schema: logs_schema,
            handler: logs,
        },
    ]
}
```

Add the handlers (and `use devkit_ports::registry::Role;` to the imports):

```rust
#[derive(Deserialize)]
struct DownArgs {
    root: String,
    #[serde(default)]
    role: Option<Role>,
}

fn down_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Absolute path to the worktree (the ports holder)." },
            "role": { "type": "string", "enum": ["issue", "baseline"], "description": "Only stop this role (default: all roles)." }
        },
        "required": ["root"],
        "additionalProperties": false
    })
}

fn down(_ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: DownArgs = serde_json::from_value(args).context("invalid devrun.down arguments")?;
    let out = run::bring_down(&a.root, a.role)?;
    Ok(serde_json::to_value(out)?)
}

#[derive(Deserialize)]
struct LogsArgs {
    root: String,
    app: String,
    #[serde(default)]
    role: Option<Role>,
    #[serde(default)]
    lines: Option<usize>,
}

fn logs_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Absolute path to the worktree." },
            "app": { "type": "string", "description": "App name whose log to read." },
            "role": { "type": "string", "enum": ["issue", "baseline"], "description": "Role to disambiguate (default: any)." },
            "lines": { "type": "integer", "minimum": 1, "description": "Tail length (default 200)." }
        },
        "required": ["root", "app"],
        "additionalProperties": false
    })
}

fn logs(_ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: LogsArgs = serde_json::from_value(args).context("invalid devrun.logs arguments")?;
    let text = run::read_log(&a.root, &a.app, a.role, a.lines.unwrap_or(200))?;
    Ok(serde_json::json!({ "log": text }))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test mcp devrun_`
Expected: PASS (status + logs + down tests).

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-mcp/src/devrun.rs tests/mcp.rs
git commit -m "$(cat <<'EOF'
feat(mcp): add devrun.down and devrun.logs actions

devrun.down stops a worktree's servers and releases their ports (returning
stopped/freed/via_daemon); devrun.logs tails a tracked app's log. Both
reuse the run facade so the CLI and MCP share one code path.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: `devrun.up` action (non-blocking)

**Files:**
- Modify: `crates/devkit-mcp/src/devrun.rs` (add `up`)
- Modify: `tests/mcp.rs` (up error-path + allocation test)

**Interfaces:**
- Consumes: `devkit_ports::load::load`, `run::{ensure_provider, plan_group, launch, daemon_running}`, `registry::alloc`, `Role`
- Produces: `devrun.up` action

- [ ] **Step 1: Write failing integration tests**

Add to `tests/mcp.rs`:

```rust
#[test]
fn devrun_up_unknown_app_is_an_error() {
    let proj = project_with_config();
    let state = scratch("state");
    let root = proj.to_str().unwrap();
    let resps = mcp(
        &proj,
        &state,
        &[call_req(1, "devrun.up", json!({ "root": root, "apps": ["ghost"] }))],
    );
    let payload = tool_json(&resps[0], true);
    assert!(payload.as_str().unwrap().contains("ghost"));
}

#[test]
fn devrun_up_requires_at_least_one_app() {
    let proj = project_with_config();
    let state = scratch("state");
    let root = proj.to_str().unwrap();
    let resps = mcp(
        &proj,
        &state,
        &[call_req(1, "devrun.up", json!({ "root": root, "apps": [] }))],
    );
    let payload = tool_json(&resps[0], true);
    assert!(payload.as_str().unwrap().contains("at least one app"));
}
```

The happy path (a real spawn) requires `doppler` on PATH, which CI lacks; the spawn mechanism itself is covered by `run::launch`'s unit test (Task 2). These two error-path tests cover the handler's argument plumbing.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --test mcp devrun_up`
Expected: FAIL — `devrun.up` is an unknown action.

- [ ] **Step 3: Implement `up`**

In `crates/devkit-mcp/src/devrun.rs`, add to the imports:

```rust
use std::collections::BTreeMap;
use std::path::Path;

use devkit_ports::load;
```

Add the `up` entry to `actions()` (after `devrun.status`):

```rust
        Action {
            name: "devrun.up",
            summary: "Start dev servers for a worktree (non-blocking; poll devrun.status for readiness).",
            schema: up_schema,
            handler: up,
        },
```

Add the handler:

```rust
#[derive(Deserialize)]
struct UpArgs {
    root: String,
    apps: Vec<String>,
    #[serde(default)]
    env: Option<BTreeMap<String, String>>,
}

fn up_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Absolute path to the worktree (holds devkit.toml; the ports holder)." },
            "apps": { "type": "array", "items": { "type": "string" }, "description": "App names from the devkit.toml catalog." },
            "env": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Per-launch env overrides (KEY=VALUE)." }
        },
        "required": ["root", "apps"],
        "additionalProperties": false
    })
}

fn up(_ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: UpArgs = serde_json::from_value(args).context("invalid devrun.up arguments")?;
    anyhow::ensure!(!a.apps.is_empty(), "devrun.up requires at least one app");

    let loaded = load::load(None, Path::new(&a.root)).context("loading devkit.toml")?;
    let catalog = &loaded.catalog;

    let mut apps = a.apps.clone();
    for app in &apps {
        anyhow::ensure!(catalog.contains_key(app), "unknown app `{app}`");
    }
    run::ensure_provider(catalog, &mut apps);

    let user = a.env.unwrap_or_default();
    let reqs: Vec<(String, u16)> = apps
        .iter()
        .map(|x| (x.clone(), catalog[x].base_port))
        .collect();
    let ports: BTreeMap<String, u16> = registry::alloc(&a.root, &reqs, Role::Issue)?
        .into_iter()
        .collect();
    let provider = catalog
        .iter()
        .find(|(_, ap)| ap.provides_url)
        .map(|(n, _)| n.clone());
    let plans = run::plan_group(
        catalog,
        &loaded.config.defaults.doppler_config,
        &apps,
        &ports,
        provider.as_deref(),
        Path::new(&a.root),
        Role::Issue,
        &user,
    );
    let statuses = run::launch(&plans, &a.root, Role::Issue, run::daemon_running(), false)?;
    Ok(serde_json::json!({
        "servers": serde_json::to_value(&statuses)?,
        "hint": "poll devrun.status for readiness"
    }))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test mcp devrun_up`
Expected: PASS (2 error-path tests).

Run: `cargo test --workspace`
Expected: PASS.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

Run: `cargo fmt --all`

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-mcp/src/devrun.rs tests/mcp.rs
git commit -m "$(cat <<'EOF'
feat(mcp): add non-blocking devrun.up action

devrun.up allocates ports, spawns the apps (issue role), and returns
immediately with each server "starting"; the agent polls devrun.status
for readiness. Uses daemon supervision only if a daemon is already
running, else direct detached spawn.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Documentation

**Files:**
- Modify: `README.md:82-91` (action list)
- Modify: `AGENTS.md:32` (devkit-mcp role line)
- Modify: `docs/next-steps.md` (mark the devrun phase done)

- [ ] **Step 1: Update the README action list**

In `README.md`, in the `## devkit-mcp (MCP server)` section, after the v1 actions paragraph (the line ending `locks.{acquire,check,release,status,prune}`), add:

```markdown
Phase-2 `devrun` actions: `devrun.status` (tracked servers for a worktree, or
`all`), `devrun.up` (start servers — **non-blocking**: returns each server
`starting`; poll `devrun.status` for readiness), `devrun.down` (stop + release
a worktree's servers), and `devrun.logs` (tail a tracked app's log). All take
`root` (the worktree); `up` is `issue`-role only and starts servers under a
running `devkitd` when present, else detached.
```

- [ ] **Step 2: Update AGENTS.md**

In `AGENTS.md`, change the `crates/devkit-mcp` row of the layout table (line 32) to:

```markdown
| `crates/devkit-mcp` | lib: stdio MCP server (`jsonrpc`, action `registry`, `ports`/`locks`/`devrun` handlers) over the port + lock facades and the `devkit-ports::run` server-lifecycle facade |
```

- [ ] **Step 3: Update next-steps.md**

In `docs/next-steps.md`, replace the `- **`devrun` actions (phase 2 — in design).**` bullet with:

```markdown
- **`devrun` actions (phase 2 — shipped).** `devrun.status`, `devrun.up`
  (non-blocking kick-and-poll), `devrun.down`, and `devrun.logs` are registered
  MCP actions over the new `devkit-ports::run` facade. `devrun up`'s blocking
  readiness wait stays CLI-only; the MCP `up` returns `starting` and the agent
  polls `devrun.status`.
```

- [ ] **Step 4: Verify the gate (docs don't affect tests, but confirm nothing regressed)**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add README.md AGENTS.md docs/next-steps.md
git commit -m "$(cat <<'EOF'
docs: document the devrun MCP actions

Record the four devrun.* actions in the README and AGENTS layout, and mark
the devrun phase shipped in next-steps.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
EOF
)"
```

---

## Self-Review

**1. Spec coverage:**
- Action catalog (status/up/down/logs) → Tasks 4, 5, 6. ✓
- Non-blocking `up` (kick-and-poll, `starting`/`ready`/`crashed`) → `run::launch(wait=false)` + `server_state` (Tasks 2, 6); states defined in `ServerState` (Task 1). ✓
- Facade extraction, no shelling → Tasks 1–3 (`devkit-ports::run`); MCP handlers call `run::*`. ✓
- `up` issue-role only → `up` hardcodes `Role::Issue` (Task 6). ✓
- `down` no-role stops all roles → `bring_down(holder, None)` releases all (Task 3). ✓
- `up` daemon-if-present else detached → `run::daemon_running()` gating (Tasks 2, 6). ✓
- `root` explicit, holder = root → all handlers require `root`; `alloc`/`bring_down`/`read_log` use it as holder (Tasks 4–6). ✓
- Error semantics (facade error → `isError`; `up` starting = success) → handler tests assert `isError` true/false (Tasks 4–6). ✓
- Testing: poll-not-sleep, skip-if-no-python → `launch` unit test and `devrun.status` polling (Tasks 2, 4). ✓

**2. Placeholder scan:** No TBD/TODO; every code step shows complete code and exact commands. ✓

**3. Type consistency:** `ServerStatus`/`ServerState`/`LaunchPlan`/`DownOutcome` defined in Task 1–3 and consumed unchanged in Tasks 4–6. `run::launch(plans, holder, role, supervise_daemon, wait)` signature is identical at its definition (Task 2) and both call sites (`cmd_up` Task 2, `up` Task 6). `bring_down`/`read_log`/`server_rows` signatures match across producer and consumers. ✓

## Unresolved questions

None blocking. Two confirm-during-execution notes already captured in the spec's Open Questions: the exact home of the extracted functions (this plan puts them all in `devkit-ports::run`) and whether `status_table` should be refactored onto `server_rows` (this plan keeps them independent to avoid touching `portm`'s CLI output — the one-line `listening()` overlap is not a duplicated logic block).
