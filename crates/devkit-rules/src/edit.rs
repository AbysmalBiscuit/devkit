//! Changing the index in place, for `devkit rules add`, `edit` and `remove`.
//!
//! The index is `repo-rules-agent`'s file, so a write keeps every field devkit
//! does not model and pins each rule it touches (see `Rule::pinned`).

use std::{ffi::OsString, path::Path};

use anyhow::{Result, bail};
use devkit_common::store;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    model::Rule,
    vocab::{Severity, Task, canonical_language, vocabulary_key},
};

/// The index as a write sees it: rules as raw objects, everything else passed
/// through untouched.
#[derive(Default, Serialize, Deserialize)]
pub struct IndexDocument {
    #[serde(default)]
    rules: Vec<Map<String, Value>>,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

impl store::Document for IndexDocument {
    fn stamp_version(&mut self) {}

    fn salvage(_: &str) -> Option<Self> {
        None
    }

    fn label() -> &'static str {
        "rules index"
    }

    fn len(&self) -> usize {
        self.rules.len()
    }
}

/// What a person sets on a rule. `None` leaves a field as it is.
#[derive(Debug, Default)]
pub struct Fields {
    pub title: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub severity: Option<Severity>,
    pub tasks: Option<Vec<Task>>,
    pub languages: Option<Vec<String>>,
    pub topics: Option<Vec<String>>,
    /// Repo-relative and '/'-separated; empty is the repository root. Sets the
    /// scope to match, as the extractor does.
    pub directory: Option<String>,
}

impl Fields {
    fn is_empty(&self) -> bool {
        let Fields {
            title,
            description,
            category,
            severity,
            tasks,
            languages,
            topics,
            directory,
        } = self;
        title.is_none()
            && description.is_none()
            && category.is_none()
            && severity.is_none()
            && tasks.is_none()
            && languages.is_none()
            && topics.is_none()
            && directory.is_none()
    }

    fn apply(self, rule: &mut Map<String, Value>) {
        let mut set = |key: &str, value: Value| {
            rule.insert(key.to_string(), value);
        };
        let strings = |values: Vec<String>| Value::from(values);
        if let Some(title) = self.title {
            set("title", title.into());
        }
        if let Some(description) = self.description {
            set("description", description.into());
        }
        if let Some(category) = self.category {
            set("category", vocabulary_key(&category).into());
        }
        if let Some(severity) = self.severity {
            set("severity", severity.to_string().into());
        }
        if let Some(tasks) = self.tasks {
            set(
                "tasks",
                strings(tasks.iter().map(Task::to_string).collect()),
            );
        }
        if let Some(languages) = self.languages {
            let canonical = languages.iter().map(|l| canonical_language(l)).collect();
            set("languages", strings(canonical));
        }
        if let Some(topics) = self.topics {
            set(
                "topics",
                strings(topics.iter().map(|t| vocabulary_key(t)).collect()),
            );
        }
        if let Some(directory) = self.directory {
            let scope = if directory.is_empty() {
                "repo"
            } else {
                "directory"
            };
            set("scope", scope.into());
            set("directory", directory.into());
        }
    }
}

/// Run `f` against the index at `path` under an advisory lock beside it, and
/// write the result back when `f` succeeds. A missing index starts empty; one
/// that does not parse is an error and stays as it was.
pub fn update<T>(path: &Path, f: impl FnOnce(&mut IndexDocument) -> Result<T>) -> Result<T> {
    let mut lock = OsString::from(path.as_os_str());
    lock.push(".lock");
    store::with_lock_strict(Path::new(&lock), path, f)
}

/// The id the extractor gives a rule: a prefix of the SHA-256 of
/// `source_file:title`, as `repo-rules-agent` `models.py` computes it.
fn rule_id(source_file: &str, title: &str) -> String {
    let digest = ring::digest::digest(
        &ring::digest::SHA256,
        format!("{source_file}:{title}").as_bytes(),
    );
    digest.as_ref()[..6]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn has_id(rule: &Map<String, Value>, id: &str) -> bool {
    rule.get("id").and_then(Value::as_str) == Some(id)
}

impl IndexDocument {
    /// Add a rule with the extractor's defaults under `fields`, and return its
    /// id. `repo` names the repository when the index is new.
    pub fn add(&mut self, repo: &str, fields: Fields) -> Result<String> {
        let title = fields.title.clone().unwrap_or_default();
        let id = rule_id("", &title);
        if self.rules.iter().any(|rule| has_id(rule, &id)) {
            bail!("rule {id} already exists; change it with `devkit rules edit {id}`");
        }
        let defaults: Rule = serde_json::from_value(Value::Object(Map::new()))?;
        let Value::Object(mut rule) = serde_json::to_value(defaults)? else {
            unreachable!("a struct serializes to an object");
        };
        fields.apply(&mut rule);
        rule.insert("id".to_string(), id.clone().into());
        rule.insert("pinned".to_string(), true.into());
        self.rest
            .entry("repo")
            .or_insert_with(|| repo.to_string().into());
        self.rules.push(rule);
        Ok(id)
    }

    /// Change the rule `id` and pin it, keeping its id and every field `fields`
    /// leaves alone.
    pub fn edit(&mut self, id: &str, fields: Fields) -> Result<()> {
        if fields.is_empty() {
            bail!("nothing to change: pass at least one field to set");
        }
        let at = self.position(id)?;
        let rule = &mut self.rules[at];
        fields.apply(rule);
        rule.insert("pinned".to_string(), true.into());
        Ok(())
    }

    /// Drop the rule `id`. An extracted rule stays as a pinned tombstone; one
    /// a person added has no source file to come back from and is deleted.
    pub fn remove(&mut self, id: &str) -> Result<()> {
        let at = self.position(id)?;
        let rule = &mut self.rules[at];
        let extracted = rule
            .get("source_file")
            .and_then(Value::as_str)
            .is_some_and(|source| !source.is_empty());
        if extracted {
            rule.insert("pinned".to_string(), true.into());
            rule.insert("removed".to_string(), true.into());
        } else {
            self.rules.remove(at);
        }
        Ok(())
    }

    /// Where the live rule `id` sits. A tombstone counts as absent.
    fn position(&self, id: &str) -> Result<usize> {
        self.rules
            .iter()
            .position(|rule| has_id(rule, id) && rule.get("removed") != Some(&Value::Bool(true)))
            .ok_or_else(|| anyhow::anyhow!("no rule {id} in the index"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Python's `hashlib.sha256(b"AGENTS.md:Use tracing").hexdigest()[:12]`.
    #[test]
    fn rule_ids_match_the_extractors() {
        assert_eq!(rule_id("AGENTS.md", "Use tracing"), "9c143a6b7b0e");
    }
}
