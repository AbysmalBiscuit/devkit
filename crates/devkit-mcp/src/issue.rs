use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use devkit_issue::{prs, status};

use crate::ServerCtx;
use crate::actions::Action;

pub fn actions() -> Vec<Action> {
    vec![
        Action {
            name: "issue.status",
            summary: "List issue worktrees (optionally filtered by id) with PR/Linear state and a finished verdict.",
            schema: status_schema,
            handler: status,
        },
        Action {
            name: "issue.prs",
            summary: "Triage your GitHub PRs: the ones you authored and the ones awaiting your review.",
            schema: prs_schema,
            handler: prs_handler,
        },
    ]
}

#[derive(Deserialize)]
struct StatusArgs {
    #[serde(default)]
    root: Option<String>,
    #[serde(default)]
    ids: Vec<String>,
}

fn status_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Directory whose worktrees are enumerated (default \".\")." },
            "ids": { "type": "array", "items": { "type": "string" }, "description": "Filter to these issue ids (case-insensitive)." }
        },
        "additionalProperties": false
    })
}

fn status(_ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: StatusArgs = serde_json::from_value(args).context("invalid issue.status arguments")?;
    let root = a.root.unwrap_or_else(|| ".".to_string());
    let report = status::gather(&root, &a.ids)?;
    Ok(serde_json::to_value(report)?)
}

#[derive(Deserialize)]
struct PrsArgs {
    #[serde(default)]
    root: Option<String>,
    #[serde(default)]
    mine: bool,
    #[serde(default)]
    reviews: bool,
    #[serde(default)]
    repo: Option<String>,
}

fn prs_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Directory to run gh in (default \".\"); not the MCP server's CWD." },
            "mine": { "type": "boolean", "description": "Include PRs you authored. Neither flag set ⇒ both groups." },
            "reviews": { "type": "boolean", "description": "Include PRs awaiting your review. Neither flag set ⇒ both groups." },
            "repo": { "type": "string", "description": "owner/name to target instead of detecting from root." }
        },
        "additionalProperties": false
    })
}

fn prs_handler(_ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: PrsArgs = serde_json::from_value(args).context("invalid issue.prs arguments")?;
    let root = a.root.unwrap_or_else(|| ".".to_string());
    let report = prs::gather(&root, a.mine, a.reviews, a.repo.as_deref())?;
    Ok(serde_json::to_value(report)?)
}
