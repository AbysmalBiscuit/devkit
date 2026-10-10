//! `ticket render`: an issue's title and body from the `issue_title` and
//! `issue_body` templates, printed for a tracker MCP call, with a receipt per
//! field the pre-tool-use hook checks that call against.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Result, bail};
use devkit_common::{caller::Caller, required::Missing};
use devkit_config::Config;
use serde::Serialize;

use super::{
    receipt,
    review::{missing_required, parse_args, render_review, with_fields},
};
use crate::template::VarArgs;

/// The names the issue templates' render contexts supply, which no `--arg`
/// can. `the_issue_context_keys_match_what_render_builds` holds them against
/// the contexts.
pub(crate) const ISSUE_CONTEXT_KEYS: &[&str] = &["input", "issue_title"];

#[derive(Debug, Serialize)]
pub(crate) struct Rendered {
    pub title: String,
    pub body: String,
}

/// Where the issue's title comes from.
#[derive(Clone, Copy)]
pub(crate) enum Title<'a> {
    /// Rendered from this input through the `issue_title` template.
    Input(&'a str),
    /// The issue's current title, used as is: the `issue_title` template is
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
        Title::Input(_) => &[tmpls.issue_title(), tmpls.issue_body()],
        Title::Kept(_) => &[tmpls.issue_body()],
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
        ("issue_title", serde_json::json!(rendered_title)),
    ])
}

/// Refuse on a missing required arg, naming `surface`, then render the title
/// and the body.
pub(crate) fn render(
    cfg: &Config,
    surface: &str,
    title: Title<'_>,
    body: &str,
    vars: &VarArgs,
    caller: Caller,
) -> Result<Rendered> {
    let tmpls = &cfg.templates;
    let given = parse_args(vars, &tmpls.declared())?;
    devkit_common::required::ensure_supplied(surface, &missing(cfg, title, &given, caller)?)?;
    let mut values = tmpls.defaults();
    values.extend(given);

    let title = match title {
        Title::Input(input) => render_review(
            tmpls.issue_title(),
            "issue_title",
            &title_context(input),
            &values,
            None,
        )?,
        Title::Kept(current) => current.to_string(),
    };
    if title.trim().is_empty() {
        bail!("--title is required: the `issue_title` template rendered empty");
    }
    let body = render_review(
        tmpls.issue_body(),
        "issue_body",
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
    let rendered = render(
        &loaded.config,
        "ticket render",
        Title::Input(&args.title),
        args.body.as_deref().unwrap_or_default(),
        &args.vars,
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
