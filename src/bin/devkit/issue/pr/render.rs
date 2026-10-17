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

/// The headings a template's literal text and string literals hold, across
/// every branch. A heading line whose text runs on into an expression or a
/// concatenation is skipped, since its full text is known only once rendered.
/// Text right after a tag counts as a line start, because the tag may render
/// a newline.
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
        collect_lines(text, false, runs_into_expression, &mut headings);
        let Some(tag) = tag else { break };
        match tag_end(tag, &mut headings) {
            Some(after) => rest = after,
            None => break,
        }
    }
    headings
}

/// The source after the tag `tag` opens, collecting the headings its string
/// literals hold, or `None` when the tag never closes. Comments hold none.
fn tag_end<'a>(tag: &'a str, headings: &mut Vec<String>) -> Option<&'a str> {
    let body = &tag[2..];
    if tag.starts_with("{#") {
        return body.find("#}").map(|j| &body[j + 2..]);
    }
    let close = if tag.starts_with("{{") { "}}" } else { "%}" };
    let bytes = body.as_bytes();
    let mut joined = false;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(close.as_bytes()) {
            return Some(&body[i + close.len()..]);
        }
        match bytes[i] {
            quote @ (b'"' | b'\'') => {
                let (literal, end) = string_literal(&body[i + 1..], quote)?;
                i += 1 + end;
                let joined_after = body[i..].trim_start().starts_with(['~', '+']);
                collect_lines(&literal, joined, joined_after, headings);
                joined = false;
                continue;
            }
            b'~' | b'+' => joined = true,
            b if b.is_ascii_whitespace() => {}
            _ => joined = false,
        }
        i += 1;
    }
    None
}

/// The unescaped text of a string literal whose opening `quote` precedes
/// `rest`, and the byte length of the literal's remainder, closing quote
/// included. `None` when it never closes.
fn string_literal(rest: &str, quote: u8) -> Option<(String, usize)> {
    let mut text = String::new();
    let mut chars = rest.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => match chars.next()?.1 {
                'n' => text.push('\n'),
                't' => text.push('\t'),
                'r' => text.push('\r'),
                escaped => text.push(escaped),
            },
            c if c == char::from(quote) => return Some((text, i + 1)),
            c => text.push(c),
        }
    }
    None
}

/// Push the headings among `text`'s lines, skipping the first when it
/// continues earlier text and the last when it runs on into later text.
fn collect_lines(text: &str, joined_before: bool, joined_after: bool, out: &mut Vec<String>) {
    let lines: Vec<&str> = text.split('\n').collect();
    for (i, line) in lines.iter().enumerate() {
        if (i == 0 && joined_before) || (i + 1 == lines.len() && joined_after) {
            continue;
        }
        out.extend(heading(line));
    }
}

/// The ATX and setext headings of a markdown summary, outside its fenced and
/// indented code blocks. Its first line's indentation is ignored, since a
/// template that trims its input turns an indented first line into a heading.
fn summary_headings(summary: &str) -> Vec<String> {
    let mut headings = Vec::new();
    let mut fence: Option<(u8, usize)> = None;
    let mut paragraph: Vec<&str> = Vec::new();
    let mut in_container = false;
    for line in summary.trim_start().lines() {
        if let Some((marker, len)) = fence {
            if unindented(line).is_some_and(|t| closes_fence(t, marker, len)) {
                fence = None;
            }
            continue;
        }
        let Some(t) = unindented(line) else {
            if !paragraph.is_empty() {
                paragraph.push(line.trim());
            }
            continue;
        };
        if t.trim().is_empty() {
            paragraph.clear();
            in_container = false;
            continue;
        }
        if let Some(opened) = opens_fence(t) {
            fence = Some(opened);
            paragraph.clear();
        } else if let Some(level) = setext_level(t).filter(|_| !paragraph.is_empty()) {
            headings.push(format!("{} {}", "#".repeat(level), paragraph.join(" ")));
            paragraph.clear();
        } else if let Some(h) = heading(t) {
            headings.push(h);
            paragraph.clear();
        } else if is_thematic_break(t) {
            paragraph.clear();
        } else if opens_container(t) {
            paragraph.clear();
            in_container = true;
        } else if !in_container {
            paragraph.push(t.trim());
        }
    }
    headings
}

/// `line` without its indentation, or `None` when it is indented four
/// columns or more, which makes it indented code.
fn unindented(line: &str) -> Option<&str> {
    let t = line.trim_start_matches(' ');
    let blank = t.trim().is_empty();
    (blank || (line.len() - t.len() <= 3 && !t.starts_with('\t'))).then_some(t)
}

/// The marker and length of the code fence `t` opens.
fn opens_fence(t: &str) -> Option<(u8, usize)> {
    let marker = *t.as_bytes().first().filter(|b| matches!(b, b'`' | b'~'))?;
    let len = t.bytes().take_while(|b| *b == marker).count();
    let info_ok = marker == b'~' || !t[len..].contains('`');
    (len >= 3 && info_ok).then_some((marker, len))
}

fn closes_fence(t: &str, marker: u8, len: usize) -> bool {
    let run = t.bytes().take_while(|b| *b == marker).count();
    run >= len && t[run..].trim().is_empty()
}

/// The heading level a setext underline (`===` or `---`) gives the
/// paragraph above it.
fn setext_level(t: &str) -> Option<usize> {
    let t = t.trim_end();
    if t.bytes().all(|b| b == b'=') {
        Some(1)
    } else if t.bytes().all(|b| b == b'-') {
        Some(2)
    } else {
        None
    }
}

fn is_thematic_break(t: &str) -> bool {
    let marks: Vec<u8> = t.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    marks.len() >= 3
        && matches!(marks[0], b'-' | b'*' | b'_')
        && marks.iter().all(|b| *b == marks[0])
}

/// Whether `t` opens a block quote or list item, whose own lines cannot
/// become a top-level setext heading.
fn opens_container(t: &str) -> bool {
    let after_marker = |rest: &str| rest.is_empty() || rest.starts_with([' ', '\t']);
    if t.starts_with('>') {
        return true;
    }
    if let Some(rest) = t.strip_prefix(['-', '*', '+']) {
        return after_marker(rest);
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    (1..=9).contains(&digits)
        && t[digits..]
            .strip_prefix(['.', ')'])
            .is_some_and(after_marker)
}

/// `line` as an ATX heading, its closing `#` run dropped, or `None` when it
/// is not one or has no text. Indentation is ignored; callers that care
/// about indented code strip it first.
fn heading(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let level = trimmed.bytes().take_while(|b| *b == b'#').count();
    let rest = &trimmed[level..];
    if !(1..=6).contains(&level) || !rest.starts_with([' ', '\t']) {
        return None;
    }
    let text = rest.trim();
    let open = text.trim_end_matches('#');
    let text = if open.is_empty() || open.ends_with([' ', '\t']) {
        open.trim_end()
    } else {
        text
    };
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

    fn headings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn template_headings_skip_comments() {
        let source = "{# ## Hidden #}\n## Shown\n{#\n## Also hidden\n#}";
        assert_eq!(template_headings(source), headings(&["## Shown"]));
    }

    #[test]
    fn template_headings_stop_at_an_unclosed_tag() {
        assert_eq!(
            template_headings("## Before\n{% if x \n## After"),
            headings(&["## Before"])
        );
    }

    #[test]
    fn template_headings_count_text_after_a_tag_as_a_line_start() {
        let source = "{%- if agent -%}## Agent summary{% endif %}";
        assert_eq!(template_headings(source), headings(&["## Agent summary"]));
    }

    #[test]
    fn template_headings_skip_a_heading_running_into_an_expression() {
        assert_eq!(
            template_headings("## Step {{ n }}\n## Done"),
            headings(&["## Done"])
        );
    }

    #[test]
    fn template_headings_strip_closing_hashes() {
        assert_eq!(
            template_headings("## Proof ##\n## C#"),
            headings(&["## Proof", "## C#"])
        );
    }

    #[test]
    fn template_headings_read_string_literals_in_tags() {
        let source = concat!(
            r###"{{- "\n\n## Rollout\n" if rollout }}"###,
            r###"{% set x = 'a\n## Quoted }}\n' %}"###,
            r###"{{ "## Step " ~ n }}{{ "}}" }}## Tail"###,
        );
        assert_eq!(
            template_headings(source),
            headings(&["## Rollout", "## Quoted }}", "## Tail"])
        );
    }

    #[test]
    fn summary_headings_strip_closing_hashes() {
        assert_eq!(
            summary_headings("Why.\n\n## Proof ##\n\n### Proof #####"),
            headings(&["## Proof", "### Proof"])
        );
    }

    #[test]
    fn summary_headings_close_a_fence_only_on_its_own_marker() {
        let summary = "~~~\n```\n## Inside\n~~~\n## Outside";
        assert_eq!(summary_headings(summary), headings(&["## Outside"]));
        let summary = "````\n```\n## Inside\n````\n## Outside";
        assert_eq!(summary_headings(summary), headings(&["## Outside"]));
    }

    #[test]
    fn summary_headings_read_setext_headings() {
        let summary = "Why.\n\nProof\n-----\n\nTitle\n===\n\n---\n\n- item\n---";
        assert_eq!(
            summary_headings(summary),
            headings(&["## Proof", "# Title"])
        );
    }

    #[test]
    fn summary_headings_skip_indented_code() {
        let summary = "Why.\n\n    ## Proof\n\n\t## Proof\n\n   ## Shown";
        assert_eq!(summary_headings(summary), headings(&["## Shown"]));
    }

    #[test]
    fn summary_headings_read_an_indented_first_line_the_template_trims() {
        assert_eq!(
            summary_headings("\n      ## Proof\n\nWhy."),
            headings(&["## Proof"])
        );
    }
}
