//! Placing one section of template variables in a PR body that has drifted
//! from what the `pr_body` template rendered. Bots and people add to a live
//! body, so only the span the section owns is rewritten.
//!
//! The template is rendered once with every variable as its [`slot`]. That
//! skeleton's anchors, the lines carrying any of the template's own text, are
//! found in the live body in order, and the ones on either side of a
//! variable's slot bound the span that it shares with every slot inside it.
//! A variable on a line with template text has that line as its opening bound,
//! so its span runs from that line up to the next anchor, and it shares that
//! span with every slot below it.

use anyhow::{Result, bail};

const OPEN: &str = "@@devkit-slot:";
const CLOSE: &str = "@@";

/// The variable a body renders the PR's title into. An edit cannot pass it.
pub(crate) const TITLE: &str = "pr_title";

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
    let first = before + usize::from(!opens_with_values(sk[before].text));
    for other in sk[first..after].iter().flat_map(|l| slot_names(l.text)) {
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

/// How much of a section the values render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shown<'a> {
    Whole,
    /// Another variable hides some of the section's variables. The text is
    /// the template rendered with the values.
    Part(&'a str),
    Hidden,
}

/// `live` with the span the section `names` owns replaced by that span of
/// `rendered`, the template rendered with the section's values and every
/// other variable as its slot. When the live body lacks the template text that
/// opens the span, a [`Shown::Whole`] span is inserted where the template puts
/// it, a [`Shown::Part`] span is replaced by what the values render of it, and
/// `live` is kept as it is for a [`Shown::Hidden`] span.
pub(crate) fn splice(
    skeleton: &str,
    rendered: &str,
    live: &str,
    names: &[String],
    shown: Shown<'_>,
) -> Result<String> {
    let name = names[0].as_str();
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

    let span_end = sk[after].start;
    let Some(end) = found(after).map(|l| body[l].start) else {
        bail!(lost(name));
    };
    let (start, opens) = match (found(before), shown) {
        // The line fits, so the render writes its template text back unchanged.
        (Some(l), _) if opens_with_values(sk[before].text) => (body[l].start, sk[before].start),
        (Some(l), _) => (body[l].end, sk[before].end),
        (None, Shown::Hidden) => return Ok(live.to_string()),
        (None, Shown::Part(full)) => {
            let Some((open, row)) = (0..before)
                .rev()
                .filter(|&k| is_anchor(sk[k].text))
                .find_map(|k| found(k).map(|l| (k, l)))
            else {
                bail!(lost(name));
            };
            let text = visible_part(&sk, open, after, names, full)?;
            return Ok(format!("{}{text}{}", &live[..body[row].end], &live[end..]));
        }
        (None, Shown::Whole) => (
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

/// How much of the section `names` the values render. `marked` is the
/// render with the section's variables as slots and every other variable as
/// its value, and `full` is the render with the values. A variable outside
/// the section hides each section variable whose slot `marked` lacks. The
/// section's own values hide it when `full` has fewer of the line that opens
/// it than `marked`.
pub(crate) fn shown<'a>(
    skeleton: &str,
    marked: &str,
    full: &'a str,
    names: &[String],
) -> Result<Shown<'a>> {
    let visible = names.iter().filter(|n| marked.contains(&slot(n))).count();
    if visible == 0 {
        return Ok(Shown::Hidden);
    }
    if visible < names.len() {
        return Ok(Shown::Part(full));
    }
    let name = names[0].as_str();
    let sk = lines(skeleton);
    let at = slot_line(&sk, &slot(name), name)?;
    let (before, _) = bounds(&sk, at, name)?;
    let count = |text: &str| {
        lines(text)
            .iter()
            .filter(|l| fits(sk[before].text, l.text))
            .count()
    };
    Ok(if count(full) < count(marked) {
        Shown::Hidden
    } else {
        Shown::Whole
    })
}

/// The text `full` renders between the template lines `open` and `after`.
/// Every variable in that span must be one of the section's `names`.
fn visible_part<'a>(
    sk: &[Line<'_>],
    open: usize,
    after: usize,
    names: &[String],
    full: &'a str,
) -> Result<&'a str> {
    let name = &names[0];
    let left_out: Vec<String> = sk[open + 1..after]
        .iter()
        .flat_map(|l| slot_names(l.text))
        .filter(|n| !names.iter().any(|x| x == n))
        .map(|n| format!("`{n}`"))
        .collect();
    if !left_out.is_empty() {
        bail!(
            "the body lacks the line that opens `{name}`'s section, so the edit \
             replaces from the line before it and must also pass {}",
            left_out.join(", ")
        );
    }
    let fl = lines(full);
    let only = |i: usize| {
        let mut fitting = fl.iter().filter(|l| fits(sk[i].text, l.text));
        match (fitting.next(), fitting.next()) {
            (Some(l), None) => Ok(l),
            _ => Err(anyhow::anyhow!(
                "cannot tell where `{name}` goes: the pr_body template's line `{}` \
                 does not render once",
                sk[i].text
            )),
        }
    };
    let (from, to) = (only(open)?.end, only(after)?.start);
    if from > to {
        bail!(lost(name));
    }
    Ok(&full[from..to])
}

/// The template lines that bound skeleton line `at`: the line itself when
/// template text comes before its slot, else the one before it, and the one
/// after it. Text only after the slot cannot open the span, because a
/// multi-line value's first row does not show it.
fn bounds(sk: &[Line<'_>], at: usize, name: &str) -> Result<(usize, usize)> {
    let anchor = |i: &usize| is_anchor(sk[*i].text);
    let opens_itself = !segments(sk[at].text)[0].trim().is_empty();
    let Some(before) = (0..at + usize::from(opens_itself)).rev().find(anchor) else {
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
/// own text in order, with anything where its slots are. A line with text
/// before its first slot fits by that text alone: a multi-line value moves the
/// text after a slot to a later row, and a blank value can leave no space after
/// the text before it.
fn fits(pattern: &str, text: &str) -> bool {
    let parts = segments(pattern);
    let (first, after_first) = parts.split_first().expect("a line has a first segment");
    if !after_first.is_empty() && !first.trim().is_empty() {
        return text.starts_with(first) || text == first.trim_end();
    }
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

/// Whether a span's opening line holds variables an edit can pass. A value on
/// that line can run on into the span, so the line belongs to the span. A title
/// cannot be passed and stays on one row, so its line only bounds the span.
fn opens_with_values(text: &str) -> bool {
    slot_names(text).next().is_some() && slot_names(text).all(|n| n != TITLE)
}

/// Every variable whose slot is on a skeleton line, in order.
fn slot_names(text: &str) -> impl Iterator<Item = &str> {
    text.split(OPEN)
        .skip(1)
        .filter_map(|rest| rest.find(CLOSE).map(|end| &rest[..end]))
}

/// The one skeleton line that holds `marker`, with no other variable on it.
fn slot_line(sk: &[Line<'_>], marker: &str, name: &str) -> Result<usize> {
    let mut holding = sk
        .iter()
        .enumerate()
        .filter(|(_, l)| l.text.contains(marker));
    let Some((at, line)) = holding.next() else {
        bail!("`{name}` is not in the pr_body template");
    };
    if holding.next().is_some() || line.text.matches(marker).count() > 1 {
        bail!(
            "`{name}` renders in more than one place in the pr_body template, so \
             no one place can be replaced alone"
        );
    }
    let others: Vec<String> = slot_names(line.text)
        .filter(|other| *other != name)
        .map(|other| format!("`{other}`"))
        .collect();
    if !others.is_empty() {
        bail!(
            "`{name}` shares its line of the pr_body template with {}, so it cannot \
             be replaced alone",
            others.join(", ")
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
/// it, so a section opened by several lines is inserted whole. A line with a
/// variable ends the run, since that variable is not this section's to render.
fn section_start(sk: &[Line<'_>], heading: usize, matched: impl Fn(usize) -> bool) -> usize {
    let mut opens = heading;
    for k in (0..heading).rev() {
        let line = &sk[k];
        if line.text.is_empty() {
            continue;
        }
        if slot_names(line.text).next().is_some() || matched(k) {
            break;
        }
        opens = k;
    }
    opens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(of: &[&str]) -> Vec<String> {
        of.iter().map(|n| n.to_string()).collect()
    }

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
            &names(&["proof"]),
            Shown::Whole,
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
            &names(&["input"]),
            Shown::Whole,
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
        let err = splice(tail, tail, live, &names(&["proof"]), Shown::Whole).unwrap_err();
        assert!(err.to_string().contains("after it"), "{err}");

        let head = "@@devkit-slot:input@@\n\n## Proof\n\n@@devkit-slot:proof@@\n\n---";
        let live = "> bot banner\n\nsummary\n\n## Proof\n\np\n\n---\n";
        let err = splice(head, head, live, &names(&["input"]), Shown::Whole).unwrap_err();
        assert!(err.to_string().contains("before it"), "{err}");
    }

    /// The text below a label line at the end of the template cannot be told
    /// from what a bot appended there, as for a variable on its own line.
    #[test]
    fn a_label_line_with_no_template_line_after_it_is_refused() {
        let err = splice(
            SKELETON,
            SKELETON,
            "Footer by X",
            &names(&["agent_model"]),
            Shown::Whole,
        )
        .unwrap_err();
        assert!(err.to_string().contains("after it"), "{err}");
    }

    const LABELS: &str = "## Merge danger\n\n**Door:** @@devkit-slot:door@@\n\n\
                          **Blast Radius:** @@devkit-slot:blast_radius@@\n\n---\n";

    fn replace_on_label_line(skeleton: &str, name: &str, value: &str, live: &str) -> String {
        splice(
            skeleton,
            &fill(skeleton, name, value),
            live,
            &names(&[name]),
            Shown::Whole,
        )
        .unwrap()
    }

    #[test]
    fn a_label_line_keeps_its_text_when_its_variable_is_replaced() {
        let live = "## Merge danger\n\n**Door:** a\n\n**Blast Radius:** b\n\n---\nbot\n";

        assert_eq!(
            replace_on_label_line(LABELS, "door", "x", live),
            "## Merge danger\n\n**Door:** x\n\n**Blast Radius:** b\n\n---\nbot\n"
        );
    }

    #[test]
    fn a_multi_line_value_fills_the_label_line_and_the_lines_after_it() {
        let live = "## Merge danger\n\n**Door:** a\n\n**Blast Radius:** old\n- old\n\n---\nbot\n";

        assert_eq!(
            replace_on_label_line(LABELS, "blast_radius", "new\n- one\n- two", live),
            "## Merge danger\n\n**Door:** a\n\n**Blast Radius:** new\n- one\n- two\n\n---\nbot\n"
        );
    }

    #[test]
    fn a_label_line_keeps_the_text_after_its_variable() {
        let skeleton = "## H\n\nDoor @@devkit-slot:door@@ (checked)\n\n---\n";
        let live = "## H\n\nDoor a (checked)\n\n---\nbot\n";

        assert_eq!(
            replace_on_label_line(skeleton, "door", "x", live),
            "## H\n\nDoor x (checked)\n\n---\nbot\n"
        );
    }

    /// A blank value leaves a label line whose trailing space the body may
    /// have lost, and that line is still the one to replace.
    #[test]
    fn a_label_line_whose_value_is_blank_is_found() {
        let live = "## Merge danger\n\n**Door:**\n\n**Blast Radius:** b\n\n---\n";

        assert_eq!(
            replace_on_label_line(LABELS, "door", "x", live),
            "## Merge danger\n\n**Door:** x\n\n**Blast Radius:** b\n\n---\n"
        );
    }

    #[test]
    fn a_label_line_shares_its_section_with_the_slots_below_it() {
        let skeleton = "## H\n\n**Door:** @@devkit-slot:door@@\n@@devkit-slot:extra@@\n\n---\n";

        let err = section(skeleton, "door", |n| n == "door").unwrap_err();
        assert!(err.to_string().contains("also pass `extra`"), "{err}");
        assert_eq!(section(skeleton, "door", |_| true).unwrap(), [
            "door", "extra"
        ]);
    }

    #[test]
    fn a_variable_on_a_line_with_another_variable_is_refused_naming_it() {
        let skeleton =
            "## H\n\n**Door:** @@devkit-slot:door@@ **Owner:** @@devkit-slot:owner@@\n\n---\n";

        let err = section(skeleton, "door", |_| true).unwrap_err().to_string();
        assert!(err.contains("shares its line"), "{err}");
        assert!(err.contains("`owner`"), "{err}");
    }

    #[test]
    fn a_variable_rendered_twice_is_refused_naming_the_reason() {
        for skeleton in [
            "## H\n\nDoor @@devkit-slot:door@@ again @@devkit-slot:door@@\n\n---\n",
            "## H\n\n@@devkit-slot:door@@\n\n## I\n\n@@devkit-slot:door@@\n\n---\n",
        ] {
            let err = section(skeleton, "door", |_| true).unwrap_err().to_string();
            assert!(err.contains("more than one place"), "{err}");
        }
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

        let got = splice(
            SHARED,
            &rendered,
            live,
            &names(&["door", "blast_radius"]),
            Shown::Whole,
        )
        .unwrap();

        assert_eq!(got, "## Merge danger\n\nx\n\ny\n\n---\nbot\n");
    }

    #[test]
    fn a_part_shown_section_without_its_opening_line_needs_the_variables_before_it() {
        let skeleton = "## Summary\n\n@@devkit-slot:input@@\n\n### Optional\n\n\
                        @@devkit-slot:detail@@\n\n@@devkit-slot:tail@@\n\n## Proof\n";
        let full = "## Summary\n\n@@devkit-slot:input@@\n\nnew\n\n## Proof\n";
        let live = "## Summary\n\ns\n\nold\n\n## Proof\n";

        let err = splice(
            skeleton,
            skeleton,
            live,
            &names(&["detail", "tail"]),
            Shown::Part(full),
        )
        .unwrap_err();

        assert!(err.to_string().contains("also pass `input`"), "{err}");
    }

    #[test]
    fn a_render_without_the_sections_opening_line_hides_it() {
        let marked = SHARED;
        let shown = fill(&fill(SHARED, "door", "x"), "blast_radius", "y");

        let both = names(&["door", "blast_radius"]);
        assert_eq!(
            super::shown(SHARED, marked, &shown, &both).unwrap(),
            Shown::Whole
        );
        assert_eq!(
            super::shown(SHARED, marked, "---\n", &both).unwrap(),
            Shown::Hidden
        );
    }
}
