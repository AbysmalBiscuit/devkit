//! `issue pr render`: the PR title and body `issue pr create` would send,
//! printed for a PR opened some other way, with a receipt per agent session.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Result, bail};
use devkit_common::{
    caller::Caller,
    record::IssueRecord,
    required::Missing,
    vcs::{Vcs, VersionControl},
};
use devkit_config::{Config, Templates};
use devkit_ports::templates::worktree_context;

use crate::{
    issue::{
        receipt,
        render::Rendered,
        review::{
            PR_CONTEXT_KEYS, check_required, missing_required, parse_args, render_review,
            with_fields,
        },
    },
    template::VarArgs,
};

/// Reject an empty rendered PR title. Opening or rendering a PR needs one;
/// reusing an open PR does not.
pub(super) fn require_pr_title(title: &str) -> Result<()> {
    if title.trim().is_empty() {
        bail!("--pr-title is required: the pr_title template rendered empty");
    }
    Ok(())
}

/// The required args the `pr_title` and `pr_body` templates read that `given`
/// does not supply.
pub(crate) fn missing(
    cfg: &Config,
    given: &BTreeMap<String, String>,
    caller: Caller,
) -> Result<Vec<Missing>> {
    let tmpls = &cfg.templates;
    missing_required(
        cfg,
        &[tmpls.pr_title(), tmpls.pr_body()],
        PR_CONTEXT_KEYS,
        given,
        caller,
    )
}

/// The values a PR's templates render with: the declared defaults under
/// `vars`. A required one the caller did not pass is refused, naming
/// `surface`, before either template renders.
pub(crate) fn values(
    surface: &str,
    cfg: &Config,
    vars: &VarArgs,
    caller: Caller,
) -> Result<BTreeMap<String, String>> {
    let tmpls = &cfg.templates;
    let given = parse_args(vars, &tmpls.declared())?;
    check_required(
        surface,
        cfg,
        &[tmpls.pr_title(), tmpls.pr_body()],
        PR_CONTEXT_KEYS,
        &given,
        caller,
    )?;
    let mut values = tmpls.defaults();
    values.extend(given);
    Ok(values)
}

/// Everything the `pr_title` and `pr_body` templates render from, for one
/// worktree and one set of arguments.
pub(crate) struct Texts<'a> {
    tmpls: &'a Templates,
    ctx: serde_json::Value,
    values: BTreeMap<String, String>,
    /// The worktree to name when a template fails for want of an
    /// `issue setup` record.
    missing_record_at: Option<String>,
    title_input: String,
    body_input: String,
}

impl<'a> Texts<'a> {
    pub(crate) fn new(
        tmpls: &'a Templates,
        record: Option<&IssueRecord>,
        branch: &str,
        toplevel: &Path,
        values: BTreeMap<String, String>,
        title_input: Option<String>,
        body_input: Option<String>,
    ) -> Self {
        Self {
            tmpls,
            ctx: worktree_context(record, Some(branch)),
            values,
            missing_record_at: record
                .is_none()
                .then(|| toplevel.to_string_lossy().into_owned()),
            title_input: title_input.unwrap_or_default(),
            body_input: body_input.unwrap_or_default(),
        }
    }

    pub(crate) fn title(&self) -> Result<String> {
        let ctx = with_fields(&self.ctx, &[("input", serde_json::json!(self.title_input))]);
        render_review(
            self.tmpls.pr_title(),
            "pr_title",
            &ctx,
            &self.values,
            self.missing_record_at.as_deref(),
        )
    }

    pub(crate) fn body(&self, title: &str) -> Result<String> {
        let ctx = with_fields(&self.ctx, &[
            ("input", serde_json::json!(self.body_input)),
            ("pr_title", serde_json::json!(title)),
        ]);
        render_review(
            self.tmpls.pr_body(),
            "pr_body",
            &ctx,
            &self.values,
            self.missing_record_at.as_deref(),
        )
    }
}

pub(crate) struct Args {
    pub pr_title: Option<String>,
    pub pr_body: Option<String>,
    pub vars: VarArgs,
    pub dir: Option<String>,
    pub config: Option<String>,
}

pub(crate) fn run(args: Args) -> Result<()> {
    let start = args.dir.unwrap_or_else(|| ".".to_string());
    let here = Path::new(&start);
    let loaded = devkit_ports::load::load(args.config.as_deref().map(Path::new), here)?;
    let values = values(
        "issue pr render",
        &loaded.config,
        &args.vars,
        devkit_common::caller::caller(),
    )?;

    let branch = Vcs::at(here).branch(here)?;
    let toplevel = devkit_common::vcs::checkout_root(here)?;
    let record = devkit_common::record::read(&toplevel);
    let texts = Texts::new(
        &loaded.config.templates,
        record.as_ref(),
        &branch,
        &toplevel,
        values,
        args.pr_title,
        args.pr_body,
    );
    let title = texts.title()?;
    require_pr_title(&title)?;
    let body = texts.body(&title)?;

    receipt::record(here, receipt::Kind::Pr, &title, &body)?;
    println!("{}", serde_json::to_string(&Rendered { title, body })?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_pr_title_rejects_empty() {
        assert!(require_pr_title("  ").is_err());
        assert!(require_pr_title("Fix login").is_ok());
    }
}
