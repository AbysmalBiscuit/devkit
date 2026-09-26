//! One markdown block per injection event.

use crate::model::Rule;

pub const TRUNCATION_NOTE: &str = "\n\n(truncated)";

/// The rules heading for a block about specific files.
pub const EDIT_HEADING: &str = "Rules for the files you are editing";

/// A complete context block and the entries it contains, for fired-set
/// stamping.
pub struct Rendered<'a> {
    pub text: String,
    pub rules: Vec<&'a Rule>,
    pub files: Vec<(String, String)>,
}

/// Fit complete entries within `cap`, dropping trailing files before rules.
pub fn fit<'a>(
    heading: &str,
    mut rules: Vec<&'a Rule>,
    mut files: Vec<(String, String)>,
    cap: usize,
) -> Rendered<'a> {
    loop {
        let text = block(heading, &rules, &files, usize::MAX);
        if text.len() <= cap {
            return Rendered { text, rules, files };
        }
        if files.pop().is_none() {
            rules.pop();
        }
    }
}

/// The rules under `heading` and the files as one block, truncated to `cap`
/// bytes at a character boundary. Empty when there is nothing to say, which
/// the caller reads as "emit nothing" rather than as an empty context block.
pub fn block(heading: &str, rules: &[&Rule], files: &[(String, String)], cap: usize) -> String {
    if rules.is_empty() && files.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    if !rules.is_empty() {
        out.push_str(&format!("## {heading}\n\n"));
        for rule in rules {
            out.push_str(&format!(
                "- **{}** ({}) {}",
                rule.title, rule.severity_raw, rule.description
            ));
            if !rule.source_file.is_empty() {
                out.push_str(&format!(" (from {})", rule.source_file));
            }
            out.push('\n');
        }
    }
    for (path, body) in files {
        out.push_str(&format!("\n## {path}\n\n{body}\n"));
    }
    if out.len() > cap {
        let mut end = cap;
        while end > 0 && !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push_str(TRUNCATION_NOTE);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RuleIndex;

    fn fixture() -> RuleIndex {
        serde_json::from_str(include_str!("../tests/fixtures/index.json")).unwrap()
    }

    #[test]
    fn a_block_names_each_rule_and_its_source() {
        let index = fixture();
        let rules: Vec<&_> = index.rules.iter().take(1).collect();
        let text = block(EDIT_HEADING, &rules, &[], 4096);
        assert!(text.contains("Root must"), "{text}");
        assert!(text.contains("AGENTS.md"), "the source file: {text}");
    }

    /// A rule added by hand has no source file to name.
    #[test]
    fn a_rule_without_a_source_names_none() {
        let rule = crate::model::Rule {
            source_file: String::new(),
            ..fixture().rules[0].clone()
        };
        let text = block(EDIT_HEADING, &[&rule], &[], 4096);
        assert!(text.contains("Root must"), "{text}");
        assert!(!text.contains("(from"), "{text}");
    }

    #[test]
    fn a_block_with_nothing_in_it_is_empty() {
        assert_eq!(block(EDIT_HEADING, &[], &[], 4096), "");
    }

    #[test]
    fn a_block_over_the_cap_is_truncated_and_says_so() {
        let index = fixture();
        let rules: Vec<&_> = index.rules.iter().collect();
        let text = block(EDIT_HEADING, &rules, &[], 120);
        assert!(
            text.len() <= 120 + TRUNCATION_NOTE.len(),
            "len {}",
            text.len()
        );
        assert!(text.ends_with(TRUNCATION_NOTE), "{text}");
    }

    #[test]
    fn a_file_dump_carries_its_path_and_body() {
        let files = [("crates/foo/AGENTS.md".to_string(), "be careful".to_string())];
        let text = block(EDIT_HEADING, &[], &files, 4096);
        assert!(text.contains("crates/foo/AGENTS.md"), "{text}");
        assert!(text.contains("be careful"), "{text}");
    }

    #[test]
    fn a_cap_landing_inside_a_multibyte_character_walks_back() {
        let files = [(
            "p".to_string(),
            "cafes are nice, but so are cafés".to_string(),
        )];
        let full = block(EDIT_HEADING, &[], &files, usize::MAX);
        let mid = full.find('\u{e9}').unwrap() + 1;
        assert!(!full.is_char_boundary(mid), "cap must land mid-character");

        let text = block(EDIT_HEADING, &[], &files, mid);
        assert!(text.ends_with(TRUNCATION_NOTE), "{text}");
        assert_eq!(
            &text[..text.len() - TRUNCATION_NOTE.len()],
            &full[..mid - 1]
        );
    }
}
