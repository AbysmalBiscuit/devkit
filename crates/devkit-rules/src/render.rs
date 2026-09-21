//! One markdown block per injection event.

use crate::model::Rule;

pub const TRUNCATION_NOTE: &str = "\n\n(truncated)";

/// The rules and files as one block, truncated to `cap` bytes at a character
/// boundary. Empty when there is nothing to say, which the caller reads as
/// "emit nothing" rather than as an empty context block.
pub fn block(rules: &[&Rule], files: &[(String, String)], cap: usize) -> String {
    if rules.is_empty() && files.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    if !rules.is_empty() {
        out.push_str("## Rules for the files you are editing\n\n");
        for rule in rules {
            out.push_str(&format!(
                "- **{}** ({}) {} (from {})\n",
                rule.title, rule.severity_raw, rule.description, rule.source_file
            ));
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
        let text = block(&rules, &[], 4096);
        assert!(text.contains("Root must"), "{text}");
        assert!(text.contains("AGENTS.md"), "the source file: {text}");
    }

    #[test]
    fn a_block_with_nothing_in_it_is_empty() {
        assert_eq!(block(&[], &[], 4096), "");
    }

    #[test]
    fn a_block_over_the_cap_is_truncated_and_says_so() {
        let index = fixture();
        let rules: Vec<&_> = index.rules.iter().collect();
        let text = block(&rules, &[], 120);
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
        let text = block(&[], &files, 4096);
        assert!(text.contains("crates/foo/AGENTS.md"), "{text}");
        assert!(text.contains("be careful"), "{text}");
    }

    #[test]
    fn a_cap_landing_inside_a_multibyte_character_walks_back() {
        let files = [(
            "p".to_string(),
            "cafes are nice, but so are cafés".to_string(),
        )];
        let full = block(&[], &files, usize::MAX);
        let mid = full.find('\u{e9}').unwrap() + 1;
        assert!(!full.is_char_boundary(mid), "cap must land mid-character");

        let text = block(&[], &files, mid);
        assert!(text.ends_with(TRUNCATION_NOTE), "{text}");
        assert_eq!(
            &text[..text.len() - TRUNCATION_NOTE.len()],
            &full[..mid - 1]
        );
    }
}
