//! `ticket render`: a ticket's title and body from the `ticket_title` and
//! `ticket_body` templates, printed for a tracker MCP call, with a receipt per
//! field the pre-tool-use hook checks that call against.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Result, anyhow, bail};
use devkit_common::{
    caller::Caller,
    fields::{self, Given, Resolved},
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

/// The required args the issue templates rendered for `title` read that
/// `given` does not supply.
pub(crate) fn missing(
    cfg: &Config,
    title: Title<'_>,
    given: &BTreeMap<String, String>,
    caller: Caller,
) -> Result<Vec<Missing>> {
    let tmpls = &cfg.templates;
    let rendered: &[&str] = match title {
        Title::Input(_) => &[tmpls.ticket_title(), tmpls.ticket_body()],
        Title::Kept(_) => &[tmpls.ticket_body()],
    };
    missing_required(cfg, rendered, ISSUE_CONTEXT_KEYS, given, caller)
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

/// The tracker whose `[ticket.fields]` table applies: `kind` when the caller
/// resolved one, else `[tracker] kind`, else the one table declared. Both
/// tables declared with no kind set is refused, since the tracker in use
/// decides which fields exist.
pub(crate) fn fields_tracker(cfg: &Config, kind: Option<TrackerKind>) -> Result<TrackerKind> {
    if let Some(kind) = kind.or(cfg.tracker.kind) {
        return Ok(kind);
    }
    let f = &cfg.ticket.fields;
    match (f.github.is_empty(), f.linear.is_empty()) {
        (false, true) => Ok(TrackerKind::Github),
        (true, false) => Ok(TrackerKind::Linear),
        (true, true) => Ok(TrackerKind::None),
        (false, false) => bail!(
            "[ticket.fields] declares fields for both github and linear; set [tracker] kind so \
             devkit knows which apply"
        ),
    }
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
) -> Result<Resolved> {
    let team = fields::team(decls, given);
    fields::resolve(decls, kind, given, &cfg.people, team.as_deref())
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
/// and the body. `fields` is what [`shown`] gives the templates.
pub(crate) fn render(
    cfg: &Config,
    surface: &str,
    title: Title<'_>,
    body: &str,
    given: &BTreeMap<String, String>,
    fields: &BTreeMap<String, String>,
    caller: Caller,
) -> Result<Rendered> {
    let tmpls = &cfg.templates;
    devkit_common::required::ensure_supplied(surface, &missing(cfg, title, given, caller)?)?;
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
    let body = render_review(
        tmpls.ticket_body(),
        "ticket_body",
        &body_context(body, &title),
        &values,
        None,
    )?;
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
    let kind = fields_tracker(cfg, None)?;
    let decls = cfg.ticket.fields.for_tracker(kind);
    let input = ticket_args(cfg, &args.vars, &FieldArgs::default(), decls)?;
    let resolved = resolve_fields(cfg, kind, decls, &input.fields)?;
    let rendered = render(
        cfg,
        "ticket render",
        Title::Input(&args.title),
        args.body.as_deref().unwrap_or_default(),
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
