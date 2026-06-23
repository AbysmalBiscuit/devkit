use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde_json::Value;

use devkit_ports::load;
use devkit_ports::registry::{self, Role};
use devkit_ports::run;

use crate::ServerCtx;
use crate::actions::Action;

pub fn actions() -> Vec<Action> {
    vec![
        Action {
            name: "devrun.up",
            summary: "Start dev servers for a worktree (non-blocking; poll devrun.status for readiness).",
            schema: up_schema,
            handler: up,
        },
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
