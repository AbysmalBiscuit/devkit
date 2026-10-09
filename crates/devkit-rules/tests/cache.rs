//! The SQLite cache of a remote source's rules: what it keeps, whose rules it
//! answers for, and what a reader sees while it is replaced.

use std::path::Path;

use devkit_rules::{
    cache::{CacheKey, RuleCache},
    model::RuleIndex,
};
use serde_json::Value;

const INDEX: &str = include_str!("fixtures/index.json");

fn index() -> RuleIndex {
    serde_json::from_str(INDEX).unwrap()
}

fn key(repository: &str) -> CacheKey {
    CacheKey {
        kind: "postgres",
        repository: repository.to_string(),
        source: "db.example:5432/rules".to_string(),
    }
}

fn cache(dir: &Path, repository: &str) -> RuleCache {
    RuleCache::new(dir.join("rules-cache").join("c.sqlite"), key(repository))
}

fn value(index: &RuleIndex) -> Value {
    serde_json::to_value(index).unwrap()
}

#[test]
fn round_trips_the_index() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache(dir.path(), "a");
    cache.write(7, &index()).unwrap();
    assert_eq!(value(&cache.read().unwrap().unwrap()), value(&index()));
    let meta = cache.meta().unwrap();
    assert_eq!(meta.revision, 7);
    assert!(meta.pulled_at > 0);
}

#[test]
fn missing_file_reads_as_none() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache(dir.path(), "a");
    assert!(cache.meta().is_none());
    assert!(cache.read().unwrap().is_none());
    assert!(!cache.path().exists());
}

#[test]
fn meta_for_another_repository_reads_as_no_cache() {
    let dir = tempfile::tempdir().unwrap();
    cache(dir.path(), "a").write(1, &index()).unwrap();
    let other = cache(dir.path(), "b");
    assert!(other.meta().is_none());
    assert!(other.read().unwrap().is_none());
    let other_kind = RuleCache::new(dir.path().join("rules-cache").join("c.sqlite"), CacheKey {
        kind: "supabase",
        repository: "a".to_string(),
        source: "db.example:5432/rules".to_string(),
    });
    assert!(other_kind.meta().is_none());
    assert!(other_kind.read().unwrap().is_none());
}

#[test]
fn meta_from_another_remote_reads_as_no_cache() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rules-cache").join("c.sqlite");
    let from = |source: &str| {
        RuleCache::new(path.clone(), CacheKey {
            source: source.to_string(),
            ..key("a")
        })
    };
    from("db.example:5432/rules").write(1, &index()).unwrap();
    let other = from("other.example:5432/rules");
    assert!(other.meta().is_none());
    assert!(other.read().unwrap().is_none());
    other.write(2, &index()).unwrap();
    assert_eq!(other.meta().unwrap().revision, 2);
    assert!(from("db.example:5432/rules").meta().is_none());
}

#[test]
fn unknown_format_reads_as_none() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache(dir.path(), "a");
    cache.write(1, &index()).unwrap();
    rusqlite::Connection::open(cache.path())
        .unwrap()
        .execute("UPDATE meta SET format = 99", [])
        .unwrap();
    assert!(cache.meta().is_none());
    assert!(cache.read().unwrap().is_none());
    cache.write(2, &index()).unwrap();
    assert!(cache.read().unwrap().is_some());
    assert_eq!(cache.meta().unwrap().revision, 2);
}

#[test]
fn at_state_dir_names_the_file_by_kind_and_repository() {
    let dir = tempfile::tempdir().unwrap();
    let cache = RuleCache::at_state_dir(dir.path(), key("0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a10"));
    assert_eq!(
        cache.path(),
        dir.path()
            .join("rules-cache")
            .join("postgres-0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a10.sqlite")
    );
}

#[test]
fn concurrent_refreshes_leave_one_whole_index() {
    let dir = tempfile::tempdir().unwrap();
    let first = index();
    let mut second = index();
    second.rules.truncate(2);
    second.rules[0].title = "Changed".to_string();
    let (a, b) = (value(&first), value(&second));
    cache(dir.path(), "a").write(0, &first).unwrap();
    std::thread::scope(|s| {
        for (n, written) in [(1, &first), (2, &second)] {
            let path = dir.path();
            s.spawn(move || {
                let cache = cache(path, "a");
                for _ in 0..20 {
                    cache.write(n, written).unwrap();
                }
            });
        }
        let path = dir.path();
        let (a, b) = (&a, &b);
        s.spawn(move || {
            let cache = cache(path, "a");
            for _ in 0..50 {
                let read = value(&cache.read().unwrap().expect("a cache"));
                assert!(read == *a || read == *b, "a torn read: {read}");
            }
        });
    });
}
