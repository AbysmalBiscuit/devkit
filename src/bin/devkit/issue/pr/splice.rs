//! Placing one section of template variables in a PR body that has drifted
//! from what the `pr_body` template rendered. Bots and people add to a live
//! body, so only the span the section owns is rewritten.
//!
//! The template is rendered once with every variable as its [`slot`]. That
//! skeleton's anchors, the lines carrying any of the template's own text, are
//! found in the live body in order, and the ones on either side of a
//! variable's slot bound the span that it shares with every slot inside it.

use anyhow::{Result, bail};

const OPEN: &str = "@@devkit-slot:";
const CLOSE: &str = "@@";

/// What variable `name` renders as in a skeleton.
pub(crate) fn slot(name: &str) -> String {
    format!("{OPEN}{name}{CLOSE}")
}

/// The variables that render into the span `name` owns in the skeleton,
/// `name` first. Such a section is rendered whole, so an edit that is not
/// `given` every one of them is refused, naming the ones it left out.
pub(crate) fn section(
    skeleton: &str,
    name: &str,
    given: impl Fn(&str) -> bool,
) -> Result<Vec<String>> {
    let sk = lines(skeleton);
    let at = slot_line(&sk, &slot(name), name)?;
    let (before, after) = bounds(&sk, at, name)?;
    let mut names = vec![name.to_string()];
    for other in sk[before + 1..after]
        .iter()
        .flat_map(|l| slot_names(l.text))
    {
        if !names.iter().any(|n| n == other) {
            names.push(other.to_string());
        }
    }
    let left_out: Vec<String> = names
        .iter()
        .filter(|n| !given(n))
        .map(|n| format!("`{n}`"))
        .collect();
    if !left_out.is_empty() {
        bail!(
            "`{name}` shares its section of the pr_body template, so the edit must \
             also pass {}",
            left_out.join(", ")
        );
    }
    Ok(names)
}

/// `live` with the span variable `name` owns replaced by that span of
/// `rendered`, the template rendered with the section's values and every
/// other variable as its slot. When the live body lacks the template text that
/// opens the span, the span is inserted where the template puts it, unless
/// the section is `hidden`, and `live` is kept as it is.
pub(crate) fn splice(
    skeleton: &str,
    rendered: &str,
    live: &str,
    name: &str,
    hidden: bool,
) -> Result<String> {
    let sk = lines(skeleton);
    let at = slot_line(&sk, &slot(name), name)?;
    let (before, after) = bounds(&sk, at, name)?;
    let body = lines(live);

    let anchors: Vec<usize> = (0..sk.len()).filter(|&i| is_anchor(sk[i].text)).collect();
    let rows: Vec<usize> = (0..body.len()).collect();
    let earliest = align(&sk, &anchors, &body, &rows);
    let reversed = |v: &[usize]| v.iter().rev().copied().collect::<Vec<_>>();
    let latest = align(&sk, &reversed(&anchors), &body, &reversed(&rows));
    let row_of =
        |pairs: &[(usize, usize)], i: usize| pairs.iter().find(|(s, _)| *s == i).map(|(_, l)| *l);
    let found = |i: usize| row_of(&earliest, i);
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
    let Some(end) = found(after).map(|l| body[l].start) else {
        bail!(lost(name));
    };
    let (start, opens) = match found(before) {
        Some(l) => (body[l].end, span_start),
        None if hidden => return Ok(live.to_string()),
        None => (
            end,
            sk[section_start(&sk, before, |k| found(k).is_some())].start,
        ),
    };
    let Some(text) = rendered
        .strip_prefix(&skeleton[..opens])
        .and_then(|rest| rest.strip_suffix(&skeleton[span_end..]))
    else {
        bail!(
            "the pr_body template renders `{name}` outside its own section, so \
             that section cannot be replaced alone"
        );
    };
    Ok(format!("{}{text}{}", &live[..start], &live[end..]))
}

/// Whether the values hide the section `name` owns. `marked` is the render
/// with the section's variables as slots and every other variable as its
/// value. A variable outside the section hides it when `marked` lacks the
/// slot of `name`. The section's own values hide it when `rendered` has
/// fewer of the line that opens it than `marked`.
pub(crate) fn hides(skeleton: &str, marked: &str, rendered: &str, name: &str) -> Result<bool> {
    if !marked.contains(&slot(name)) {
        return Ok(true);
    }
    let sk = lines(skeleton);
    let at = slot_line(&sk, &slot(name), name)?;
    let (before, _) = bounds(&sk, at, name)?;
    let count = |text: &str| {
        lines(text)
            .iter()
            .filter(|l| fits(sk[before].text, l.text))
            .count()
    };
    Ok(count(rendered) < count(marked))
}

/// The template lines on either side of skeleton line `at`.
fn bounds(sk: &[Line<'_>], at: usize, name: &str) -> Result<(usize, usize)> {
    let anchor = |i: &usize| is_anchor(sk[*i].text);
    let Some(before) = (0..at).rev().find(anchor) else {
        bail!(unbounded(name, "before"));
    };
    let Some(after) = (at + 1..sk.len()).find(anchor) else {
        bail!(unbounded(name, "after"));
    };
    Ok((before, after))
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

/// Every variable whose slot is on a skeleton line, in order.
fn slot_names(text: &str) -> impl Iterator<Item = &str> {
    text.split(OPEN)
        .skip(1)
        .filter_map(|rest| rest.find(CLOSE).map(|end| &rest[..end]))
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

    /// `skeleton` rendered with `value` for `name`.
    fn fill(skeleton: &str, name: &str, value: &str) -> String {
        skeleton.replace(&slot(name), value)
    }

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

        let got = splice(
            SKELETON,
            &fill(SKELETON, "proof", "new"),
            live,
            "proof",
            false,
        )
        .unwrap();

        assert_eq!(got, live.replace("\np\n", "\nnew\n"));
    }

    /// GitHub stores a body edited in its web UI with CRLF line endings.
    #[test]
    fn crlf_line_endings_still_match() {
        let live = "## Summary\r\n\r\nold\r\n\r\n## Proof\r\n\r\np\r\n\r\n---\r\n\r\nFooter\r\n";

        let got = splice(
            SKELETON,
            &fill(SKELETON, "input", "new"),
            live,
            "input",
            false,
        )
        .unwrap();

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
        let err = splice(tail, tail, live, "proof", false).unwrap_err();
        assert!(err.to_string().contains("after it"), "{err}");

        let head = "@@devkit-slot:input@@\n\n## Proof\n\n@@devkit-slot:proof@@\n\n---";
        let live = "> bot banner\n\nsummary\n\n## Proof\n\np\n\n---\n";
        let err = splice(head, head, live, "input", false).unwrap_err();
        assert!(err.to_string().contains("before it"), "{err}");
    }

    #[test]
    fn an_inline_variable_is_refused() {
        let err = splice(SKELETON, SKELETON, "Footer by X", "agent_model", false).unwrap_err();
        assert!(err.to_string().contains("line of its own"), "{err}");
    }

    const SHARED: &str =
        "## Merge danger\n\n@@devkit-slot:door@@\n\n@@devkit-slot:blast_radius@@\n\n---\n";

    #[test]
    fn a_shared_section_names_every_variable_it_renders() {
        let names = section(SHARED, "blast_radius", |_| true).unwrap();
        assert_eq!(names, ["blast_radius", "door"]);
    }

    #[test]
    fn a_shared_section_given_in_part_names_what_it_left_out() {
        let err = section(SHARED, "door", |n| n == "door").unwrap_err();
        assert!(
            err.to_string().contains("also pass `blast_radius`"),
            "{err}"
        );
    }

    #[test]
    fn a_shared_section_is_replaced_whole_from_the_rendered_template() {
        let rendered = fill(&fill(SHARED, "door", "x"), "blast_radius", "y");
        let live = "## Merge danger\n\na\n\nb\n\n---\nbot\n";

        let got = splice(SHARED, &rendered, live, "door", false).unwrap();

        assert_eq!(got, "## Merge danger\n\nx\n\ny\n\n---\nbot\n");
    }

    #[test]
    fn a_render_without_the_sections_opening_line_hides_it() {
        let marked = SHARED;
        let shown = fill(&fill(SHARED, "door", "x"), "blast_radius", "y");

        assert!(!hides(SHARED, marked, &shown, "door").unwrap());
        assert!(hides(SHARED, marked, "---\n", "door").unwrap());
    }
}
