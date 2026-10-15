//! `ticket render`: a ticket's title and body from the `ticket_title` and
//! `ticket_body` templates, printed for a tracker MCP call, with a receipt per
//! field the pre-tool-use hook checks that call against.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow, bail};
use devkit_common::{
    caller::Caller,
    fields::{self, Given, Naming, Resolved},
    required::Missing,
};
/// The names the issue templates' render contexts supply, which no `--arg`
/// can. `the_issue_context_keys_match_what_render_builds` holds them against
/// the contexts.
pub(crate) use devkit_config::ISSUE_CONTEXT_KEYS;
use devkit_config::{Config, FieldDecl, TrackerKind};
use serde::Serialize;

use super::{
    receipt,
    review::{missing_required, render_review, with_fields},
};
use crate::template::{FieldArgs, VarArgs};

#[derive(Debug, Serialize)]
pub(crate) struct Rendered {
    pub title: String,
    pub body: String,
}

/// Where the issue's title comes from.
#[derive(Clone, Copy)]
pub(crate) enum Title<'a> {
    /// Rendered from this input through the `ticket_title` template.
    Input(&'a str),
    /// The issue's current title, used as is: the `ticket_title` template is
    /// not rendered and its required args are not asked for.
    Kept(&'a str),
}

/// The required args the issue templates rendered for `title`, and for the
/// body when `body`, read that `given` does not supply.
pub(crate) fn missing(
    cfg: &Config,
    title: Title<'_>,
    body: bool,
    given: &BTreeMap<String, String>,
    caller: Caller,
) -> Result<Vec<Missing>> {
    missing_required(
        cfg,
        &rendered_templates(cfg, matches!(title, Title::Input(_)), body),
        ISSUE_CONTEXT_KEYS,
        given,
        caller,
    )
}

/// The names the issue templates read: the title template when `title`, and
/// the body template when `body`.
pub(crate) fn reads(cfg: &Config, title: bool, body: bool) -> Result<BTreeSet<String>> {
    devkit_common::template::undeclared(&rendered_templates(cfg, title, body))
}

fn rendered_templates(cfg: &Config, title: bool, body: bool) -> Vec<&str> {
    let tmpls = &cfg.templates;
    let mut rendered = Vec::new();
    if title {
        rendered.push(tmpls.ticket_title());
    }
    if body {
        rendered.push(tmpls.ticket_body());
    }
    rendered
}

fn title_context(title: &str) -> serde_json::Value {
    with_fields(&serde_json::json!({}), &[(
        "input",
        serde_json::json!(title),
    )])
}

fn body_context(body: &str, rendered_title: &str) -> serde_json::Value {
    with_fields(&serde_json::json!({}), &[
        ("input", serde_json::json!(body)),
        ("ticket_title", serde_json::json!(rendered_title)),
        ("issue_title", serde_json::json!(rendered_title)),
    ])
}

/// The tracker in use, `[tracker] kind` or else the one detected, and its
/// `[ticket.fields]` table, checked in full. `origin` names the layers for
/// that check.
pub(crate) fn tracker_fields<'a>(
    cfg: &'a Config,
    origin: &HashMap<String, PathBuf>,
    config: Option<&str>,
    start: &str,
) -> Result<(TrackerKind, &'a BTreeMap<String, FieldDecl>)> {
    let kind = cfg.tracker.kind.unwrap_or_else(|| {
        devkit_common::tracker::select(config.map(Path::new), start, None)
            .tracker
            .tracker
            .kind()
    });
    devkit_config::check_ticket_fields(cfg, kind, origin)?;
    Ok((kind, cfg.ticket.fields.for_tracker(kind)))
}

/// A ticket verb's `--arg`s, split into template variables and ticket fields.
pub(crate) struct TicketArgs {
    pub template: BTreeMap<String, String>,
    pub fields: Given,
}

/// Split `--arg`s between `[templates.variables]` and the declared fields,
/// refusing a name that is neither and a `--none` naming no field.
pub(crate) fn ticket_args(
    cfg: &Config,
    vars: &VarArgs,
    nones: &FieldArgs,
    decls: &BTreeMap<String, FieldDecl>,
) -> Result<TicketArgs> {
    let declared = cfg.templates.declared();
    let mut out = TicketArgs {
        template: BTreeMap::new(),
        fields: Given::default(),
    };
    for (k, v) in vars.parse()? {
        if decls.contains_key(&k) {
            out.fields.values.insert(k, v);
        } else if declared.contains(&k) {
            out.template.insert(k, v);
        } else {
            bail!("--arg `{k}` is not declared in [templates.variables] or [ticket.fields]");
        }
    }
    for name in &nones.none {
        if !decls.contains_key(name) {
            bail!("`--none {name}` names no declared ticket field in [ticket.fields]");
        }
        out.fields.nones.insert(name.clone());
    }
    Ok(out)
}

/// Validate `given` against the declarations, refusing with every problem.
pub(crate) fn resolve_fields(
    cfg: &Config,
    kind: TrackerKind,
    decls: &BTreeMap<String, FieldDecl>,
    given: &Given,
    naming: Naming,
) -> Result<Resolved> {
    let team = fields::team(decls, given);
    fields::resolve(decls, kind, given, &cfg.people, team.as_deref(), naming)
        .map_err(|problems| anyhow!("{}", fields::message(decls, &problems)))
}

/// Each declared field as the templates read it: its values' names joined
/// by `, `, or the empty string when it has none.
pub(crate) fn shown(
    decls: &BTreeMap<String, FieldDecl>,
    resolved: &Resolved,
) -> BTreeMap<String, String> {
    decls
        .keys()
        .map(|name| {
            let text = resolved
                .fields
                .get(name)
                .map(|vs| vs.iter().map(|v| v.name()).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            (name.clone(), text)
        })
        .collect()
}

/// Refuse on a missing required arg, naming `surface`, then render the title
/// and, unless `body` is `None`, the body; with `None` the body template is
/// neither rendered nor asked for, and the body comes back empty. `fields` is
/// what [`shown`] gives the templates.
pub(crate) fn render(
    cfg: &Config,
    surface: &str,
    title: Title<'_>,
    body: Option<&str>,
    given: &BTreeMap<String, String>,
    fields: &BTreeMap<String, String>,
    caller: Caller,
) -> Result<Rendered> {
    let tmpls = &cfg.templates;
    devkit_common::required::ensure_supplied(
        surface,
        &missing(cfg, title, body.is_some(), given, caller)?,
    )?;
    let mut values = tmpls.defaults();
    values.extend(fields.clone());
    values.extend(given.clone());

    let title = match title {
        Title::Input(input) => render_review(
            tmpls.ticket_title(),
            "ticket_title",
            &title_context(input),
            &values,
            None,
        )?,
        Title::Kept(current) => current.to_string(),
    };
    if title.trim().is_empty() {
        bail!("--title is required: the `ticket_title` template rendered empty");
    }
    let body = match body {
        Some(body) => render_review(
            tmpls.ticket_body(),
            "ticket_body",
            &body_context(body, &title),
            &values,
            None,
        )?,
        None => String::new(),
    };
    Ok(Rendered { title, body })
}

pub(crate) struct RenderArgs {
    pub title: String,
    pub body: Option<String>,
    pub vars: VarArgs,
    pub dir: Option<String>,
    pub config: Option<String>,
}

pub(crate) fn run(args: RenderArgs) -> Result<()> {
    let start = super::start(&args.dir);
    let loaded =
        devkit_ports::load::load(args.config.as_deref().map(Path::new), Path::new(&start))?;
    let cfg = &loaded.config;
    let (kind, decls) = tracker_fields(
        cfg,
        &loaded.provenance.origin,
        args.config.as_deref(),
        &start,
    )?;
    let input = ticket_args(cfg, &args.vars, &FieldArgs::default(), decls)?;
    let resolved = resolve_fields(cfg, kind, decls, &input.fields, Naming::Tracker)?;
    let rendered = render(
        cfg,
        "ticket render",
        Title::Input(&args.title),
        Some(args.body.as_deref().unwrap_or_default()),
        &input.template,
        &shown(decls, &resolved),
        devkit_common::caller::caller(),
    )?;

    receipt::record(
        Path::new(&start),
        receipt::Kind::Issue,
        &rendered.title,
        &rendered.body,
    )?;
    println!("{}", serde_json::to_string(&rendered)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn keys(v: &serde_json::Value) -> BTreeSet<String> {
        v.as_object().unwrap().keys().cloned().collect()
    }

    #[test]
    fn the_issue_context_keys_match_what_render_builds() {
        let expected: BTreeSet<String> = ISSUE_CONTEXT_KEYS.iter().map(|k| k.to_string()).collect();
        let mut built = keys(&title_context("t"));
        built.extend(keys(&body_context("b", "t")));
        assert_eq!(built, expected);
    }
}
