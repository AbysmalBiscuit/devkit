//! Every rule source, fed the same records, gives the same rules and the same
//! answers to the same queries. The records are one index in the form
//! `repo-rules-agent export-json` writes: the file source reads it as is, and
//! the SQLite and Postgres sources read it imported into a store of their own,
//! the Postgres one through its cache.
//! A new source joins [`sources`] fed from the same export.
//!
//! The Postgres member needs `DEVKIT_TEST_POSTGRES_URL`; without it the test
//! compares the others.

#[path = "common/pgstore.rs"]
mod pgstore;
#[path = "common/sqlitestore.rs"]
mod sqlitestore;

use std::{path::Path, time::Duration};

use devkit_common::tls::Trust;
use devkit_rules::{
    cache::{CacheKey, RuleCache},
    index::FileSource,
    model::RuleIndex,
    postgres::{Database, PostgresSource},
    query::{self, Filter},
    remote::Remote,
    source::{CachedSource, RuleSource, Source},
    sqlite::SqliteSource,
    vocab::{Scope, Severity, Task},
};
use pgstore::TestStore;
use serde_json::Value;

const EXPORT: &str = include_str!("fixtures/export.json");

/// Each source over the export, named, with what keeps it alive.
fn sources(dir: &Path) -> (Option<TestStore>, Vec<(&'static str, Source)>) {
    let export: Value = serde_json::from_str(EXPORT).unwrap();
    let index = dir.join("index.json");
    std::fs::write(&index, EXPORT).unwrap();
    let store = dir.join("index.sqlite");
    sqlitestore::import(&store, &export);
    let mut sources = vec![
        ("file", Source::Json(FileSource::at(index))),
        (
            "sqlite",
            Source::Sqlite(SqliteSource::at(store, Path::new("/srv/acme"))),
        ),
    ];
    let Some(store) = TestStore::create() else {
        return (None, sources);
    };
    let repo = store.import(&export);
    let db = Database::new(
        &store.url,
        Duration::from_secs(10),
        &Trust::default(),
        "rules database",
    )
    .unwrap();
    let postgres =
        PostgresSource::new(std::sync::Arc::new(db), Some(&repo), Path::new("/srv/acme"));
    let cache = RuleCache::at_state_dir(dir, CacheKey {
        kind: "postgres",
        repository: repo.clone(),
    });
    let cached = CachedSource::new(cache, Remote::Postgres(postgres));
    cached.refresh(false).unwrap();
    sources.push(("postgres", Source::Cached(cached)));
    (Some(store), sources)
}

/// What a reader sees of an index: every live rule in order, and each
/// discovered file's path, tier and failed chunks.
fn seen(index: &RuleIndex) -> Value {
    let files: Vec<Value> = index
        .files
        .iter()
        .map(|f| serde_json::json!([f.path, f.tier, f.errors]))
        .collect();
    serde_json::json!({ "rules": index.rules, "files": files })
}

/// The ids a query returns, in rank order.
fn answer(index: &RuleIndex, filter: &Filter, topics: &[String]) -> Vec<String> {
    query::rank(index, query::matching(index, filter), topics)
        .iter()
        .map(|rule| rule.id.clone())
        .collect()
}

fn filters() -> Vec<(Filter, Vec<String>)> {
    let path = |p: &str| vec![p.to_string()];
    vec![
        (Filter::default(), Vec::new()),
        (Filter::default(), vec!["security".to_string()]),
        (
            Filter {
                task: Some(Task::CodeGeneration),
                ..Filter::default()
            },
            Vec::new(),
        ),
        (
            Filter {
                task: Some(Task::CodeReview),
                language: Some("python".to_string()),
                ..Filter::default()
            },
            Vec::new(),
        ),
        (
            Filter {
                scope: Some(Scope::Directory),
                ..Filter::default()
            },
            Vec::new(),
        ),
        (
            Filter {
                severity: Some(Severity::Must),
                ..Filter::default()
            },
            Vec::new(),
        ),
        (
            Filter {
                task: Some(Task::CodeGeneration),
                language: Some("rust".to_string()),
                min_severity: Some(Severity::Should),
                paths: path("crates/foo/bar/lib.rs"),
                ..Filter::default()
            },
            Vec::new(),
        ),
    ]
}

#[test]
fn every_source_reads_the_same_records_the_same_way() {
    let dir = tempfile::tempdir().unwrap();
    let (_store, sources) = sources(dir.path());
    let read: Vec<(&str, RuleIndex)> = sources
        .iter()
        .map(|(name, source)| (*name, source.read().unwrap().unwrap()))
        .collect();
    let (first, reference) = &read[0];
    let ids: Vec<&str> = reference.rules.iter().map(|r| r.id.as_str()).collect();
    assert!(
        !ids.contains(&"r-gone"),
        "a tombstone is never read: {ids:?}"
    );
    assert_eq!(
        ids.iter().filter(|id| **id == "r-dup").count(),
        2,
        "{ids:?}"
    );
    for (name, index) in &read[1..] {
        assert_eq!(seen(index), seen(reference), "{name} against {first}");
        for (filter, topics) in filters() {
            assert_eq!(
                answer(index, &filter, &topics),
                answer(reference, &filter, &topics),
                "{name} against {first} for {filter:?} {topics:?}"
            );
        }
    }
}
