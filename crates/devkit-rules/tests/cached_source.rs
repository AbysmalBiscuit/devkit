//! A remote source read through its cache: when a refresh pulls, what a
//! failed one leaves, and what reads and edits see. The remote is the
//! `supabase` source over a scripted Data API.

#[path = "common/fakeapi.rs"]
mod fakeapi;

use std::{path::Path, sync::Arc, time::Duration};

use devkit_rules::{
    cache::{CacheKey, RuleCache},
    edit::Fields,
    model::RuleIndex,
    remote::Remote,
    source::{CachedSource, Refresh, RuleSource},
    supabase::SupabaseSource,
};
use devkit_supabase::{Api, Auth, fakehttp::FakeServer};
use serde_json::Value;

const INDEX: &str = include_str!("fixtures/index.json");
const REPO: &str = "0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a10";

fn index() -> RuleIndex {
    serde_json::from_str(INDEX).unwrap()
}

/// The fixture's live rules as `query_rules` returns them.
fn rules() -> Vec<Value> {
    fakeapi::payloads(&index())
}

fn source(dir: &Path, server: &FakeServer) -> CachedSource {
    source_at(dir, server, "/clones/one")
}

/// A source over `server` describing the clone at `checkout`, its cache
/// under `dir`.
fn source_at(dir: &Path, server: &FakeServer, checkout: &str) -> CachedSource {
    let cache = RuleCache::at_state_dir(dir, CacheKey {
        kind: "supabase",
        repository: REPO.to_string(),
        source: "fake".to_string(),
    });
    let api = Api::new(
        &server.url(),
        "repo_rules_api",
        Auth::None,
        Duration::from_secs(5),
        "rules API",
    )
    .unwrap();
    let remote = SupabaseSource::new(Arc::new(api), Some(REPO), Path::new(checkout));
    CachedSource::new(cache, Remote::Supabase(remote))
}

/// How many pulls `server` has served.
fn pulls(server: &FakeServer) -> usize {
    server
        .requests()
        .iter()
        .filter(|r| r.path_and_query.ends_with("/query_rules"))
        .count()
}

fn titles(index: &RuleIndex) -> Vec<String> {
    index.rules.iter().map(|r| r.title.clone()).collect()
}

#[test]
fn unchanged_revision_pulls_nothing() {
    let server = FakeServer::start(vec![
        (200, fakeapi::stats(1)),
        (200, fakeapi::page(1, &rules(), None)),
        (200, fakeapi::stats(1)),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path(), &server);
    let first = source.refresh(Refresh::IfChanged).unwrap();
    assert!(first.pulled);
    assert_eq!(first.revision, 1);
    assert_eq!(first.rules, index().rules.len());
    let second = source.refresh(Refresh::IfChanged).unwrap();
    assert!(!second.pulled);
    assert_eq!(second.rules, index().rules.len());
    assert_eq!(pulls(&server), 1);
}

#[test]
fn changed_revision_replaces_the_cache() {
    let server = FakeServer::start(vec![
        (200, fakeapi::stats(1)),
        (200, fakeapi::page(1, &rules(), None)),
        (200, fakeapi::stats(2)),
        (200, fakeapi::page(2, &rules()[..1], None)),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path(), &server);
    source.refresh(Refresh::IfChanged).unwrap();
    assert!(source.refresh(Refresh::IfChanged).unwrap().pulled);
    assert_eq!(source.read().unwrap().unwrap().rules.len(), 1);
    assert_eq!(source.cache().meta().unwrap().revision, 2);
}

#[test]
fn failed_pull_keeps_the_old_cache() {
    let server = FakeServer::start(vec![
        (200, fakeapi::stats(1)),
        (200, fakeapi::page(1, &rules(), None)),
        (200, fakeapi::stats(2)),
        (500, serde_json::json!({"message": "down"})),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path(), &server);
    source.refresh(Refresh::IfChanged).unwrap();
    source.refresh(Refresh::IfChanged).unwrap_err();
    assert_eq!(titles(&source.read().unwrap().unwrap()), titles(&index()));
    assert_eq!(source.cache().meta().unwrap().revision, 1);
}

#[test]
fn edit_refreshes_the_cache() {
    let mut renamed = rules();
    renamed[0]["title"] = "Renamed".into();
    let key = renamed[0]["rule_key"].as_str().unwrap().to_string();
    let server = FakeServer::start(vec![
        (200, fakeapi::stats(1)),
        (200, fakeapi::page(1, &rules(), None)),
        (200, fakeapi::page(1, &rules(), None)),
        (200, fakeapi::put(2, &key)),
        (200, fakeapi::stats(2)),
        (200, fakeapi::page(2, &renamed, None)),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path(), &server);
    source.refresh(Refresh::IfChanged).unwrap();
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
    let server = FakeServer::start(vec![(200, fakeapi::page(1, &rules(), None))]);
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path(), &server);
    assert_eq!(titles(&source.read().unwrap().unwrap()), titles(&index()));
    assert!(!source.cache().path().exists());
}

#[test]
fn cache_another_clone_filled_reads_as_this_clone() {
    let server = FakeServer::start(vec![
        (200, fakeapi::stats(1)),
        (200, fakeapi::page(1, &rules(), None)),
    ]);
    let down = FakeServer::start(Vec::new());
    let dir = tempfile::tempdir().unwrap();
    source_at(dir.path(), &server, "/clones/one")
        .refresh(Refresh::IfChanged)
        .unwrap();
    let other = source_at(dir.path(), &down, "/clones/two");
    assert_eq!(other.read().unwrap().unwrap().repo, "/clones/two");
    assert!(down.requests().is_empty());
}

#[test]
fn force_pulls_at_the_same_revision() {
    let server = FakeServer::start(vec![
        (200, fakeapi::stats(1)),
        (200, fakeapi::page(1, &rules(), None)),
        (200, fakeapi::stats(1)),
        (200, fakeapi::page(1, &rules(), None)),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path(), &server);
    source.refresh(Refresh::IfChanged).unwrap();
    assert!(source.refresh(Refresh::Pull).unwrap().pulled);
    assert_eq!(pulls(&server), 2);
}

#[test]
fn location_names_the_remote_and_the_cache() {
    let server = FakeServer::start(Vec::new());
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path(), &server);
    let location = source.location();
    assert!(location.contains(&server.url()), "{location}");
    assert!(
        location.contains(&source.cache().path().display().to_string()),
        "{location}"
    );
}
