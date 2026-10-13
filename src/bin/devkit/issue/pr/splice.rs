//! Placing one template variable in a PR body that has drifted from what the
//! `pr_body` template rendered. Bots and people add to a live body, so only
//! the span the variable owns is rewritten.
//!
//! The template is rendered once with every variable as its [`slot`]. That
//! skeleton's lines with no slot are the template's own text: they are found
//! in the live body, in order, and the ones on either side of a variable's
//! slot bound the span it owns there.

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

    let literals: Vec<usize> = (0..sk.len()).filter(|&i| is_literal(&sk[i])).collect();
    let matched = align(&sk, &literals, &body);
    let found = |i: usize| matched.iter().find(|(s, _)| *s == i).map(|(_, l)| *l);
    let before = literals.iter().rev().copied().find(|&i| i < at);
    let after = literals.iter().copied().find(|&i| i > at);

    let span_start = before.map_or(0, |i| sk[i].end);
    let span_end = after.map_or(skeleton.len(), |i| sk[i].start);
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

    let end = match after {
        None => live.len(),
        Some(i) => match found(i) {
            Some(l) => body[l].start,
            None => bail!(lost(name)),
        },
    };
    let value = value.trim();
    match before {
        None => {
            let text = skeleton[..span_end].replace(&marker, value);
            Ok(format!("{text}{}", &live[end..]))
        }
        Some(i) => match found(i) {
            Some(l) => {
                let text = skeleton[span_start..span_end].replace(&marker, value);
                Ok(format!("{}{text}{}", &live[..body[l].end], &live[end..]))
            }
            None if after.is_some() => {
                let opens = section_start(&sk, i, |k| found(k).is_some());
                let text = skeleton[sk[opens].start..span_end].replace(&marker, value);
                Ok(format!("{}{text}{}", &live[..end], &live[end..]))
            }
            None => bail!(lost(name)),
        },
    }
}

fn lost(name: &str) -> String {
    format!(
        "cannot find where `{name}` goes: the body no longer has the pr_body \
         template's lines around it"
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

fn is_literal(line: &Line<'_>) -> bool {
    !line.text.is_empty() && !line.text.contains(OPEN)
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

/// The skeleton's literal lines paired with the live lines they match, as
/// many as can be matched in order: a longest common subsequence. Matching
/// greedily would pair a separator the live body lacks with a later one,
/// and lose every anchor in between.
fn align(sk: &[Line<'_>], literals: &[usize], body: &[Line<'_>]) -> Vec<(usize, usize)> {
    let (n, m) = (literals.len(), body.len());
    let same = |i: usize, j: usize| sk[literals[i]].text == body[j].text;
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
            pairs.push((literals[i], j));
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
        if !is_literal(line) || matched(k) {
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

    #[test]
    fn an_inline_variable_is_refused() {
        let err = splice(SKELETON, "Footer by X", "agent_model", "Y").unwrap_err();
        assert!(err.to_string().contains("line of its own"), "{err}");
    }

    #[test]
    fn a_section_holding_two_variables_is_refused() {
        let skeleton = "## Merge danger\n\n@@devkit-slot:door@@\n\n@@devkit-slot:blast_radius@@\n";
        let err = splice(skeleton, "## Merge danger\n\na\n\nb\n", "door", "x").unwrap_err();
        assert!(err.to_string().contains("`blast_radius`"), "{err}");
    }
}
