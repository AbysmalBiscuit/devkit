//! `issue render`: an issue's title and body from the `issue_title` and
//! `issue_body` templates, printed for a tracker MCP call, with a receipt per
//! field the pre-tool-use hook checks that call against.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Result, bail};
use devkit_common::{caller::Caller, required::Missing};
use devkit_config::Config;
use serde::Serialize;

use super::{
    receipt,
    review::{parse_args, render_review, with_fields},
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

/// The required args the two issue templates read that `given` does not
/// supply.
pub(crate) fn missing(
    cfg: &Config,
    given: &BTreeMap<String, String>,
    caller: Caller,
) -> Result<Vec<Missing>> {
    let tmpls = &cfg.templates;
    let declared = tmpls.declared();
    let mut reads =
        devkit_common::template::undeclared(&[tmpls.issue_title(), tmpls.issue_body()])?;
    reads.retain(|n| declared.contains(n) && !ISSUE_CONTEXT_KEYS.contains(&n.as_str()));
    Ok(devkit_common::required::missing_args(
        cfg, None, &reads, given, caller,
    ))
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
    title: &str,
    body: &str,
    vars: &VarArgs,
    caller: Caller,
) -> Result<Rendered> {
    let tmpls = &cfg.templates;
    let given = parse_args(vars, &tmpls.declared())?;
    devkit_common::required::ensure_supplied(surface, &missing(cfg, &given, caller)?)?;
    let mut values = tmpls.defaults();
    values.extend(given);

    let title = render_review(
        tmpls.issue_title(),
        "issue_title",
        &title_context(title),
        &values,
        None,
    )?;
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
        "issue render",
        &args.title,
        args.body.as_deref().unwrap_or_default(),
        &args.vars,
        devkit_common::caller::caller(),
    )?;

    let sessions = receipt::sessions_from_env();
    if sessions.is_empty() {
        eprintln!("no agent session: no receipt written");
    } else {
        let checkout = devkit_common::git::checkout_root(Path::new(&start))?;
        let _ = receipt::sweep_stale(&checkout, receipt::STALE_AFTER);
        for session in &sessions {
            receipt::write(&checkout, session, &rendered.title, &rendered.body)?;
        }
    }
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
