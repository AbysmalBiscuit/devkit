//! `pr render`: the PR title and body `pr create` would send,
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
        review::{PR_CONTEXT_KEYS, missing_required, parse_args, render_review, with_fields},
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

/// Refuse a summary that writes a heading the `pr_body` template emits itself
/// on any of its branches: the body would carry that heading twice, and a
/// section the template keeps for someone else, such as a human's TL;DR,
/// would be filled from the summary. Taken from the template source rather
/// than a render, since a branch this caller's render skips still emits its
/// heading for another caller.
pub(crate) fn refuse_template_headings(tmpls: &Templates, summary: &str) -> Result<()> {
    let emitted = template_headings(tmpls.pr_body());
    let repeated: Vec<String> = summary_headings(summary)
        .into_iter()
        .filter(|h| emitted.contains(h))
        .map(|h| format!("`{h}`"))
        .collect();
    if repeated.is_empty() {
        return Ok(());
    }
    bail!(
        "--pr-body writes {}, a heading the pr_body template emits itself. Drop it from \
         the summary: the template fills its own sections from their --arg values \
         (`devkit template show pr_body` lists them)",
        repeated.join(", ")
    )
}

/// The headings a template's literal text holds, across every branch. A
/// heading line whose text runs on into an expression is skipped, since its
/// full text is known only once rendered. Text right after a tag counts as a
/// line start, because the tag may render a newline.
fn template_headings(source: &str) -> Vec<String> {
    let mut headings = Vec::new();
    let mut rest = source;
    loop {
        let open = ["{{", "{%", "{#"].iter().filter_map(|o| rest.find(o)).min();
        let (text, tag) = match open {
            Some(i) => (&rest[..i], Some(&rest[i..])),
            None => (rest, None),
        };
        let runs_into_expression = tag.is_some_and(|t| t.starts_with("{{"));
        let lines: Vec<&str> = text.split('\n').collect();
        for (i, line) in lines.iter().enumerate() {
            if i + 1 == lines.len() && runs_into_expression {
                continue;
            }
            headings.extend(heading(line));
        }
        let Some(tag) = tag else { break };
        let close = match &tag[..2] {
            "{{" => "}}",
            "{%" => "%}",
            _ => "#}",
        };
        match tag[2..].find(close) {
            Some(j) => rest = &tag[2 + j + close.len()..],
            None => break,
        }
    }
    headings
}

/// The ATX headings of a markdown summary, outside its fenced code blocks.
fn summary_headings(summary: &str) -> Vec<String> {
    let mut fenced = false;
    let mut headings = Vec::new();
    for line in summary.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
        } else if !fenced {
            headings.extend(heading(line));
        }
    }
    headings
}

/// `line` as an ATX heading, its marker and text separated by one space, or
/// `None` when it is not one. Indentation is ignored: a template that trims
/// its input turns an indented first line into a heading.
fn heading(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let level = trimmed.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let text = trimmed[level..].strip_prefix([' ', '\t'])?.trim();
    (!text.is_empty()).then(|| format!("{} {text}", &trimmed[..level]))
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
    devkit_common::required::ensure_supplied(surface, &missing(cfg, &given, caller)?)?;
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
    /// `workspace setup` record.
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
        "pr render",
        &loaded.config,
        &args.vars,
        devkit_common::caller::caller(),
    )?;
    if let Some(summary) = &args.pr_body {
        refuse_template_headings(&loaded.config.templates, summary)?;
    }

    let branch = Vcs::at(here).branch(here)?;
    let toplevel = devkit_common::vcs::checkout_root(here)?;
    let record = devkit_common::record::read_on(&toplevel, &branch);
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
