//! Answers in the shape of `repo-rules-agent`'s `repo_rules_api` functions,
//! as the rules design spec's contract section describes them, for scripting
//! a `FakeServer` that stands in for a Supabase project.
#![allow(dead_code)]

use devkit_rules::model::RuleIndex;
use serde_json::{Value, json};

/// Each live rule of `index` as `query_rules` returns it: its fields, a
/// fresh `rule_key`, and its topics as one of the extra fields.
pub fn payloads(index: &RuleIndex) -> Vec<Value> {
    index
        .rules
        .iter()
        .filter(|rule| !rule.removed)
        .map(|rule| {
            json!({
                "id": rule.id,
                "rule_key": uuid::Uuid::new_v4().to_string(),
                "title": rule.title,
                "description": rule.description,
                "category": rule.category,
                "scope": rule.scope_raw,
                "severity": rule.severity_raw,
                "directory": rule.directory,
                "source_file": rule.source_file,
                "pinned": rule.pinned,
                "removed": false,
                "tasks": rule.tasks,
                "languages": rule.languages,
                "topics": rule.topics,
            })
        })
        .collect()
}

/// One `query_rules` page at `revision`, continuing after the position and
/// rule key given.
pub fn page(revision: i64, rules: &[Value], continuation: Option<(i64, &str)>) -> Value {
    let continuation = match continuation {
        Some((position, key)) => json!({
            "after_position": position,
            "after_rule_key": key,
            "expected_revision": revision,
        }),
        None => Value::Null,
    };
    json!({
        "revision": revision,
        "generation": "00000000-0000-4000-8000-000000000001",
        "rules": rules,
        "continuation": continuation,
    })
}

/// `stats_rules`' answer at `revision`.
pub fn stats(revision: i64) -> Value {
    json!({
        "revision": revision,
        "generation": "00000000-0000-4000-8000-000000000001",
        "conflict_count": 0,
        "total_rules": 0,
        "total_files": 0,
        "by_severity": {},
        "by_category": {},
    })
}

/// `put_rule`'s answer for a change that took the revision to `revision`.
pub fn put(revision: i64, key: &str) -> Value {
    json!({
        "revision": revision,
        "generation": "00000000-0000-4000-8000-000000000001",
        "rule_key": key,
    })
}

/// A revision conflict, as every function that takes an expected revision
/// answers it.
pub fn conflict(revision: i64) -> Value {
    json!({
        "error": "conflict",
        "revision": revision,
        "generation": "00000000-0000-4000-8000-000000000001",
    })
}
