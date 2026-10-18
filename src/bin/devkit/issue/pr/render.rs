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
        .filter(|line| heading_key(line).is_some_and(|k| emitted.contains(&k)))
        .map(|line| format!("`{line}`"))
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

/// The keys of the ATX headings in a template's literal text, across every
/// branch. Each tag boundary counts as a line start, since a tag may render a
/// newline, and a heading line running into a `{{` expression is skipped, its
/// full text known only once rendered. A heading written only inside a string
/// literal in a tag is not seen.
fn template_headings(source: &str) -> Vec<String> {
    let source = without_comments(source);
    let mut headings = Vec::new();
    let mut rest = source.as_str();
    loop {
        let open = ["{{", "{%"].iter().filter_map(|o| rest.find(o)).min();
        let end = open.unwrap_or(rest.len());
        let expression = rest[end..].starts_with("{{");
        let text = match rest[..end].rsplit_once('\n') {
            Some((whole_lines, _)) if expression => whole_lines,
            None if expression => "",
            _ => &rest[..end],
        };
        headings.extend(text.split('\n').filter_map(heading_key));
        let Some(i) = open else { break };
        let close = if expression { "}}" } else { "%}" };
        let Some(j) = rest[i + 2..].find(close) else {
            break;
        };
        rest = &rest[i + j + 4..];
    }
    headings
}

/// `source` without its `{# ... #}` comments; an unclosed one runs to the end.
fn without_comments(source: &str) -> String {
    let mut out = String::new();
    let mut rest = source;
    while let Some(i) = rest.find("{#") {
        out.push_str(&rest[..i]);
        let Some(j) = rest[i + 2..].find("#}") else {
            return out;
        };
        rest = &rest[i + 2 + j + 2..];
    }
    out + rest
}

/// The ATX heading lines of a markdown summary, trimmed, outside its fenced
/// code blocks.
fn summary_headings(summary: &str) -> Vec<&str> {
    let mut headings = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for line in summary.lines().map(str::trim) {
        let run = line
            .chars()
            .next()
            .filter(|c| matches!(c, '`' | '~'))
            .map(|c| (c, line.chars().take_while(|x| *x == c).count()));
        match (fence, run) {
            (Some((c, len)), Some((d, n))) if c == d && n >= len && n == line.len() => fence = None,
            (Some(_), _) => {}
            (None, Some((c, n))) if n >= 3 => fence = Some((c, n)),
            (None, _) => headings.extend(heading_key(line).map(|_| line)),
        }
    }
    headings
}

/// What two ATX headings are compared by: their text without the closing `#`
/// run, lowercased, the level ignored. `None` when `line` is not one or has
/// no text.
fn heading_key(line: &str) -> Option<String> {
    let line = line.trim();
    let level = line.bytes().take_while(|b| *b == b'#').count();
    let rest = &line[level..];
    if !(1..=6).contains(&level) || !rest.starts_with([' ', '\t']) {
        return None;
    }
    let open = rest.trim_end_matches('#');
    let text = if open.ends_with([' ', '\t']) {
        open
    } else {
        rest
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_lowercase())
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

    fn keys(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn template_headings_skip_comments() {
        let source = "{# ## Hidden #}\n## Shown\n{#\n## Also hidden\n#}";
        assert_eq!(template_headings(source), keys(&["shown"]));
    }

    #[test]
    fn template_headings_stop_at_an_unclosed_tag() {
        assert_eq!(
            template_headings("## Before\n{% if x \n## After"),
            keys(&["before"])
        );
    }

    #[test]
    fn template_headings_count_text_after_a_tag_as_a_line_start() {
        let source = "{%- if agent -%}## Agent summary{% endif %}";
        assert_eq!(template_headings(source), keys(&["agent summary"]));
    }

    #[test]
    fn template_headings_skip_a_heading_running_into_an_expression() {
        assert_eq!(
            template_headings("## Step {{ n }}\n## Done"),
            keys(&["done"])
        );
    }

    #[test]
    fn summary_headings_skip_fenced_code() {
        let summary = "~~~\n```\n## Inside\n~~~\n## Outside\n````\n```\n## Inside\n````";
        assert_eq!(summary_headings(summary), vec!["## Outside"]);
    }

    #[test]
    fn heading_keys_ignore_case_level_and_closing_hashes() {
        for line in ["## Proof", "# proof", "### PROOF ##", "  ## Proof #####"] {
            assert_eq!(heading_key(line).as_deref(), Some("proof"), "{line}");
        }
        assert_eq!(heading_key("## C#").as_deref(), Some("c#"));
        for line in ["##Proof", "####### Proof", "## ##", "Proof"] {
            assert_eq!(heading_key(line), None, "{line}");
        }
    }
}
