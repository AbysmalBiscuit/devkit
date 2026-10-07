use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use devkit_ports::{load, templates};
use serde::Deserialize;
use serde_json::Value;

use crate::{ServerCtx, actions::Action};

/// All three read only and act on the server's own checkout, so none takes a
/// `root` for `assert_own_worktree` to police.
pub fn actions() -> Vec<Action> {
    vec![
        Action {
            name: "templates.list",
            summary: "List the custom and built-in templates, with the args each reads.",
            schema: list_schema,
            handler: list,
        },
        Action {
            name: "templates.show",
            summary: "Show a template's source and each arg: who must pass it, its default, and its description.",
            schema: name_schema,
            handler: show,
        },
        Action {
            name: "templates.render",
            summary: "Render a template with the given args and return the text, for the caller to deliver itself.",
            schema: render_schema,
            handler: render,
        },
    ]
}

/// The server's checkout, or its working directory when it was started
/// outside a repository.
fn start(ctx: &ServerCtx) -> &Path {
    ctx.own_worktree.as_deref().unwrap_or(Path::new("."))
}

fn loaded(ctx: &ServerCtx) -> Result<load::Loaded> {
    load::load(None, start(ctx)).context("loading devkit.toml")
}

fn list_schema() -> Value {
    serde_json::json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn list(ctx: &ServerCtx, _args: Value) -> Result<Value> {
    let rows = templates::list(
        &loaded(ctx)?.config,
        start(ctx),
        devkit_common::caller::caller(),
    )?;
    Ok(serde_json::to_value(rows)?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NameArgs {
    name: String,
}

fn name_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "name": { "type": "string", "description": "Template name from templates.list." }
        },
        "required": ["name"],
        "additionalProperties": false
    })
}

fn show(ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: NameArgs = serde_json::from_value(args).context("invalid templates.show arguments")?;
    let t = templates::show(
        &loaded(ctx)?.config,
        start(ctx),
        &a.name,
        devkit_common::caller::caller(),
    )?;
    Ok(serde_json::to_value(t)?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenderArgs {
    name: String,
    #[serde(default)]
    args: BTreeMap<String, String>,
    subject: Option<String>,
    body: Option<String>,
    #[serde(default)]
    coauthor: Vec<String>,
}

fn render_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "name": { "type": "string", "description": "Template name from templates.list." },
            "args": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Values for the names the template reads, over their [templates.variables] defaults. Not the commit_message parts, which take their own parameters." },
            "subject": { "type": "string", "description": "commit_message only: the subject line, as `devkit commit --subject`." },
            "body": { "type": "string", "description": "commit_message only: the body, as `devkit commit --body`." },
            "coauthor": { "type": "array", "items": { "type": "string" }, "description": "commit_message only: co-authors, `Name <email>`, one `Co-authored-by` trailer each, as `devkit commit --coauthor`." }
        },
        "required": ["name"],
        "additionalProperties": false
    })
}

fn render(ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: RenderArgs =
        serde_json::from_value(args).context("invalid templates.render arguments")?;
    let text = templates::render(
        &loaded(ctx)?.config,
        start(ctx),
        &a.name,
        (
            &templates::CommitMessage {
                subject: a.subject.as_deref(),
                body: a.body.as_deref(),
                coauthors: &a.coauthor,
            },
            templates::PartNames::Params,
        ),
        &a.args,
        devkit_common::caller::caller(),
    )?;
    Ok(serde_json::json!({ "text": text }))
}
