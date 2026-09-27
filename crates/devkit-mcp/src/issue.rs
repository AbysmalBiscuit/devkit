use anyhow::{Context, Result};
use devkit_common::tracker;
use devkit_issue::{prs, status};
use serde::Deserialize;
use serde_json::Value;

use crate::{ServerCtx, actions::Action};

pub fn actions() -> Vec<Action> {
    vec![
        Action {
            name: "issue.status",
            summary: "List issue worktrees (optionally filtered by id) with PR/tracker state and a finished verdict.",
            schema: status_schema,
            handler: status,
        },
        Action {
            name: "issue.prs",
            summary: "Triage your PRs on the project's forge: the ones you authored and the ones awaiting your review.",
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
    let sel = tracker::select(None, &root, None);
    let report = status::gather_with(&root, &a.ids, &sel.tracker, &sel.forge, false)?;
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
    #[serde(default)]
    batch_size: Option<u32>,
    #[serde(default)]
    retries: Option<u32>,
}

fn prs_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Project directory (default \".\"); not the MCP server's CWD." },
            "mine": { "type": "boolean", "description": "Include PRs you authored. Neither flag set ⇒ both groups." },
            "reviews": { "type": "boolean", "description": "Include PRs awaiting your review. Neither flag set ⇒ both groups." },
            "repo": { "type": "string", "description": "owner/name to target instead of detecting from root." },
            "batch_size": { "type": "integer", "description": "PRs fetched per search page (default 25). Lower it if the forge returns HTTP 504." },
            "retries": { "type": "integer", "description": "Extra attempts per page after a failure (default 0)." }
        },
        "additionalProperties": false
    })
}

fn prs_handler(_ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: PrsArgs = serde_json::from_value(args).context("invalid issue.prs arguments")?;
    let root = a.root.unwrap_or_else(|| ".".to_string());
    let sel = tracker::select(None, &root, a.repo.as_deref());
    // Check-name globs to discount from the CHECK verdict, plus the Linear
    // PR-link opt-in. Without a config, neither.
    let ignored_checks = sel
        .config
        .as_ref()
        .map(|c| c.defaults.ignored_checks.clone())
        .unwrap_or_default();
    let resolve_pr_links = sel
        .config
        .as_ref()
        .is_some_and(|c| c.linear.resolve_pr_links);
    let repo = sel.forge.repos.prs()?;
    let report = prs::gather(
        sel.forge.forge.as_ref(),
        repo,
        a.mine,
        a.reviews,
        &ignored_checks,
        resolve_pr_links,
        prs::Fetch {
            batch_size: a.batch_size.unwrap_or(prs::DEFAULT_BATCH_SIZE),
            retries: a.retries.unwrap_or(0),
        },
        sel.tracker.tracker.as_ref(),
    )?;
    Ok(serde_json::to_value(report)?)
}
