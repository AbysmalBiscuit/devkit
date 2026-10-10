//! `ticket edit`: render the issue templates and write the result over an
//! existing GitHub issue. Other trackers edit issues through their MCP, gated
//! by the pre-tool-use hook on `ticket render`'s receipts.

use anyhow::{Context, Result};

use super::render::{self, Title};
use crate::template::VarArgs;

pub(crate) struct EditArgs {
    pub issue: String,
    pub title: Option<String>,
    pub body: String,
    pub vars: VarArgs,
    pub dir: Option<String>,
    pub config: Option<String>,
}

pub(crate) fn run(args: EditArgs) -> Result<()> {
    let start = super::start(&args.dir);
    let (sel, cfg) = super::github_only("edit", &start, args.config.as_deref())?;
    let tracker = &sel.tracker.tracker;
    let repo = sel.forge.repos.issues()?;
    let id = tracker.issue_ref(&args.issue)?.id;

    let current;
    let title = match &args.title {
        Some(input) => Title::Input(input),
        None => {
            current = tracker
                .title(&id)?
                .with_context(|| format!("issue {id} was not found"))?;
            Title::Kept(&current)
        }
    };
    let rendered = render::render(
        &cfg,
        "ticket edit",
        title,
        &args.body,
        &args.vars,
        devkit_common::caller::caller(),
    )?;

    let mut gh = vec!["issue", "edit", id.as_str()];
    if args.title.is_some() {
        gh.extend(["--title", &rendered.title]);
    }
    gh.extend(["--body", &rendered.body]);
    let out = devkit_common::cmd::gh_capture(&gh, repo, &start)?;
    if let Some(url) = out.lines().rev().find(|l| l.contains("://")) {
        println!("{}", url.trim());
    }
    Ok(())
}
