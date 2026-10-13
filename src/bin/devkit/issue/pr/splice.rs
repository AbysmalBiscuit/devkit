//! Placing one template variable in a PR body that has drifted from what the
//! `pr_body` template rendered. Bots and people add to a live body, so only
//! the span the variable owns is rewritten.
//!
//! The template is rendered once with every variable as its [`slot`]. That
//! skeleton's anchors, the lines carrying any of the template's own text, are
//! found in the live body in order, and the ones on either side of a
//! variable's slot bound the span it owns there.

use anyhow::{Result, bail};

const OPEN: &str = "@@devkit-slot:";
const CLOSE: &str = "@@";

/// What variable `name` renders as in a skeleton.
pub(crate) fn slot(name: &str) -> String {
    format!("{OPEN}{name}{CLOSE}")
}

/// `live` with the span variable `name` owns replaced by `value`, or, when
/// the live body lacks the template text that opens that span, with the
/// span inserted where the template puts it.
pub(crate) fn splice(skeleton: &str, live: &str, name: &str, value: &str) -> Result<String> {
    let marker = slot(name);
    let sk = lines(skeleton);
    let at = slot_line(&sk, &marker, name)?;
    let body = lines(live);

    let anchors: Vec<usize> = (0..sk.len()).filter(|&i| is_anchor(sk[i].text)).collect();
    let rows: Vec<usize> = (0..body.len()).collect();
    let earliest = align(&sk, &anchors, &body, &rows);
    let reversed = |v: &[usize]| v.iter().rev().copied().collect::<Vec<_>>();
    let latest = align(&sk, &reversed(&anchors), &body, &reversed(&rows));
    let row_of =
        |pairs: &[(usize, usize)], i: usize| pairs.iter().find(|(s, _)| *s == i).map(|(_, l)| *l);
    let found = |i: usize| row_of(&earliest, i);
    let Some(before) = anchors.iter().rev().copied().find(|&i| i < at) else {
        bail!(unbounded(name, "before"));
    };
    let Some(after) = anchors.iter().copied().find(|&i| i > at) else {
        bail!(unbounded(name, "after"));
    };
    if let Some(i) = [before, after]
        .into_iter()
        .find(|&i| found(i) != row_of(&latest, i))
    {
        bail!(
            "cannot tell where `{name}` goes: the body has the pr_body template's \
             line `{}` in more than one place that fits",
            sk[i].text
        );
    }

    let span_start = sk[before].end;
    let span_end = sk[after].start;
    if let Some(other) = sk
        .iter()
        .filter(|l| l.start >= span_start && l.end <= span_end && l.start != sk[at].start)
        .find_map(|l| slot_name(l.text))
    {
        bail!(
            "`{name}` shares its section of the pr_body template with `{other}`, \
             so it cannot be replaced alone"
        );
    }

    let Some(end) = found(after).map(|l| body[l].start) else {
        bail!(lost(name));
    };
    let value = value.trim();
    let (start, opens) = match found(before) {
        Some(l) => (body[l].end, span_start),
        None => (
            end,
            sk[section_start(&sk, before, |k| found(k).is_some())].start,
        ),
    };
    let text = skeleton[opens..span_end].replace(&marker, value);
    Ok(format!("{}{text}{}", &live[..start], &live[end..]))
}

fn lost(name: &str) -> String {
    format!(
        "cannot find where `{name}` goes: the body no longer has the pr_body \
         template's lines around it"
    )
}

/// Text above or below a variable that no template line bounds cannot be
/// told apart from what bots and people add there.
fn unbounded(name: &str, side: &str) -> String {
    format!(
        "`{name}` has no line of the pr_body template {side} it, so its text \
         cannot be told from what was added {side} it"
    )
}

/// One line of a text and where it sits, its newline included.
struct Line<'a> {
    text: &'a str,
    start: usize,
    end: usize,
}

fn lines(s: &str) -> Vec<Line<'_>> {
    let mut start = 0;
    s.split_inclusive('\n')
        .map(|raw| {
            let line = Line {
                text: raw.trim_end(),
                start,
                end: start + raw.len(),
            };
            start = line.end;
            line
        })
        .collect()
}

/// The template's own text on a skeleton line, split around each slot.
fn segments(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find(OPEN) {
        out.push(&rest[..open]);
        let after = &rest[open + OPEN.len()..];
        rest = after
            .find(CLOSE)
            .map_or("", |close| &after[close + CLOSE.len()..]);
    }
    out.push(rest);
    out
}

/// Whether a skeleton line carries any of the template's own text, and so
/// can be found in the live body. A line that is nothing but slots cannot.
fn is_anchor(text: &str) -> bool {
    segments(text).iter().any(|s| !s.trim().is_empty())
}

/// Whether live line `text` is what skeleton line `pattern` renders to: its
/// own text in order, with anything where its slots are.
fn fits(pattern: &str, text: &str) -> bool {
    let parts = segments(pattern);
    let (first, after_first) = parts.split_first().expect("a line has a first segment");
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let Some((last, middle)) = after_first.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

fn slot_name(text: &str) -> Option<&str> {
    let rest = &text[text.find(OPEN)? + OPEN.len()..];
    rest.find(CLOSE).map(|end| &rest[..end])
}

/// The skeleton line that is `marker` and nothing else.
fn slot_line(sk: &[Line<'_>], marker: &str, name: &str) -> Result<usize> {
    let mut holding = sk
        .iter()
        .enumerate()
        .filter(|(_, l)| l.text.contains(marker));
    let Some((at, line)) = holding.next() else {
        bail!("`{name}` is not in the pr_body template");
    };
    if line.text.trim() != marker || holding.next().is_some() {
        bail!(
            "`{name}` does not render on a line of its own in the pr_body \
             template, so it cannot be replaced alone"
        );
    }
    Ok(at)
}

/// The skeleton's anchors paired with the live lines they fit, as many as
/// can be paired in order: a longest common subsequence, taken in the order
/// `anchors` and `rows` walk. Matching greedily would pair a separator the
/// live body lacks with a later one, and lose every anchor in between.
fn align(
    sk: &[Line<'_>],
    anchors: &[usize],
    body: &[Line<'_>],
    rows: &[usize],
) -> Vec<(usize, usize)> {
    let (n, m) = (anchors.len(), rows.len());
    let same = |i: usize, j: usize| fits(sk[anchors[i]].text, body[rows[j]].text);
    let mut rest = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            rest[i][j] = if same(i, j) {
                rest[i + 1][j + 1] + 1
            } else {
                rest[i + 1][j].max(rest[i][j + 1])
            };
        }
    }
    let (mut i, mut j, mut pairs) = (0, 0, Vec::new());
    while i < n && j < m {
        if same(i, j) && rest[i][j] == rest[i + 1][j + 1] + 1 {
            pairs.push((anchors[i], rows[j]));
            i += 1;
            j += 1;
        } else if rest[i + 1][j] >= rest[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

/// The first line of the section `heading` opens, when none of it is in the
/// live body: the earliest of the unmatched template lines run together above
/// it, so a section opened by several lines is inserted whole.
fn section_start(sk: &[Line<'_>], heading: usize, matched: impl Fn(usize) -> bool) -> usize {
    let mut opens = heading;
    for k in (0..heading).rev() {
        let line = &sk[k];
        if line.text.is_empty() {
            continue;
        }
        if !is_anchor(line.text) || matched(k) {
            break;
        }
        opens = k;
    }
    opens
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKELETON: &str = "\
## TL;DR
@@devkit-slot:human_tldr@@

---

@@devkit-slot:refs@@

---

## Summary

@@devkit-slot:input@@

## Proof

@@devkit-slot:proof@@

---

Footer by @@devkit-slot:agent_model@@";

    /// A body with no refs has one separator fewer than the skeleton, so a
    /// greedy match would pair the skeleton's first `---` with the footer's.
    #[test]
    fn a_separator_the_body_lacks_does_not_swallow_the_sections_after_it() {
        let live = "## TL;DR\n\n---\n\n## Summary\n\nold\n\n## Proof\n\np\n\n---\n\nFooter by X\n";

        let got = splice(SKELETON, live, "proof", "new").unwrap();

        assert_eq!(got, live.replace("\np\n", "\nnew\n"));
    }

    /// GitHub stores a body edited in its web UI with CRLF line endings.
    #[test]
    fn crlf_line_endings_still_match() {
        let live = "## Summary\r\n\r\nold\r\n\r\n## Proof\r\n\r\np\r\n\r\n---\r\n\r\nFooter\r\n";

        let got = splice(SKELETON, live, "input", "new").unwrap();

        assert_eq!(
            got,
            "## Summary\r\n\nnew\n\n## Proof\r\n\r\np\r\n\r\n---\r\n\r\nFooter\r\n"
        );
    }

    /// With no template line after it, a variable's text cannot be told from
    /// what a bot appended below it, and replacing to the end would delete
    /// that. The same holds above a variable with no template line before it.
    #[test]
    fn a_variable_at_either_end_of_the_template_is_refused() {
        let tail = "## Proof\n\n@@devkit-slot:proof@@";
        let live = "## Proof\n\np\n\n## Walkthrough\nbot text\n";
        let err = splice(tail, live, "proof", "new").unwrap_err();
        assert!(err.to_string().contains("after it"), "{err}");

        let head = "@@devkit-slot:input@@\n\n## Proof\n\n@@devkit-slot:proof@@\n\n---";
        let live = "> bot banner\n\nsummary\n\n## Proof\n\np\n\n---\n";
        let err = splice(head, live, "input", "new").unwrap_err();
        assert!(err.to_string().contains("before it"), "{err}");
    }

    #[test]
    fn an_inline_variable_is_refused() {
        let err = splice(SKELETON, "Footer by X", "agent_model", "Y").unwrap_err();
        assert!(err.to_string().contains("line of its own"), "{err}");
    }

    #[test]
    fn a_section_holding_two_variables_is_refused() {
        let skeleton =
            "## Merge danger\n\n@@devkit-slot:door@@\n\n@@devkit-slot:blast_radius@@\n\n---\n";
        let err = splice(skeleton, "## Merge danger\n\na\n\nb\n\n---\n", "door", "x").unwrap_err();
        assert!(err.to_string().contains("`blast_radius`"), "{err}");
    }
}
