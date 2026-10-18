//! The proof gate on `pr create` and `pr edit`: an agent's proof must answer
//! every item under the issue's `Done when` (or `Acceptance criteria`) section.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::{Context, Result, bail};
use devkit_common::{caller::Caller, forge::Repos, progress::Steps, tracker::Tracker};
use devkit_config::Config;

/// Refuse an agent's proof in `vars` that skips an item of the worktree's
/// tracker `issue`, when `defaults.pr_proof_variable` names the variable
/// carrying it.
pub(crate) fn gate_proof(
    cfg: &Config,
    here: &Path,
    repos: &Repos,
    issue: Option<&str>,
    caller: Caller,
    vars: &BTreeMap<String, String>,
) -> Result<()> {
    let (Some(variable), Some(issue), Caller::Agent) =
        (&cfg.defaults.pr_proof_variable, issue, caller)
    else {
        return Ok(());
    };
    let tracker = devkit_common::tracker::resolve(cfg.tracker.kind, here, repos);
    let proof = vars.get(variable).map_or("", String::as_str);
    require_proof(tracker.tracker.as_ref(), issue, proof, variable)
}

/// Refuse `proof` when it skips an item of `issue`, read from the tracker so
/// the items are the ones the issue carries now. An issue the tracker cannot
/// return is refused too: the check cannot pass on items it never saw, and so
/// is one whose `Done when` section is missing or lists no items, which leaves
/// nothing to prove.
pub(crate) fn require_proof(
    tracker: &dyn Tracker,
    issue: &str,
    proof: &str,
    variable: &str,
) -> Result<()> {
    let details = Steps::new()
        .during_result("Reading the issue's Done when items\u{2026}", || {
            tracker.details(issue)
        })
        .with_context(|| format!("reading issue {issue} to check --arg {variable}"))?
        .with_context(|| {
            format!(
                "no issue {issue} in the {} tracker to check --arg {variable} against",
                tracker.kind()
            )
        })?;
    let items = done_when_items(&details.description);
    if items.is_empty() {
        let (problem, fix) = match outside_fences(&details.description).any(opens_section) {
            true => (
                "its `Done when` (or `Acceptance criteria`) section lists no items",
                "List the outcomes under that section, one item per outcome",
            ),
            false => (
                "it has no `Done when` (or `Acceptance criteria`) section",
                "Add a `## Done when` list to the issue, one item per outcome",
            ),
        };
        bail!(
            "refusing --arg {variable} for issue {issue}: {problem}, so there is \
             nothing to prove. {fix}, then run this again."
        );
    }
    require_every_item(&items, proof, variable)
}

/// Section titles, matched case-insensitively and with a trailing colon
/// ignored, that open the list of items an issue is done when.
const SECTION_TITLES: [&str; 2] = ["done when", "acceptance criteria"];

/// The items listed under the issue description's first `Done when` or
/// `Acceptance criteria` section, in order. Empty when there is no such
/// section, opened by a heading or a bold label (`**Acceptance criteria:**`).
///
/// A nested entry or a wrapped line joins the item above it. The list ends at
/// a heading, a bold label, or unindented text after a blank line.
pub(crate) fn done_when_items(description: &str) -> Vec<String> {
    done_when_entries(description)
        .into_iter()
        .map(|e| e.text)
        .collect()
}

/// One item of an issue's `Done when` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DoneWhen {
    /// The item's text, without its list marker or checkbox.
    pub(crate) text: String,
    /// Whether its task checkbox is ticked, `[x]`.
    pub(crate) done: bool,
}

/// The items [`done_when_items`] lists, each with whether it is ticked.
pub(crate) fn done_when_entries(description: &str) -> Vec<DoneWhen> {
    let mut items: Vec<DoneWhen> = Vec::new();
    let mut top: Option<usize> = None;
    let mut after_blank = false;
    let section = outside_fences(description)
        .skip_while(|l| !opens_section(l))
        .skip(1);
    for line in section {
        let text = line.trim();
        if text.is_empty() {
            after_blank = true;
            continue;
        }
        if heading(line).is_some() || bold_label(line).is_some() {
            break;
        }
        let indent = indent(line);
        match (list_item(text), top) {
            (Some((done, item)), None) => {
                top = Some(indent);
                items.push(DoneWhen {
                    text: item.to_string(),
                    done,
                });
            }
            (Some((done, item)), Some(top)) if indent <= top => items.push(DoneWhen {
                text: item.to_string(),
                done,
            }),
            (_, None) => {}
            (_, Some(top)) if indent > top || !after_blank => {
                let last = &mut items.last_mut().expect("a list has begun").text;
                last.push(' ');
                last.push_str(text);
            }
            _ => break,
        }
        after_blank = false;
    }
    items
}

/// Refuse a proof that leaves an item unanswered. Item `n` (1-based) is
/// answered by a top-level proof line that starts with `n.`, `n)` or `n:`, so
/// an agent may reword the item as long as it keeps the number.
pub(crate) fn require_every_item(items: &[String], proof: &str, variable: &str) -> Result<()> {
    let answered: BTreeSet<usize> = proof.lines().filter_map(proof_key).collect();
    let missing: Vec<String> = (1..)
        .zip(items)
        .filter(|(n, _)| !answered.contains(n))
        .map(|(n, item)| format!("  {n}. {item}"))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    bail!(
        "refusing to open a PR whose proof skips items the issue is done when:\n{}\n\
         Answer each in --arg {variable}, on a line starting with the item's number: \
         `1. <evidence>`",
        missing.join("\n")
    )
}

/// The lines of `text` outside fenced code blocks, fence lines dropped too.
fn outside_fences(text: &str) -> impl Iterator<Item = &str> {
    let mut open: Option<&str> = None;
    text.lines().filter(move |l| {
        let t = l.trim_start();
        let Some(marker) = ["```", "~~~"].into_iter().find(|m| t.starts_with(m)) else {
            return open.is_none();
        };
        match open {
            None => open = Some(marker),
            Some(m) if m == marker => open = None,
            Some(_) => return false,
        }
        false
    })
}

fn opens_section(line: &str) -> bool {
    heading(line)
        .or_else(|| bold_label(line))
        .is_some_and(|title| {
            let title = title.trim_end_matches(':').trim();
            SECTION_TITLES.iter().any(|s| title.eq_ignore_ascii_case(s))
        })
}

/// The text of an ATX heading (`## Title`).
fn heading(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let level = t.len() - t.trim_start_matches('#').len();
    let rest = &t[level..];
    ((1..=6).contains(&level) && (rest.is_empty() || rest.starts_with([' ', '\t'])))
        .then(|| rest.trim().trim_end_matches('#').trim())
}

/// The text of a line that is only a bold label: `**Title:**` or `**Title**:`.
fn bold_label(line: &str) -> Option<&str> {
    let inner = line.trim().strip_prefix("**")?;
    let inner = inner
        .strip_suffix("**:")
        .or_else(|| inner.strip_suffix("**"))?;
    (!inner.is_empty() && !inner.contains("**")).then_some(inner)
}

/// Whether a list entry's task checkbox is ticked, and the text after its
/// list marker (`-`, `*`, `+`, `1.`, `1)`) and that checkbox, or `None` when
/// `text` is not a list entry.
fn list_item(text: &str) -> Option<(bool, &str)> {
    let rest = match text.strip_prefix(['-', '*', '+']) {
        Some(rest) => rest,
        None => {
            let digits = leading_digits(text);
            if digits == 0 {
                return None;
            }
            text[digits..].strip_prefix(['.', ')'])?
        }
    };
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    let rest = rest.trim_start();
    let (done, rest) = [("[ ]", false), ("[x]", true), ("[X]", true)]
        .iter()
        .find_map(|(b, done)| rest.strip_prefix(b).map(|r| (*done, r)))
        .unwrap_or((false, rest));
    Some((done, rest.trim()))
}

/// The item number a proof line answers: its first token, after any bullet,
/// checkbox, heading marks or bold, written `n.`, `n)` or `n:`. A line
/// indented two columns or more belongs to the entry above it and answers
/// nothing, so a numbered sub-step cannot stand in for another item.
fn proof_key(line: &str) -> Option<usize> {
    if indent(line) >= 2 {
        return None;
    }
    let mut t = line.trim();
    for bullet in ["- ", "* ", "+ "] {
        t = t.strip_prefix(bullet).unwrap_or(t).trim_start();
    }
    for checkbox in ["[ ]", "[x]", "[X]"] {
        t = t.strip_prefix(checkbox).unwrap_or(t).trim_start();
    }
    t = t.trim_start_matches('#').trim_start();
    t = t.strip_prefix("**").unwrap_or(t);
    let digits = leading_digits(t);
    t[digits..].strip_prefix(['.', ')', ':'])?;
    t[..digits].parse().ok()
}

fn leading_digits(s: &str) -> usize {
    s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len()
}

/// Leading columns, a tab counting as four.
fn indent(line: &str) -> usize {
    line.chars()
        .map_while(|c| match c {
            ' ' => Some(1),
            '\t' => Some(4),
            _ => None,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISSUE: &str = "\
Intro paragraph.

## Done when

- [ ] The first thing holds (proof: test)
- [x] The second thing
  wraps onto a second line
  - with a nested detail
- [ ] The third thing

## Stop and ask if

- Not an item
";

    #[test]
    fn items_are_the_top_level_entries_of_the_section() {
        assert_eq!(done_when_items(ISSUE), vec![
            "The first thing holds (proof: test)",
            "The second thing wraps onto a second line - with a nested detail",
            "The third thing",
        ]);
    }

    #[test]
    fn acceptance_criteria_headings_and_bold_labels_count_in_any_case() {
        for heading in [
            "## Acceptance criteria",
            "### ACCEPTANCE CRITERIA",
            "# done when:",
            "**Acceptance criteria:**",
            "**acceptance criteria**:",
            "**Done When:**",
        ] {
            let body = format!("Intro.\n\n{heading}\n\n1. One\n2. Two\n\nTrailing prose.\n");
            assert_eq!(done_when_items(&body), vec!["One", "Two"], "{heading}");
        }
    }

    #[test]
    fn a_bold_label_section_ends_at_the_next_label() {
        let body = "**Acceptance criteria:**\n- One\n- Two\n**Notes:**\n- Not an item\n";
        assert_eq!(done_when_items(body), vec!["One", "Two"]);
    }

    #[test]
    fn a_fence_closes_only_on_its_own_marker() {
        let body = "```\n~~~\n## Done when\n- Fake\n```\n## Done when\n- Real\n";
        assert_eq!(done_when_items(body), vec!["Real"]);
    }

    #[test]
    fn no_section_means_no_items() {
        assert!(done_when_items("## Summary\n\n- a bullet\n").is_empty());
        assert!(done_when_items("").is_empty());
    }

    #[test]
    fn a_heading_inside_a_code_fence_is_not_a_section() {
        let body = "```md\n## Done when\n- Fake\n```\n";
        assert!(done_when_items(body).is_empty());
    }

    fn items() -> Vec<String> {
        [
            "Refuses a gap",
            "Opens when covered",
            "Reads acceptance criteria",
        ]
        .map(String::from)
        .to_vec()
    }

    #[test]
    fn a_proof_keyed_by_every_item_number_passes() {
        let proof = "1. test `refuses`\n2) test `opens`\n- 3: test `reads`\n";
        assert!(require_every_item(&items(), proof, "proof").is_ok());
    }

    #[test]
    fn a_reworded_item_still_counts_by_its_number() {
        let proof = "**1.** gaps get refused\n2. covered runs open\n3. AC headings parse\n";
        assert!(require_every_item(&items(), proof, "proof").is_ok());
    }

    #[test]
    fn the_refusal_lists_each_missing_item() {
        let proof = "2. test `opens`\n   1. a nested step is not a key\n";
        let err = require_every_item(&items(), proof, "proof")
            .unwrap_err()
            .to_string();
        assert!(err.contains("1. Refuses a gap"), "{err}");
        assert!(err.contains("3. Reads acceptance criteria"), "{err}");
        assert!(!err.contains("2. Opens when covered"), "{err}");
        assert!(err.contains("--arg proof"), "names the arg to fix: {err}");
    }

    #[test]
    fn no_items_means_nothing_to_cover() {
        assert!(require_every_item(&[], "", "proof").is_ok());
    }
}
