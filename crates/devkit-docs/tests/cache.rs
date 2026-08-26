mod common;

use common::fixture_repo;
use devkit_docs::cache::{self, LibCache, Meta, WorktreeMeta};
use devkit_docs::tags::TagPattern;
use std::path::Path;

/// A follow-up git operation against an already-built `fixture_repo`.
fn git(args: &[&str], dir: &Path) {
    devkit_common::git::Git::fixture(dir)
        .args(args.iter().copied())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
}

#[test]
fn a_directory_the_host_folds_into_an_existing_one_is_refused() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let first = devkit_docs::cache::create_dir_exact(root, "V1.0").unwrap();
    assert!(first.is_dir());

    match devkit_docs::cache::create_dir_exact(root, "v1.0") {
        Ok(path) => assert!(path.is_dir(), "a case-sensitive host keeps them distinct"),
        Err(error) => assert!(
            error.to_string().contains("V1.0"),
            "a folding host must name the directory it collided with: {error}"
        ),
    }
}

#[test]
fn clone_tags_and_ref_named_worktrees() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let tmp = tmp_dir.path();
    let repo = fixture_repo(&tmp.join("upstream"));
    let lib = LibCache::new(&tmp.join("cacheroot"), "mylib").unwrap();
    let mut meta = Meta::default();

    assert!(!lib.cloned());
    lib.ensure_clone(&repo, &mut meta).unwrap();
    assert!(lib.cloned());
    assert_eq!(meta.origin.as_deref(), Some(repo.as_str()));
    lib.ensure_clone(&repo, &mut meta).unwrap();

    let tags = lib.tags().unwrap();
    assert!(tags.contains(&"v1.0.0".to_string()) && tags.contains(&"v1.1.0".to_string()));
    assert_eq!(lib.default_branch().unwrap(), "main");

    let (_, tag_commit) = lib.resolve_ref("v1.0.0").unwrap();
    let (wt, repaired) = lib.ensure_at("v1.0.0", &tag_commit).unwrap();
    assert!(!repaired);
    assert_eq!(
        std::fs::read_to_string(wt.join("src/lib.rs")).unwrap(),
        "// v1"
    );
    assert!(!lib.ensure_at("v1.0.0", &tag_commit).unwrap().1);

    let (_, main_commit) = lib.resolve_ref("main").unwrap();
    let (main, _) = lib.ensure_at("main", &main_commit).unwrap();
    assert_eq!(
        std::fs::read_to_string(main.join("src/lib.rs")).unwrap(),
        "// v2"
    );

    let mut names: Vec<String> = lib
        .version_worktrees()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    names.sort();
    assert_eq!(names, vec!["main", "v1.0.0"]);

    lib.remove_worktree_locked("v1.0.0").unwrap();
    assert!(!lib.worktree_path("v1.0.0").exists());
}

#[test]
fn a_branch_checkout_follows_new_commits_after_fetch() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let tmp = tmp_dir.path();
    let upstream = tmp.join("upstream");
    let repo = fixture_repo(&upstream);
    let lib = LibCache::new(&tmp.join("cacheroot"), "mylib").unwrap();
    let mut meta = Meta::default();
    lib.ensure_clone(&repo, &mut meta).unwrap();
    let (_, first_commit) = lib.resolve_ref("main").unwrap();
    let (main, _) = lib.ensure_at("main", &first_commit).unwrap();
    assert_eq!(
        std::fs::read_to_string(main.join("src/lib.rs")).unwrap(),
        "// v2"
    );

    std::fs::write(upstream.join("src/lib.rs"), "// v3").unwrap();
    git(&["add", "."], &upstream);
    git(&["commit", "-m", "v3"], &upstream);
    lib.fetch().unwrap();
    let (_, next_commit) = lib.resolve_ref("main").unwrap();
    let (main, repaired) = lib.ensure_at("main", &next_commit).unwrap();
    assert!(repaired);
    assert_eq!(
        std::fs::read_to_string(main.join("src/lib.rs")).unwrap(),
        "// v3"
    );
}

#[test]
fn an_existing_clone_without_origin_bootstraps_from_the_bare_repo() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let tmp = tmp_dir.path();
    let repo = fixture_repo(&tmp.join("upstream"));
    let lib = LibCache::new(&tmp.join("cacheroot"), "mylib").unwrap();
    let mut initial = Meta::default();
    lib.ensure_clone(&repo, &mut initial).unwrap();

    let mut legacy = Meta::default();
    lib.ensure_clone(&repo, &mut legacy).unwrap();

    assert_eq!(legacy.origin.as_deref(), Some(repo.as_str()));
}

#[test]
fn a_forty_hex_pin_is_only_an_object_name() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let tmp = tmp_dir.path();
    let upstream = tmp.join("upstream");
    let repo = fixture_repo(&upstream);
    let hex = "0000000000000000000000000000000000000000";
    git(&["tag", hex, "v1.0.0"], &upstream);
    let lib = LibCache::new(&tmp.join("cacheroot"), "mylib").unwrap();
    let mut meta = Meta::default();
    lib.ensure_clone(&repo, &mut meta).unwrap();

    let error = lib.resolve_ref(hex).unwrap_err();

    assert!(
        error.to_string().contains(hex),
        "the missing object error must name the pin: {error}"
    );
}

#[test]
fn meta_round_trips() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let tmp = tmp_dir.path();
    let mut m = Meta {
        origin: Some("https://example.test/up.git".into()),
        tag_pattern: Some(TagPattern::LeafDash),
        ..Default::default()
    };
    m.layouts.insert(
        "1.0.0".into(),
        devkit_docs::layout::Layout {
            docs_dir: Some("docs".into()),
            ..Default::default()
        },
    );
    m.worktrees.insert(
        "v1.0.0".into(),
        WorktreeMeta {
            raw_ref: "v1.0.0".into(),
            resolved_ref: "refs/tags/v1.0.0".into(),
            commit: "0123456789012345678901234567890123456789".into(),
        },
    );
    cache::write_meta(tmp, &m).unwrap();
    assert_eq!(cache::read_meta(tmp).unwrap(), m);
    assert_eq!(
        cache::read_meta(&tmp.join("missing")).unwrap(),
        Meta::default()
    );
}

#[test]
fn meta_without_identity_fields_keeps_backward_compatible_defaults() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let tmp = tmp_dir.path();
    std::fs::write(tmp.join("meta.toml"), "tag_pattern = \"v\"\n").unwrap();

    let meta = cache::read_meta(tmp).unwrap();

    assert_eq!(meta.tag_pattern, Some(TagPattern::V));
    assert_eq!(meta.origin, None);
    assert!(meta.worktrees.is_empty());
}

/// A `tag_pattern` written by 0.12.x whose variant no longer exists. Reading it
/// as `Meta::default()` would discard the origin, the layouts and every
/// checkout's commit record, and the next `write_meta` would then persist that
/// loss over the file.
const ZERO_TWELVE_META: &str = "origin = \"https://example.test/up.git\"\n\
                                tag_pattern = \"name-dash-v\"\n\
                                \n\
                                [worktrees.\"v1.0.0\"]\n\
                                raw_ref = \"v1.0.0\"\n\
                                resolved_ref = \"refs/tags/v1.0.0\"\n\
                                commit = \"0123456789012345678901234567890123456789\"\n";

#[test]
fn a_meta_from_an_older_docm_is_an_error_rather_than_an_empty_one() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let tmp = tmp_dir.path();
    let path = tmp.join("meta.toml");
    std::fs::write(&path, ZERO_TWELVE_META).unwrap();

    let error = cache::read_meta(tmp).expect_err(
        "a meta.toml that does not parse must not read as a cache with no recorded state",
    );

    let report = format!("{error:#}");
    assert!(
        report.contains(&path.display().to_string()),
        "the error must name the file to delete: {report}"
    );
    assert!(
        report.contains("delete"),
        "the error must state the recovery: {report}"
    );
}

#[test]
fn resolve_leaves_a_meta_it_cannot_parse_on_disk() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let tmp = tmp_dir.path();
    let repo = fixture_repo(&tmp.join("upstream"));
    let cache_root = tmp.join("cacheroot");
    let project = tmp.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("devkit.toml"), "[defaults]\n").unwrap();
    let entry = devkit_docs::manifest::LibEntry {
        name: "mylib".into(),
        repo: Some(repo),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    // A real cache first, so the resolve below gets past the clone and reaches
    // the write — a fixture with no bare clone would prove nothing about it.
    devkit_docs::resolve::resolve(
        &entry,
        &project,
        &cache_root,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();
    let path = cache_root.join("mylib/meta.toml");
    assert!(path.is_file(), "fixture must produce a meta.toml to shadow");
    std::fs::write(&path, ZERO_TWELVE_META).unwrap();

    let result = devkit_docs::resolve::resolve(
        &entry,
        &project,
        &cache_root,
        &devkit_docs::resolve::Options::default(),
    );

    assert!(
        result.is_err(),
        "resolve must refuse a meta.toml it cannot parse instead of rebuilding it from nothing"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        ZERO_TWELVE_META,
        "resolve overwrote the meta.toml it could not read"
    );
}
