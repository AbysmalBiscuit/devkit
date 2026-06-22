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
