//! The rule index as `repo-rules-agent` writes it.
//!
//! Severity and scope arrive as raw strings and are parsed into enums by
//! `vocab`, because a rule carrying a value outside the vocabulary is dropped
//! rather than defaulted: the extractor already drops it at validation, and
//! silently coercing it here would resurrect a rule its own producer refused.

use serde::Deserialize;

fn all_languages() -> Vec<String> {
    vec!["all".to_string()]
}

fn should() -> String {
    "should".to_string()
}

fn repo() -> String {
    "repo".to_string()
}

fn best_practice() -> String {
    "best_practice".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "best_practice")]
    pub category: String,
    #[serde(default)]
    pub tasks: Vec<String>,
    #[serde(default = "all_languages")]
    pub languages: Vec<String>,
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default = "repo", rename = "scope")]
    pub scope_raw: String,
    #[serde(default = "should", rename = "severity")]
    pub severity_raw: String,
    #[serde(default)]
    pub source_file: String,
    #[serde(default)]
    pub directory: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuleFile {
    pub path: String,
    pub tier: i64,
    #[serde(default)]
    pub applies_to: String,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuleIndex {
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub files: Vec<RuleFile>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The extractor writes with `exclude_none=True`, so every pydantic default
    /// is absent from the JSON and has to be a serde default here.
    #[test]
    fn omitted_fields_take_the_extractor_defaults() {
        let raw = r#"{
            "repo": "/repo",
            "files": [{"path": "AGENTS.md", "tier": 1}],
            "rules": [{"id": "abc123", "title": "T", "description": "D", "source_file": "AGENTS.md"}]
        }"#;
        let index: RuleIndex = serde_json::from_str(raw).unwrap();
        let rule = &index.rules[0];
        assert_eq!(rule.languages, vec!["all".to_string()]);
        assert_eq!(rule.severity_raw, "should");
        assert_eq!(rule.scope_raw, "repo");
        assert!(rule.tasks.is_empty());
        assert!(rule.topics.is_empty());
        assert_eq!(rule.directory, "");
        assert_eq!(rule.category, "best_practice");
        assert_eq!(index.files[0].applies_to, "");
        assert!(index.files[0].content.is_none());
    }
}
