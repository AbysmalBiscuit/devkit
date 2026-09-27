//! `issue create`: render the issue templates and file the result as a GitHub
//! issue. Other trackers create issues through their MCP, gated by the
//! pre-tool-use hook on `issue render`'s receipts.

use anyhow::{Context, Result, bail};
use devkit_config::TrackerKind;

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
    let sel = super::tracker::select_full(args.config.as_deref(), &start, None);
    let cfg = sel
        .config
        .context("`issue create` needs a loadable devkit.toml")?;
    let kind = cfg
        .tracker
        .kind
        .unwrap_or_else(|| sel.tracker.tracker.kind());
    if kind != TrackerKind::Github {
        bail!(
            "`issue create` writes GitHub issues only, and this project's tracker is {}. \
             Render the issue with `devkit issue render` and create it through the tracker's MCP.",
            kind.as_str()
        );
    }

    let rendered = render::render(
        &cfg,
        "issue create",
        &args.title,
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
        sel.repos.issues()?,
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
