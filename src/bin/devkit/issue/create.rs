//! `issue create`: render the issue templates and file the result as a GitHub
//! issue. Other trackers create issues through their MCP, gated by the
//! pre-tool-use hook on `issue render`'s receipts.

use anyhow::{Context, Result};

use super::render;
use crate::template::VarArgs;

pub(crate) struct CreateArgs {
    pub title: String,
    pub body: Option<String>,
    pub vars: VarArgs,
    pub dir: Option<String>,
    pub config: Option<String>,
}

pub(crate) fn run(args: CreateArgs) -> Result<()> {
    let start = super::start(&args.dir);
    let (sel, cfg) = super::github_only("create", &start, args.config.as_deref())?;

    let rendered = render::render(
        &cfg,
        "issue create",
        render::Title::Input(&args.title),
        args.body.as_deref().unwrap_or_default(),
        &args.vars,
        devkit_common::caller::caller(),
    )?;
    let out = devkit_common::cmd::gh_capture(
        &[
            "issue",
            "create",
            "--title",
            &rendered.title,
            "--body",
            &rendered.body,
        ],
        sel.forge.repos.issues()?,
        &start,
    )?;
    let url = out
        .lines()
        .rev()
        .find(|l| l.contains("://"))
        .context("`gh issue create` printed no issue URL")?;
    println!("{}", url.trim());
    Ok(())
}
