//! A disposable Postgres database carrying `repo-rules-agent`'s query schema,
//! and an import that writes an exported index into it as the extractor's
//! `import-json` would for a new repository: one source per discovered file in
//! file order, a non-discovered source for any other file a rule names, and
//! each rule at its position in the export with its pin and tombstone.
//!
//! `DEVKIT_TEST_POSTGRES_URL` names the server; without it
//! [`TestStore::create`] gives `None` and the test returns early.
#![allow(dead_code)]

use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::{Map, Value};
use tokio_postgres::{NoTls, Row, types::ToSql};

const SCHEMA: &str = include_str!("../fixtures/postgres/001_schema.sql");

/// The columns a rule keeps outside `extra`, and the ownership fields the
/// store never copies into it.
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

/// One database of its own, dropped with this value.
pub struct TestStore {
    admin: String,
    name: String,
    /// The URL that reaches this database.
    pub url: String,
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Runs `sql`, every statement in it, on the database `url` names.
pub fn batch(url: &str, sql: &str) {
    runtime().block_on(async {
        let (client, connection) = tokio_postgres::connect(url, NoTls).await.unwrap();
        tokio::spawn(connection);
        client.batch_execute(sql).await.unwrap();
    });
}

/// `url` with its database name replaced by `name`.
fn with_dbname(url: &str, name: &str) -> String {
    let (base, query) = url
        .split_once('?')
        .map_or((url, None), |(b, q)| (b, Some(q)));
    let (server, _) = base.rsplit_once('/').expect("a URL with a database name");
    match query {
        Some(query) => format!("{server}/{name}?{query}"),
        None => format!("{server}/{name}"),
    }
}

impl TestStore {
    /// A fresh database with the query schema loaded, or `None` when no test
    /// server is configured.
    pub fn create() -> Option<TestStore> {
        let admin = std::env::var("DEVKIT_TEST_POSTGRES_URL")
            .ok()
            .filter(|url| !url.trim().is_empty())?;
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!(
            "rules_{}_{nanos}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        batch(&admin, &format!("CREATE DATABASE {name}"));
        let url = with_dbname(&admin, &name);
        batch(&url, SCHEMA);
        Some(TestStore { admin, name, url })
    }

    /// Rows `sql` returns.
    pub fn query(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Vec<Row> {
        runtime().block_on(async {
            let (client, connection) = tokio_postgres::connect(&self.url, NoTls).await.unwrap();
            tokio::spawn(connection);
            client.query(sql, params).await.unwrap()
        })
    }

    /// Writes `export`, an index in the form `export-json` writes, as a new
    /// repository and returns its UUID.
    pub fn import(&self, export: &Value) -> String {
        let repo = export["repo"].as_str().unwrap();
        let repo_id: String = self.query(
            "INSERT INTO repo_rules.repositories
                 (repo_id, locator, local_path, source_sha, active_generation, revision, conflict_count)
             VALUES (gen_random_uuid(), 'local:' || $1, $1, $2, gen_random_uuid(), 1, $3)
             RETURNING repo_id::text",
            &[
                &repo,
                &export["source_sha"].as_str().unwrap_or(""),
                &(export["conflicts"].as_array().map_or(0, Vec::len) as i32),
            ],
        )[0]
        .get(0);
        let mut sources: Vec<(String, String)> = Vec::new();
        let files = export["files"].as_array().cloned().unwrap_or_default();
        for (position, file) in files.iter().enumerate() {
            let mut extra = Map::new();
            extra.insert("file_position".into(), Value::from(position));
            if file["errors"].as_array().is_some_and(|e| !e.is_empty()) {
                extra.insert("errors".into(), file["errors"].clone());
            }
            let path = file["path"].as_str().unwrap().to_string();
            let key = self.source(&repo_id, &path, file["tier"].as_i64().unwrap(), true, extra);
            sources.push((path, key));
        }
        let rules = export["rules"].as_array().cloned().unwrap_or_default();
        for (position, rule) in rules.iter().enumerate() {
            let path = rule["source_file"].as_str().unwrap_or("").to_string();
            let key = match sources.iter().find(|(p, _)| *p == path) {
                Some((_, key)) => key.clone(),
                None => {
                    let key = self.source(&repo_id, &path, 0, false, Map::new());
                    sources.push((path, key.clone()));
                    key
                }
            };
            self.rule(&repo_id, &key, position as i64, rule);
        }
        repo_id
    }

    fn source(
        &self,
        repo: &str,
        path: &str,
        tier: i64,
        discovered: bool,
        extra: Map<String, Value>,
    ) -> String {
        self.query(
            "INSERT INTO repo_rules.sources (repo_id, source_key, path, tier, discovered, extra)
             VALUES ($1::text::uuid, gen_random_uuid(), $2, $3::int, $4, $5::text::jsonb)
             RETURNING source_key::text",
            &[
                &repo,
                &path,
                &(tier as i32),
                &discovered,
                &Value::Object(extra).to_string(),
            ],
        )[0]
        .get(0)
    }

    fn rule(&self, repo: &str, source: &str, position: i64, rule: &Value) {
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
        let key: String = self.query(
            "INSERT INTO repo_rules.rules (repo_id, rule_key, external_id, source_key, title,
                 description, category, scope, severity, directory, position, pinned, removed,
                 extraction_fingerprint, extra)
             VALUES ($1::text::uuid, gen_random_uuid(), $2, $3::text::uuid, $4, $5, $6, $7, $8, $9,
                 $10, $11, $12, $13, $14::text::jsonb)
             RETURNING rule_key::text",
            &[
                &repo,
                &text("id", ""),
                &source,
                &text("title", ""),
                &text("description", ""),
                &text("category", "best_practice"),
                &text("scope", "repo"),
                &text("severity", "should"),
                &text("directory", ""),
                &position,
                &pinned,
                &rule["removed"].as_bool().unwrap_or(false),
                &fingerprint,
                &Value::Object(extra).to_string(),
            ],
        )[0]
        .get(0);
        for (table, column, values) in [
            ("rule_tasks", "task", list("tasks", &[])),
            ("rule_languages", "language", list("languages", &["all"])),
        ] {
            for (position, value) in values.iter().enumerate() {
                self.query(
                    &format!(
                        "INSERT INTO repo_rules.{table} (repo_id, rule_key, position, {column})
                         VALUES ($1::text::uuid, $2::text::uuid, $3, $4)"
                    ),
                    &[&repo, &key, &(position as i32), value],
                );
            }
        }
    }
}

impl Drop for TestStore {
    fn drop(&mut self) {
        let _ = std::panic::catch_unwind(|| {
            batch(
                &self.admin,
                &format!("DROP DATABASE {} WITH (FORCE)", self.name),
            );
        });
    }
}
