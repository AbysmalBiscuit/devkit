//! `ticket create`: render the issue templates, file the result as a GitHub
//! issue with every declared field it was given, and read the fields back.
//! Other trackers create issues through their MCP, gated by the pre-tool-use
//! hook on `ticket render`'s receipts.

use anyhow::{Context, Result, bail};
use devkit_common::{
    fields::{self, Surface},
    tracker::github_fields::{self, GithubFieldWriter},
};
use devkit_config::TrackerKind;

use super::{field_write, render};
use crate::template::{FieldArgs, VarArgs};

pub(crate) struct CreateArgs {
    pub title: String,
    pub body: Option<String>,
    pub vars: VarArgs,
    pub fields: FieldArgs,
    pub dir: Option<String>,
    pub config: Option<String>,
}

pub(crate) fn run(args: CreateArgs) -> Result<()> {
    let start = super::start(&args.dir);
    let (sel, cfg) = super::github_only("create", &start, args.config.as_deref())?;
    let caller = devkit_common::caller::caller();
    let decls = cfg.ticket.fields.for_tracker(TrackerKind::Github);
    let input = render::ticket_args(&cfg, &args.vars, &args.fields, decls)?;
    let given = input.fields.with_defaults(decls);
    let mut resolved = render::resolve_fields(&cfg, TrackerKind::Github, decls, &given)?;
    field_write::number_issue_refs(sel.tracker.tracker.as_ref(), decls, &mut resolved)?;
    let report = fields::check(decls, Surface::Create, &resolved, None, caller);
    field_write::refuse("ticket create", decls, &report)?;

    let rendered = render::render(
        &cfg,
        "ticket create",
        render::Title::Input(&args.title),
        Some(args.body.as_deref().unwrap_or_default()),
        &input.template,
        &render::shown(decls, &resolved),
        caller,
    )?;
    let repo = sel.forge.repos.issues()?;
    let mut gh: Vec<String> = [
        "issue",
        "create",
        "--title",
        &rendered.title,
        "--body",
        &rendered.body,
    ]
    .map(String::from)
    .into();
    gh.extend(github_fields::create_flags(decls, &resolved));
    let refs: Vec<&str> = gh.iter().map(String::as_str).collect();
    let out = devkit_common::cmd::gh_capture(&refs, repo, &start).with_context(|| {
        format!(
            "`gh issue create` failed, and the issue may have been created anyway: search {} \
             for the title `{}` before retrying, so a retry does not file it twice",
            repo.slug, rendered.title
        )
    })?;
    let url = out
        .lines()
        .rev()
        .find(|l| l.contains("://"))
        .context("`gh issue create` printed no issue URL")?
        .trim()
        .to_string();
    println!("{url}");

    let number = field_write::number_of(&url)?;
    let writer = GithubFieldWriter::new(repo.clone(), cfg.github.project.clone(), &start);
    let problems = field_write::write_and_verify(
        &writer,
        number,
        decls,
        &resolved,
        None,
        &github_fields::on_create(decls),
        false,
    );
    field_write::warn(decls, &report);
    if !problems.is_empty() {
        for p in &problems {
            eprintln!("  {p}");
        }
        bail!(
            "issue #{number} was created ({url}), but the fields above did not take. Set them \
             with `devkit ticket edit {number} --arg <field>=...`; rerunning `ticket create` \
             would file a second issue"
        );
    }
    Ok(())
}
