use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

use devkit_locks::normalize_under_root;
use devkit_locks::{
    acquire_resolved, check_resolved, release_all_resolved, release_resolved, status_resolved,
};

use crate::ServerCtx;
use crate::actions::Action;

pub fn actions() -> Vec<Action> {
    vec![
        Action {
            name: "locks.acquire",
            summary: "Claim one or more paths for the session (all-or-nothing).",
            schema: acquire_schema,
            handler: acquire,
        },
        Action {
            name: "locks.check",
            summary: "Check whether paths are locked by another holder.",
            schema: check_schema,
            handler: check,
        },
        Action {
            name: "locks.release",
            summary: "Release locks the session holds.",
            schema: release_schema,
            handler: release,
        },
        Action {
            name: "locks.status",
            summary: "List held locks for the project (or all projects).",
            schema: status_schema,
            handler: status,
        },
        Action {
            name: "locks.prune",
            summary: "Drop expired or dead locks.",
            schema: prune_schema,
            handler: prune,
        },
    ]
}

fn resolve_holder(ctx: &ServerCtx, given: Option<String>) -> String {
    given.unwrap_or_else(|| ctx.default_holder.clone())
}

/// Express each input path as a root-relative key. Inputs may be absolute or
/// relative to `root`.
fn normalize(root: &str, paths: &[String]) -> Result<Vec<String>> {
    let root_path = Path::new(root);
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let pp = Path::new(p);
        let abs = if pp.is_absolute() {
            pp.to_path_buf()
        } else {
            root_path.join(pp)
        };
        out.push(
            normalize_under_root(&abs, root_path).with_context(|| format!("normalizing {p}"))?,
        );
    }
    Ok(out)
}

#[derive(Deserialize)]
struct AcquireArgs {
    root: String,
    paths: Vec<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default = "default_ttl")]
    ttl: u64,
    #[serde(default)]
    holder: Option<String>,
}

fn default_ttl() -> u64 {
    1800
}

fn acquire_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Absolute path to the project root." },
            "paths": { "type": "array", "items": { "type": "string" }, "description": "Paths to lock (absolute or root-relative)." },
            "note": { "type": "string", "description": "Optional note shown to others who hit the lock." },
            "ttl": { "type": "integer", "minimum": 0, "description": "Lease seconds; 0 = no expiry. Default 1800." },
            "holder": { "type": "string", "description": "Override the session holder id." }
        },
        "required": ["root", "paths"],
        "additionalProperties": false
    })
}

fn acquire(ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: AcquireArgs = serde_json::from_value(args).context("invalid locks.acquire arguments")?;
    let holder = resolve_holder(ctx, a.holder);
    let paths = normalize(&a.root, &a.paths)?;
    let outcome = acquire_resolved(&a.root, &holder, &paths, None, a.note.as_deref(), a.ttl)?;
    Ok(serde_json::to_value(outcome)?)
}

#[derive(Deserialize)]
struct CheckArgs {
    root: String,
    paths: Vec<String>,
    #[serde(default)]
    holder: Option<String>,
}

fn check_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Absolute path to the project root." },
            "paths": { "type": "array", "items": { "type": "string" }, "description": "Paths to check." },
            "holder": { "type": "string", "description": "Override the session holder id (a path held by this holder is not a conflict)." }
        },
        "required": ["root", "paths"],
        "additionalProperties": false
    })
}

fn check(ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: CheckArgs = serde_json::from_value(args).context("invalid locks.check arguments")?;
    let holder = resolve_holder(ctx, a.holder);
    let paths = normalize(&a.root, &a.paths)?;
    let conflicts = check_resolved(&a.root, &holder, &paths)?;
    Ok(serde_json::to_value(conflicts)?)
}

#[derive(Deserialize)]
struct ReleaseArgs {
    root: String,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    all: bool,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    holder: Option<String>,
}

fn release_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Absolute path to the project root." },
            "paths": { "type": "array", "items": { "type": "string" }, "description": "Paths to release (required unless all=true)." },
            "all": { "type": "boolean", "description": "Release every lock held by this holder in the project." },
            "force": { "type": "boolean", "description": "Release even locks held by another holder." },
            "holder": { "type": "string", "description": "Override the session holder id." }
        },
        "required": ["root"],
        "additionalProperties": false
    })
}

fn release(ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: ReleaseArgs = serde_json::from_value(args).context("invalid locks.release arguments")?;
    let holder = resolve_holder(ctx, a.holder);
    if a.all {
        let released = release_all_resolved(&a.root, &holder)?;
        return Ok(serde_json::json!({ "released": released, "refused": [] }));
    }
    if a.paths.is_empty() {
        bail!("locks.release requires `paths` unless `all` is true");
    }
    let paths = normalize(&a.root, &a.paths)?;
    let (released, refused) = release_resolved(&a.root, &holder, &paths, a.force)?;
    Ok(serde_json::json!({ "released": released, "refused": refused }))
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
            "root": { "type": "string", "description": "Absolute path to the project root (required unless all=true)." },
            "all": { "type": "boolean", "description": "List locks across all projects." }
        },
        "additionalProperties": false
    })
}

fn status(_ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: StatusArgs = serde_json::from_value(args).context("invalid locks.status arguments")?;
    let root = match (a.root, a.all) {
        (Some(r), _) => r,
        (None, true) => String::new(),
        (None, false) => bail!("locks.status requires `root` unless `all` is true"),
    };
    let entries = status_resolved(&root, a.all)?;
    Ok(serde_json::to_value(entries)?)
}

fn prune_schema() -> Value {
    serde_json::json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn prune(_ctx: &ServerCtx, _args: Value) -> Result<Value> {
    let pruned = devkit_locks::prune()?;
    Ok(serde_json::json!({ "pruned": pruned }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ServerCtx {
        ServerCtx {
            default_holder: format!("mcp-locks-test-{}", std::process::id()),
        }
    }

    #[test]
    fn acquire_status_release_roundtrip_through_handlers() {
        let root = std::env::temp_dir().join(format!("devkit-mcp-locks-{}", std::process::id()));
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let r = root.to_string_lossy().into_owned();
        let c = ctx();

        let out = acquire(&c, serde_json::json!({ "root": r, "paths": ["x.rs"], "ttl": 60 }))
            .expect("acquire");
        assert_eq!(out["acquired"].as_array().unwrap().len(), 1);

        let st = status(&c, serde_json::json!({ "root": r })).expect("status");
        assert!(st.as_array().unwrap().iter().any(|e| e["path"] == "x.rs"));

        let rel = release(&c, serde_json::json!({ "root": r, "paths": ["x.rs"] }))
            .expect("release");
        assert_eq!(rel["released"], serde_json::json!(["x.rs"]));

        let _ = std::fs::remove_dir_all(&root);
    }
}
