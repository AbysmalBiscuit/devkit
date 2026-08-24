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
            summary: "List issue worktrees (optionally filtered by id) with PR/tracker state and a finished verdict.",
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
    let loaded = project_config(&root);
    let kind = loaded.as_ref().and_then(|l| l.config.tracker.kind);
    let tracker = devkit_common::tracker::resolve(kind, std::path::Path::new(&root));
    let default_gh = devkit_config::GithubConfig::default();
    let github_cfg = loaded
        .as_ref()
        .map(|l| &l.config.github)
        .unwrap_or(&default_gh);
    let repos = devkit_common::github::Repos::resolve(github_cfg, &root, None);
    let report = status::gather_with(&root, &a.ids, &tracker, &repos)?;
    Ok(serde_json::to_value(report)?)
}

/// The config reachable from `root`, or `None` when none resolves. A project
/// without a `devkit.toml` — or with one that fails to load — still gets its
/// triage answer, with every config-driven choice left at its default.
fn project_config(root: &str) -> Option<devkit_ports::load::Loaded> {
    devkit_ports::load::load(None, std::path::Path::new(root)).ok()
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
            "root": { "type": "string", "description": "Directory to run gh in (default \".\"); not the MCP server's CWD." },
            "mine": { "type": "boolean", "description": "Include PRs you authored. Neither flag set ⇒ both groups." },
            "reviews": { "type": "boolean", "description": "Include PRs awaiting your review. Neither flag set ⇒ both groups." },
            "repo": { "type": "string", "description": "owner/name to target instead of detecting from root." },
            "batch_size": { "type": "integer", "description": "PRs fetched per search page (default 25). Lower it if GitHub returns HTTP 504." },
            "retries": { "type": "integer", "description": "Extra attempts per page after a failure (default 0)." }
        },
        "additionalProperties": false
    })
}

fn prs_handler(_ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: PrsArgs = serde_json::from_value(args).context("invalid issue.prs arguments")?;
    let root = a.root.unwrap_or_else(|| ".".to_string());
    // Check-name globs to discount from the CHECK verdict, plus the Linear
    // PR-link opt-in; absent config ⇒ neither.
    let loaded = project_config(&root);
    let ignored_checks = loaded
        .as_ref()
        .map(|l| l.config.defaults.ignored_checks.clone())
        .unwrap_or_default();
    let resolve_pr_links = loaded
        .as_ref()
        .is_some_and(|l| l.config.linear.resolve_pr_links);
    let default_gh = devkit_config::GithubConfig::default();
    let github_cfg = loaded
        .as_ref()
        .map(|l| &l.config.github)
        .unwrap_or(&default_gh);
    let repo = devkit_common::github::Repos::resolve(github_cfg, &root, a.repo.as_deref())
        .prs()?
        .slug
        .clone();
    let kind = loaded.as_ref().and_then(|l| l.config.tracker.kind);
    let tracker = devkit_common::tracker::resolve(kind, std::path::Path::new(&root));
    let report = prs::gather(
        &root,
        a.mine,
        a.reviews,
        &repo,
        &ignored_checks,
        resolve_pr_links,
        prs::Fetch {
            batch_size: a.batch_size.unwrap_or(prs::DEFAULT_BATCH_SIZE),
            retries: a.retries.unwrap_or(0),
        },
        tracker.tracker.as_ref(),
    )?;
    Ok(serde_json::to_value(report)?)
}
