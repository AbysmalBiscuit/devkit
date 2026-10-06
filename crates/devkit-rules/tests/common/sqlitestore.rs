//! A SQLite store carrying `repo-rules-agent`'s query schema, and an import
//! that writes an exported index into it as the extractor's `import-json`
//! would for a new repository: one source per discovered file in file order, a
//! non-discovered source for any other file a rule names, and each rule at its
//! position in the export with its pin and tombstone.

use std::path::Path;

use rusqlite::{Connection, params};
use serde_json::{Map, Value};

/// The extractor's dump of a store, whose schema the import reuses.
const DUMP: &str = include_str!("../fixtures/sqlite/store.sql");

/// The columns a rule keeps outside `extra`.
const COLUMNS: [&str; 13] = [
    "id",
    "title",
    "description",
    "category",
    "scope",
    "severity",
    "directory",
    "source_file",
    "tasks",
    "languages",
    "pinned",
    "removed",
    "extraction_fingerprint",
];

fn key() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Writes `export`, an index in the form `export-json` writes, as the only
/// repository of a new store at `path`.
pub fn import(path: &Path, export: &Value) {
    let db = Connection::open(path).unwrap();
    // The dump creates tables alphabetically, so a membership row arrives
    // before the `rules` table its foreign key names.
    db.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    db.execute_batch(DUMP).unwrap();
    db.execute_batch(
        "DELETE FROM rule_tasks; DELETE FROM rule_languages; DELETE FROM rules;
         DELETE FROM sources; DELETE FROM repository_members; DELETE FROM repositories;
         PRAGMA foreign_keys = ON;",
    )
    .unwrap();
    let repo = export["repo"].as_str().unwrap();
    let repo_id = key();
    db.execute(
        "INSERT INTO repositories VALUES (?1, 'local:' || ?2, ?2, ?3, ?4, 1, ?5)",
        params![
            repo_id,
            repo,
            export["source_sha"].as_str().unwrap_or(""),
            key(),
            export["conflicts"].as_array().map_or(0, Vec::len) as i64,
        ],
    )
    .unwrap();
    let source = |path: &str, tier: i64, discovered: bool, extra: Map<String, Value>| {
        let key = key();
        db.execute(
            "INSERT INTO sources VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                repo_id,
                key,
                path,
                tier,
                discovered,
                Value::Object(extra).to_string()
            ],
        )
        .unwrap();
        key
    };
    let mut sources: Vec<(String, String)> = Vec::new();
    let files = export["files"].as_array().cloned().unwrap_or_default();
    for (position, file) in files.iter().enumerate() {
        let mut extra = Map::new();
        extra.insert("file_position".into(), Value::from(position));
        if file["errors"].as_array().is_some_and(|e| !e.is_empty()) {
            extra.insert("errors".into(), file["errors"].clone());
        }
        let path = file["path"].as_str().unwrap().to_string();
        let key = source(&path, file["tier"].as_i64().unwrap(), true, extra);
        sources.push((path, key));
    }
    let rules = export["rules"].as_array().cloned().unwrap_or_default();
    for (position, rule) in rules.iter().enumerate() {
        let path = rule["source_file"].as_str().unwrap_or("").to_string();
        let source_key = match sources.iter().find(|(p, _)| *p == path) {
            Some((_, key)) => key.clone(),
            None => {
                let key = source(&path, 0, false, Map::new());
                sources.push((path, key.clone()));
                key
            }
        };
        insert_rule(&db, &repo_id, &source_key, position, rule);
    }
}

fn insert_rule(db: &Connection, repo_id: &str, source_key: &str, position: usize, rule: &Value) {
    let text = |key: &str, default: &str| rule[key].as_str().unwrap_or(default).to_string();
    let list = |key: &str, default: &[&str]| -> Vec<String> {
        match rule[key].as_array() {
            Some(values) => values
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect(),
            None => default.iter().map(|v| v.to_string()).collect(),
        }
    };
    let pinned = rule["pinned"].as_bool().unwrap_or(false);
    let mut extra: Map<String, Value> = rule
        .as_object()
        .unwrap()
        .iter()
        .filter(|(key, _)| !COLUMNS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    extra
        .entry("topics")
        .or_insert_with(|| Value::Array(Vec::new()));
    let fingerprint = rule["extraction_fingerprint"].as_str().filter(|_| pinned);
    let rule_key = key();
    db.execute(
        "INSERT INTO rules VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            repo_id,
            rule_key,
            text("id", ""),
            source_key,
            text("title", ""),
            text("description", ""),
            text("category", "best_practice"),
            text("scope", "repo"),
            text("severity", "should"),
            text("directory", ""),
            position as i64,
            pinned,
            rule["removed"].as_bool().unwrap_or(false),
            fingerprint,
            Value::Object(extra).to_string(),
        ],
    )
    .unwrap();
    for (table, values) in [
        ("rule_tasks", list("tasks", &[])),
        ("rule_languages", list("languages", &["all"])),
    ] {
        for (at, value) in values.iter().enumerate() {
            db.execute(
                &format!("INSERT INTO {table} VALUES (?1, ?2, ?3, ?4)"),
                params![repo_id, rule_key, at as i64, value],
            )
            .unwrap();
        }
    }
}
