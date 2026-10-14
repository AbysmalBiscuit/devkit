//! `ticket edit`: render the issue templates over an existing GitHub issue's
//! or Linear ticket's body (and title), write the declared fields named, and
//! read them back.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, anyhow};
use devkit_common::{
    fields::{self, Current, Resolved, Surface},
    tracker::{
        Selected,
        github_fields::GithubFieldWriter,
        linear_fields::{self, LinearFieldWriter},
    },
};
use devkit_config::{Config, FieldDecl, TrackerKind};

use super::{
    TicketTracker, field_write,
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
    let (sel, cfg, tracker) = super::ticket_tracker("edit", &start, args.config.as_deref())?;
    match tracker {
        TicketTracker::Linear => linear(args, &sel, &cfg),
        TicketTracker::Github => github(args, &start, &sel, &cfg),
    }
}

/// What the templates read for each field on an edit: the ticket's current
/// values with the named fields laid over them.
fn shown(
    decls: &BTreeMap<String, FieldDecl>,
    current: Option<&Current>,
    resolved: &Resolved,
) -> BTreeMap<String, String> {
    let mut shown = render::shown(decls, &Default::default());
    for (name, values) in current.into_iter().flat_map(|c| &c.0) {
        shown.insert(name.clone(), values.join(", "));
    }
    for (name, text) in render::shown(decls, resolved) {
        if resolved.fields.contains_key(&name) || resolved.nones.contains(&name) {
            shown.insert(name, text);
        }
    }
    shown
}

fn nothing_to_edit() -> anyhow::Error {
    anyhow!(
        "nothing to edit: pass --body, --title, or a field as --arg <field>=... or --none <field>"
    )
}

fn github(args: EditArgs, start: &str, sel: &Selected, cfg: &Config) -> Result<()> {
    let tracker = &sel.tracker.tracker;
    let repo = sel.forge.repos.issues()?;
    let id = tracker.issue_ref(&args.issue)?.id;
    let caller = devkit_common::caller::caller();
    let decls = cfg.ticket.fields.for_tracker(TrackerKind::Github);
    let input = render::ticket_args(cfg, &args.vars, &args.fields, decls)?;
    let mut resolved = render::resolve_fields(cfg, TrackerKind::Github, decls, &input.fields)?;
    field_write::number_issue_refs(tracker.as_ref(), decls, &mut resolved)?;
    let has_fields = !resolved.fields.is_empty() || !resolved.nones.is_empty();
    let rewrites_text = args.body.is_some() || args.title.is_some();
    if !rewrites_text && !has_fields {
        return Err(nothing_to_edit());
    }

    let number: u64 = id
        .parse()
        .with_context(|| format!("`{id}` is not a GitHub issue number"))?;
    let writer = GithubFieldWriter::new(repo.clone(), cfg.github.project.clone(), start);
    let templates_read_a_field = rewrites_text
        && render::reads(cfg, args.title.is_some(), args.body.is_some())?
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
        let shown = shown(decls, current.as_ref(), &resolved);
        let body = args.body.as_deref();
        let rendered = render::render(
            cfg,
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
        let out = devkit_common::cmd::gh_capture(&gh, repo, start)?;
        if let Some(u) = out.lines().rev().find(|l| l.contains("://")) {
            url = u.trim().to_string();
        }
    }

    let problems = if has_fields {
        field_write::write_and_verify(&writer, number, decls, &resolved, before, &BTreeSet::new())
    } else {
        Vec::new()
    };
    println!("{url}");
    field_write::warn(decls, &report);
    field_write::fail_on(
        &problems,
        format!(
            "issue #{number} was edited, but the fields above did not take. Rerun \
             `devkit ticket edit {number}` with them"
        ),
    )
}

fn linear(args: EditArgs, sel: &Selected, cfg: &Config) -> Result<()> {
    let id = sel.tracker.tracker.issue_ref(&args.issue)?.id;
    let caller = devkit_common::caller::caller();
    let decls = cfg.ticket.fields.for_tracker(TrackerKind::Linear);
    let input = render::ticket_args(cfg, &args.vars, &args.fields, decls)?;
    let rewrites_text = args.body.is_some() || args.title.is_some();
    if !rewrites_text && input.fields.values.is_empty() && input.fields.nones.is_empty() {
        return Err(nothing_to_edit());
    }
    let writer = LinearFieldWriter::new(field_write::linear_key()?);
    let before = writer.read(&id)?;
    let current = before.current(decls, &cfg.people);
    let team = fields::team(decls, &input.fields).or_else(|| before.team_name(decls));
    let mut resolved = fields::resolve(
        decls,
        TrackerKind::Linear,
        &input.fields,
        &cfg.people,
        team.as_deref(),
    )
    .map_err(|problems| anyhow!("{}", fields::message(decls, &problems)))?;
    field_write::number_issue_refs(sel.tracker.tracker.as_ref(), decls, &mut resolved)?;
    let report = fields::check(decls, Surface::Edit, &resolved, Some(&current), caller);
    field_write::refuse("ticket edit", decls, &report)?;
    let refs = writer.refs(&linear_fields::issue_refs(decls, &resolved))?;

    let rendered = if rewrites_text {
        let title = match &args.title {
            Some(input) => Title::Input(input),
            None => Title::Kept(&before.title),
        };
        Some(render::render(
            cfg,
            "ticket edit",
            title,
            args.body.as_deref(),
            &input.template,
            &shown(decls, Some(&current), &resolved),
            caller,
        )?)
    } else {
        None
    };
    let update = linear_fields::update_input(
        rendered
            .as_ref()
            .filter(|_| args.title.is_some())
            .map(|r| r.title.as_str()),
        rendered
            .as_ref()
            .filter(|_| args.body.is_some())
            .map(|r| r.body.as_str()),
        decls,
        &resolved,
        &refs,
    )?;
    writer
        .update(&before.uuid, &update)
        .map_err(|e| field_write::linear_refused(e, decls, &resolved))
        .with_context(|| format!("Linear refused or did not answer the edit of {id}"))?;

    let (after, problems) =
        field_write::linear_finish(&writer, before, decls, &resolved, &refs, true);
    println!("{}", field_write::linear_link(&after));
    field_write::warn(decls, &report);
    field_write::fail_on(
        &problems,
        format!(
            "{id} was edited, but the fields above did not take. Rerun `devkit ticket edit {id}` \
             with them"
        ),
    )
}
