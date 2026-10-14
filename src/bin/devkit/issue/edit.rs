//! `ticket edit`: render the issue templates over an existing GitHub issue's
//! body (and title), write the declared fields named, and read them back.
//! Other trackers edit issues through their MCP, gated by the pre-tool-use
//! hook on `ticket render`'s receipts.

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use devkit_common::{
    fields::{self, Surface},
    tracker::github_fields::GithubFieldWriter,
};
use devkit_config::TrackerKind;

use super::{
    field_write,
    render::{self, Title},
};
use crate::template::{FieldArgs, VarArgs};

pub(crate) struct EditArgs {
    pub issue: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub vars: VarArgs,
    pub fields: FieldArgs,
    pub dir: Option<String>,
    pub config: Option<String>,
}

pub(crate) fn run(args: EditArgs) -> Result<()> {
    let start = super::start(&args.dir);
    let (sel, cfg) = super::github_only("edit", &start, args.config.as_deref())?;
    let tracker = &sel.tracker.tracker;
    let repo = sel.forge.repos.issues()?;
    let id = tracker.issue_ref(&args.issue)?.id;
    let caller = devkit_common::caller::caller();
    let decls = cfg.ticket.fields.for_tracker(TrackerKind::Github);
    let input = render::ticket_args(&cfg, &args.vars, &args.fields, decls)?;
    let mut resolved = render::resolve_fields(&cfg, TrackerKind::Github, decls, &input.fields)?;
    field_write::number_issue_refs(tracker.as_ref(), decls, &mut resolved)?;
    let has_fields = !resolved.fields.is_empty() || !resolved.nones.is_empty();
    let rewrites_text = args.body.is_some() || args.title.is_some();
    if !rewrites_text && !has_fields {
        bail!(
            "nothing to edit: pass --body, --title, or a field as --arg <field>=... or --none \
             <field>"
        );
    }

    let number: u64 = id
        .parse()
        .with_context(|| format!("`{id}` is not a GitHub issue number"))?;
    let writer = GithubFieldWriter::new(repo.clone(), cfg.github.project.clone(), &start);
    let templates_read_a_field = rewrites_text
        && render::reads(&cfg, args.title.is_some(), args.body.is_some())?
            .iter()
            .any(|n| decls.contains_key(n));
    let before = if has_fields || templates_read_a_field {
        Some(writer.read(number, decls, Some(&resolved))?)
    } else {
        None
    };
    let current = before.as_ref().map(|b| b.current(decls, &cfg.people));
    let report = fields::check(decls, Surface::Edit, &resolved, current.as_ref(), caller);
    field_write::refuse("ticket edit", decls, &report)?;

    let mut url = format!("https://{}/{}/issues/{number}", repo.host, repo.slug);
    if rewrites_text {
        let kept;
        let title = match &args.title {
            Some(input) => Title::Input(input),
            None => {
                kept = tracker
                    .title(&id)?
                    .with_context(|| format!("issue {id} was not found"))?;
                Title::Kept(&kept)
            }
        };
        let mut shown = render::shown(decls, &Default::default());
        if let Some(current) = &current {
            for (name, values) in &current.0 {
                shown.insert(name.clone(), values.join(", "));
            }
        }
        for (name, text) in render::shown(decls, &resolved) {
            if resolved.fields.contains_key(&name) || resolved.nones.contains(&name) {
                shown.insert(name, text);
            }
        }
        let body = args.body.as_deref();
        let rendered = render::render(
            &cfg,
            "ticket edit",
            title,
            body,
            &input.template,
            &shown,
            caller,
        )?;
        let mut gh = vec!["issue", "edit", id.as_str()];
        if args.title.is_some() {
            gh.extend(["--title", &rendered.title]);
        }
        if body.is_some() {
            gh.extend(["--body", &rendered.body]);
        }
        let out = devkit_common::cmd::gh_capture(&gh, repo, &start)?;
        if let Some(u) = out.lines().rev().find(|l| l.contains("://")) {
            url = u.trim().to_string();
        }
    }

    let problems = if has_fields {
        field_write::write_and_verify(
            &writer,
            number,
            decls,
            &resolved,
            before,
            &BTreeSet::new(),
        )
    } else {
        Vec::new()
    };
    println!("{url}");
    field_write::warn(decls, &report);
    if !problems.is_empty() {
        for p in &problems {
            eprintln!("  {p}");
        }
        bail!(
            "issue #{number} was edited, but the fields above did not take. Rerun \
             `devkit ticket edit {number}` with them"
        );
    }
    Ok(())
}
