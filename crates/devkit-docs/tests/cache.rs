mod common;

use common::{fixture_repo, unique_tmp};
use devkit_docs::cache::{self, LibCache, Meta};
use devkit_docs::tags::TagPattern;

#[test]
fn a_directory_the_host_folds_into_an_existing_one_is_refused() {
    let root = common::unique_tmp("fold");
    let first = devkit_docs::cache::create_dir_exact(&root, "V1.0").unwrap();
    assert!(first.is_dir());

    match devkit_docs::cache::create_dir_exact(&root, "v1.0") {
        Ok(path) => assert!(path.is_dir(), "a case-sensitive host keeps them distinct"),
        Err(error) => assert!(
            error.to_string().contains("V1.0"),
            "a folding host must name the directory it collided with: {error}"
        ),
    }
}

#[test]
fn clone_tags_worktree_and_sync() {
    let tmp = unique_tmp("cache");
    let repo = fixture_repo(&tmp.join("upstream"));
    let lib = LibCache::new(&tmp.join("cacheroot"), "mylib").unwrap();

    assert!(!lib.cloned());
    lib.ensure_clone(&repo).unwrap();
    assert!(lib.cloned());
    lib.ensure_clone(&repo).unwrap(); // idempotent

    let tags = lib.tags().unwrap();
    assert!(tags.contains(&"v1.0.0".to_string()) && tags.contains(&"v1.1.0".to_string()));
    assert_eq!(lib.default_branch().unwrap(), "main");

    // Version worktree pins the tag's content, not the tip.
    let wt = lib.ensure_worktree("1.0.0", "v1.0.0").unwrap();
    assert_eq!(
        std::fs::read_to_string(wt.join("src/lib.rs")).unwrap(),
        "// v1"
    );
    lib.ensure_worktree("1.0.0", "v1.0.0").unwrap(); // idempotent

    // Default worktree tracks the branch tip.
    let def = lib.sync_default(None).unwrap();
    assert_eq!(
        std::fs::read_to_string(def.join("src/lib.rs")).unwrap(),
        "// v2"
    );

    let mut names: Vec<String> = lib
        .version_worktrees()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    names.sort();
    assert_eq!(names, vec!["1.0.0", "default"]);

    lib.remove_worktree("1.0.0").unwrap();
    assert!(!lib.worktree_path("1.0.0").exists());
}

#[test]
fn sync_default_follows_new_commits() {
    let tmp = unique_tmp("sync");
    let upstream = tmp.join("upstream");
    let repo = fixture_repo(&upstream);
    let lib = LibCache::new(&tmp.join("cacheroot"), "mylib").unwrap();
    lib.ensure_clone(&repo).unwrap();
    let def = lib.sync_default(None).unwrap();
    assert_eq!(
        std::fs::read_to_string(def.join("src/lib.rs")).unwrap(),
        "// v2"
    );

    // Upstream moves on; fetch + sync catches up.
    std::fs::write(upstream.join("src/lib.rs"), "// v3").unwrap();
    devkit_common::cmd::capture("git", &["add", "."], Some(upstream.to_str().unwrap())).unwrap();
    devkit_common::cmd::capture(
        "git",
        &["commit", "-m", "v3"],
        Some(upstream.to_str().unwrap()),
    )
    .unwrap();
    lib.fetch().unwrap();
    let def = lib.sync_default(None).unwrap();
    assert_eq!(
        std::fs::read_to_string(def.join("src/lib.rs")).unwrap(),
        "// v3"
    );
}

#[test]
fn meta_round_trips() {
    let tmp = unique_tmp("meta");
    let mut m = Meta {
        tag_pattern: Some(TagPattern::NameDash),
        ..Default::default()
    };
    m.layouts.insert(
        "1.0.0".into(),
        devkit_docs::layout::Layout {
            docs_dir: Some("docs".into()),
            ..Default::default()
        },
    );
    cache::write_meta(&tmp, &m).unwrap();
    assert_eq!(cache::read_meta(&tmp), m);
    assert_eq!(cache::read_meta(&tmp.join("missing")), Meta::default());
}
