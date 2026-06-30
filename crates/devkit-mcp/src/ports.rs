use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde_json::Value;

use devkit_ports::registry::{self, Role};

use crate::ServerCtx;
use crate::actions::Action;

pub fn actions() -> Vec<Action> {
    vec![
        Action {
            name: "ports.status",
            summary: "Show the current port-allocation registry.",
            schema: status_schema,
            handler: status,
        },
        Action {
            name: "ports.alloc",
            summary: "Allocate ports for one or more apps under a holder.",
            schema: alloc_schema,
            handler: alloc,
        },
        Action {
            name: "ports.release",
            summary: "Release a holder's port reservations.",
            schema: release_schema,
            handler: release,
        },
        Action {
            name: "ports.prune",
            summary: "Drop dead port reservations whose process is gone.",
            schema: prune_schema,
            handler: prune,
        },
        Action {
            name: "ports.strays",
            summary: "List dev servers running outside the devrun registry (read-only).",
            schema: strays_schema,
            handler: strays,
        },
    ]
}

fn status_schema() -> Value {
    serde_json::json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn status(_ctx: &ServerCtx, _args: Value) -> Result<Value> {
    let data = registry::snapshot()?;
    Ok(serde_json::to_value(data)?)
}

#[derive(Deserialize)]
struct AllocArgs {
    root: String,
    apps: Vec<String>,
    #[serde(default)]
    role: Option<Role>,
    #[serde(default)]
    holder: Option<String>,
}

fn alloc_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Absolute path to the project root (holds devkit.toml)." },
            "apps": { "type": "array", "items": { "type": "string" }, "description": "App names from the devkit.toml catalog." },
            "role": { "type": "string", "enum": ["issue", "baseline"], "description": "Allocation role (default issue)." },
            "holder": { "type": "string", "description": "Holder identity for the reservation; must be an existing path. Defaults to root." }
        },
        "required": ["root", "apps"],
        "additionalProperties": false
    })
}

fn alloc(_ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: AllocArgs = serde_json::from_value(args).context("invalid ports.alloc arguments")?;
    let holder = a.holder.unwrap_or_else(|| a.root.clone());
    let role = a.role.unwrap_or(Role::Issue);
    let loaded = devkit_ports::load::load(None, std::path::Path::new(&a.root))
        .context("loading devkit.toml")?;
    let mut reqs = Vec::with_capacity(a.apps.len());
    for app in &a.apps {
        let base = loaded
            .catalog
            .get(app)
            .ok_or_else(|| anyhow!("unknown app `{app}`"))?
            .base_port;
        reqs.push((app.clone(), base));
    }
    let allocated = registry::alloc(&holder, &reqs, role)?;
    let map: serde_json::Map<String, Value> = allocated
        .into_iter()
        .map(|(app, port)| (app, Value::from(port)))
        .collect();
    Ok(Value::Object(map))
}

#[derive(Deserialize)]
struct ReleaseArgs {
    root: String,
    #[serde(default)]
    role: Option<Role>,
    #[serde(default)]
    holder: Option<String>,
}

fn release_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Absolute path to the project root (the default holder)." },
            "role": { "type": "string", "enum": ["issue", "baseline"], "description": "Only release this role (default: all roles)." },
            "holder": { "type": "string", "description": "Override the holder (defaults to root)." }
        },
        "required": ["root"],
        "additionalProperties": false
    })
}

fn release(_ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: ReleaseArgs = serde_json::from_value(args).context("invalid ports.release arguments")?;
    let holder = a.holder.unwrap_or_else(|| a.root.clone());
    let freed = registry::release(&holder, a.role)?;
    Ok(serde_json::json!({ "freed": freed }))
}

fn prune_schema() -> Value {
    serde_json::json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn prune(_ctx: &ServerCtx, _args: Value) -> Result<Value> {
    let freed = registry::prune()?;
    Ok(serde_json::json!({ "freed": freed }))
}

fn strays_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Repo/worktree path to resolve devkit.toml from." }
        },
        "required": ["root"],
        "additionalProperties": false
    })
}

#[derive(Deserialize)]
struct StraysArgs {
    root: String,
}

fn strays(_ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: StraysArgs = serde_json::from_value(args).context("invalid ports.strays arguments")?;
    let loaded = devkit_ports::load::load(None, std::path::Path::new(&a.root))
        .context("loading devkit.toml for ports.strays")?;
    let data = registry::snapshot()?;
    let strays = devkit_ports::strays::scan(&loaded.config, &data);
    Ok(serde_json::to_value(strays)?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn strays_action_is_registered() {
        assert!(super::actions().iter().any(|a| a.name == "ports.strays"));
    }
}
