//! A remote source read through its cache: when a refresh pulls, what a
//! failed one leaves, and what reads and edits see.

use std::{
    cell::{Cell, RefCell},
    path::Path,
};

use devkit_rules::{
    cache::{CacheKey, RuleCache},
    edit::Fields,
    model::RuleIndex,
    remote::{FakeRemote, Remote},
    source::{CachedSource, RuleSource},
};

const INDEX: &str = include_str!("fixtures/index.json");

fn index() -> RuleIndex {
    serde_json::from_str(INDEX).unwrap()
}

fn source(dir: &Path) -> CachedSource {
    source_at(dir, "/clones/one")
}

/// A source describing the clone at `checkout`, its cache under `dir`.
fn source_at(dir: &Path, checkout: &str) -> CachedSource {
    let cache = RuleCache::at_state_dir(dir, CacheKey {
        kind: "postgres",
        repository: "a".to_string(),
        source: "fake".to_string(),
    });
    CachedSource::new(
        cache,
        Remote::Fake(FakeRemote {
            checkout: checkout.to_string(),
            revision: Cell::new(1),
            index: RefCell::new(index()),
            fail: Cell::new(false),
            pulls: Cell::new(0),
        }),
    )
}

fn fake(source: &CachedSource) -> &FakeRemote {
    match source.remote() {
        Remote::Fake(fake) => fake,
        _ => unreachable!("a fake remote"),
    }
}

fn titles(index: &RuleIndex) -> Vec<String> {
    index.rules.iter().map(|r| r.title.clone()).collect()
}

#[test]
fn unchanged_revision_pulls_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    let first = source.refresh(false).unwrap();
    assert!(first.pulled);
    assert_eq!(first.revision, 1);
    assert_eq!(first.rules, index().rules.len());
    let second = source.refresh(false).unwrap();
    assert!(!second.pulled);
    assert_eq!(second.rules, index().rules.len());
    assert_eq!(fake(&source).pulls.get(), 1);
}

#[test]
fn changed_revision_replaces_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    source.refresh(false).unwrap();
    let fake = fake(&source);
    fake.revision.set(2);
    fake.index.borrow_mut().rules.truncate(1);
    assert!(source.refresh(false).unwrap().pulled);
    assert_eq!(source.read().unwrap().unwrap().rules.len(), 1);
    assert_eq!(source.cache().meta().unwrap().revision, 2);
}

#[test]
fn failed_pull_keeps_the_old_cache() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    source.refresh(false).unwrap();
    let fake = fake(&source);
    fake.revision.set(2);
    fake.index.borrow_mut().rules.truncate(1);
    fake.fail.set(true);
    source.refresh(false).unwrap_err();
    assert_eq!(titles(&source.read().unwrap().unwrap()), titles(&index()));
}

#[test]
fn edit_refreshes_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    source.refresh(false).unwrap();
    let id = index().rules[0].id.clone();
    let fields = Fields {
        title: Some("Renamed".to_string()),
        ..Fields::default()
    };
    source.edit(&id, fields).unwrap();
    assert_eq!(source.read().unwrap().unwrap().rules[0].title, "Renamed");
    assert_eq!(source.cache().meta().unwrap().revision, 2);
}

#[test]
fn no_cache_reads_the_remote_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    assert_eq!(titles(&source.read().unwrap().unwrap()), titles(&index()));
    assert!(!source.cache().path().exists());
}

#[test]
fn cache_another_clone_filled_reads_as_this_clone() {
    let dir = tempfile::tempdir().unwrap();
    source_at(dir.path(), "/clones/one").refresh(false).unwrap();
    let other = source_at(dir.path(), "/clones/two");
    fake(&other).fail.set(true);
    assert_eq!(other.read().unwrap().unwrap().repo, "/clones/two");
}

#[test]
fn force_pulls_at_the_same_revision() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    source.refresh(false).unwrap();
    assert!(source.refresh(true).unwrap().pulled);
    assert_eq!(fake(&source).pulls.get(), 2);
}

#[test]
fn location_names_the_remote_and_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    let location = source.location();
    assert!(location.contains("fake"), "{location}");
    assert!(
        location.contains(&source.cache().path().display().to_string()),
        "{location}"
    );
}
