//! `ticket create`: render the issue templates, file the result as a GitHub
//! issue or a Linear ticket with every declared field it was given, and read
//! the fields back.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use devkit_common::{
    caller::Caller,
    fields::{self, Report, Resolved, Surface},
    tracker::{
        Selected,
        github_fields::{self, GithubFieldWriter},
        linear_fields::{self, LinearFieldWriter},
    },
};
use devkit_config::{Config, FieldDecl, Place, TrackerKind};

use super::{TicketTracker, field_write, render};
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
    let (sel, cfg, tracker) = super::ticket_tracker("create", &start, args.config.as_deref())?;
    match tracker {
        TicketTracker::Linear => linear(args, &sel, &cfg),
        TicketTracker::Github => github(args, &start, &sel, &cfg),
    }
}

/// A create's arguments split and its fields resolved and checked, refusing
/// before any write.
struct Checked<'a> {
    decls: &'a BTreeMap<String, FieldDecl>,
    input: render::TicketArgs,
    resolved: Resolved,
    report: Report,
    caller: Caller,
}

fn checked<'a>(
    args: &CreateArgs,
    sel: &Selected,
    cfg: &'a Config,
    kind: TrackerKind,
) -> Result<Checked<'a>> {
    let decls = cfg.ticket.fields.for_tracker(kind);
    let input = render::ticket_args(cfg, &args.vars, &args.fields, decls)?;
    let given = input.fields.with_defaults(decls);
    let mut resolved = render::resolve_fields(cfg, kind, decls, &given)?;
    field_write::number_issue_refs(sel.tracker.tracker.as_ref(), decls, &mut resolved)?;
    let caller = devkit_common::caller::caller();
    let report = fields::check(decls, Surface::Create, &resolved, None, caller);
    field_write::refuse("ticket create", decls, &report)?;
    Ok(Checked {
        decls,
        input,
        resolved,
        report,
        caller,
    })
}

fn github(args: CreateArgs, start: &str, sel: &Selected, cfg: &Config) -> Result<()> {
    let Checked {
        decls,
        input,
        resolved,
        report,
        caller,
    } = checked(&args, sel, cfg, TrackerKind::Github)?;

    let rendered = render::render(
        cfg,
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
    let out = devkit_common::cmd::gh_capture(&refs, repo, start).with_context(|| {
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
    let writer = GithubFieldWriter::new(repo.clone(), cfg.github.project.clone(), start);
    let written = fields::Resolved {
        nones: Default::default(),
        ..resolved
    };
    let problems = field_write::write_and_verify(
        &writer,
        number,
        decls,
        &written,
        None,
        &github_fields::on_create(decls),
    );
    field_write::warn(decls, &report);
    field_write::fail_on(
        &problems,
        format!(
            "issue #{number} was created ({url}), but the fields above did not take. Set them \
             with `devkit ticket edit {number} --arg <field>=...`; rerunning `ticket create` \
             would file a second issue"
        ),
    )
}

fn linear(args: CreateArgs, sel: &Selected, cfg: &Config) -> Result<()> {
    let team_field = cfg
        .ticket
        .fields
        .for_tracker(TrackerKind::Linear)
        .iter()
        .find(|(_, d)| d.r#where == Some(Place::Team))
        .map(|(name, _)| name.clone())
        .context(
            "a Linear ticket needs a team, and no field declares one. Declare it in devkit.toml \
             as `[ticket.fields.linear.team]` with `type = \"enum\"`, `where = \"team\"` and \
             each team as `values = [{ name = \"ENG\", id = \"<team id>\" }]`",
        )?;
    let Checked {
        decls,
        input,
        resolved,
        report,
        caller,
    } = checked(&args, sel, cfg, TrackerKind::Linear)?;
    let team = resolved
        .fields
        .get(&team_field)
        .map(|v| linear_fields::written_ids(v))
        .and_then(|ids| ids.into_iter().next());
    let Some(team) = team else {
        bail!(
            "ticket create refused, nothing was written:\n{}",
            fields::message(decls, &[fields::Problem::Missing { field: team_field }])
        );
    };
    let key = field_write::linear_key()?;

    let rendered = render::render(
        cfg,
        "ticket create",
        render::Title::Input(&args.title),
        Some(args.body.as_deref().unwrap_or_default()),
        &input.template,
        &render::shown(decls, &resolved),
        caller,
    )?;
    let written = fields::Resolved {
        nones: Default::default(),
        ..resolved
    };
    let writer = LinearFieldWriter::new(key);
    let refs = writer.refs(&linear_fields::issue_refs(decls, &written))?;
    let id = linear_fields::create_id(&team, &rendered.title, &rendered.body);
    let create =
        linear_fields::create_input(id, &rendered.title, &rendered.body, decls, &written, &refs)?;
    let created = writer
        .create(&create)
        .map_err(|e| field_write::linear_refused(e, decls, &written))?;
    let issue = created.issue;
    let ident = issue.identifier.clone();
    println!("{}", field_write::linear_link(&issue));
    if created.existed {
        eprintln!(
            "note: {ident} already had this create's id, so an earlier run of the same \
             `ticket create` filed it; no second ticket was filed"
        );
    }
    let (_, problems) = field_write::linear_finish(&writer, issue, decls, &written, &refs, false);
    field_write::warn(decls, &report);
    field_write::fail_on(
        &problems,
        format!(
            "{ident} was created, but the fields above did not take. Set them with `devkit \
             ticket edit {ident} --arg <field>=...`"
        ),
    )
}
