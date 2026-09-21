//! The rule index as `repo-rules-agent` writes it.
//!
//! Severity and scope arrive as raw strings and are parsed into enums by
//! `vocab`, because a rule carrying a value outside the vocabulary is dropped
//! rather than defaulted: the extractor already drops it at validation, and
//! silently coercing it here would resurrect a rule its own producer refused.

use serde::{Deserialize, Serialize};

use crate::vocab::{Scope, Severity, Task, canonical_language, vocabulary_key};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl Rule {
    /// `None` when the index carries a severity outside the vocabulary, which
    /// drops the rule. The extractor's own validation already refuses these.
    pub fn severity(&self) -> Option<Severity> {
        self.severity_raw.parse().ok()
    }

    pub fn scope(&self) -> Option<Scope> {
        self.scope_raw.parse().ok()
    }

    /// Parsed tasks, or `None` if any spelling is outside the vocabulary.
    /// An empty list applies to every task.
    pub fn tasks(&self) -> Option<Vec<Task>> {
        self.tasks.iter().map(|t| t.parse().ok()).collect()
    }

    pub fn languages_canonical(&self) -> Vec<String> {
        self.languages
            .iter()
            .map(|l| canonical_language(l))
            .collect()
    }

    pub fn topics_canonical(&self) -> Vec<String> {
        self.topics.iter().map(|t| vocabulary_key(t)).collect()
    }
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
