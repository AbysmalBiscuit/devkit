mod common;

use common::{fixture_repo, unique_tmp};
use devkit_docs::manifest::{Ecosystem, LibEntry};
use devkit_docs::refs::RefStore;
use devkit_docs::resolve::resolve;

#[test]
fn lockfile_version_resolves_to_tag_worktree_and_records_ref() {
    let tmp = unique_tmp("resolve");
    let repo = fixture_repo(&tmp.join("upstream"));
    let cache_root = tmp.join("cache");
    let project = tmp.join("proj");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"mylib\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();

    let entry = LibEntry {
        name: "mylib".into(),
        ecosystem: Some(Ecosystem::Rust),
        repo: Some(repo),
        ..Default::default()
    };
    let r = resolve(&entry, &project, &cache_root).unwrap();
    assert_eq!(r.worktree, "1.0.0");
    assert_eq!(r.version, "1.0.0");
    // Tag content, not tip: v1.0.0 has "// v1".
    assert_eq!(
        std::fs::read_to_string(r.path.join("src/lib.rs")).unwrap(),
        "// v1"
    );
    assert_eq!(r.layout.docs_dir.as_deref(), Some("docs"));
    assert!(r.warnings.is_empty());

    let data = RefStore::at(&cache_root).snapshot();
    assert_eq!(data.rows.len(), 1);
    assert_eq!(data.rows[0].project, project.to_string_lossy());
    assert_eq!(data.rows[0].version, "1.0.0");

    // Cached tag pattern short-circuits the next probe.
    let meta = devkit_docs::cache::read_meta(&cache_root.join("mylib"));
    assert_eq!(meta.tag_pattern, Some(devkit_docs::tags::TagPattern::V));
}

#[test]
fn ref_pin_wins_and_no_lockfile_falls_back_to_default_with_warning() {
    let tmp = unique_tmp("resolve-pin");
    let repo = fixture_repo(&tmp.join("upstream"));
    let cache_root = tmp.join("cache");
    let project = tmp.join("proj");
    std::fs::create_dir_all(&project).unwrap();

    // Manual pin → default worktree at the pin, version label = the pin.
    let pinned = LibEntry {
        name: "mylib".into(),
        repo: Some(repo.clone()),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    let r = resolve(&pinned, &project, &cache_root).unwrap();
    assert_eq!(r.worktree, "default");
    assert_eq!(r.version, "v1.0.0");
    assert_eq!(
        std::fs::read_to_string(r.path.join("src/lib.rs")).unwrap(),
        "// v1"
    );

    // No pin, no lockfile → default branch + a warning.
    let cache2 = tmp.join("cache2");
    let unpinned = LibEntry {
        name: "mylib".into(),
        ecosystem: Some(Ecosystem::Rust),
        repo: Some(repo),
        ..Default::default()
    };
    let r2 = resolve(&unpinned, &project, &cache2).unwrap();
    assert_eq!(r2.worktree, "default");
    assert_eq!(r2.version, "main");
    assert_eq!(r2.warnings.len(), 1);
}

#[test]
fn layout_override_applies_and_meta_caches_detection() {
    let tmp = unique_tmp("resolve-layout");
    let repo = fixture_repo(&tmp.join("upstream"));
    let cache_root = tmp.join("cache");
    let entry = LibEntry {
        name: "mylib".into(),
        repo: Some(repo),
        r#ref: Some("v1.0.0".into()),
        docs_dir: Some("docs/special".into()),
        ..Default::default()
    };
    let r = resolve(&entry, &tmp, &cache_root).unwrap();
    assert_eq!(r.layout.docs_dir.as_deref(), Some("docs/special")); // override wins
    let meta = devkit_docs::cache::read_meta(&cache_root.join("mylib"));
    // meta stores the DETECTED layout (docs), not the override.
    assert_eq!(meta.layouts["default"].docs_dir.as_deref(), Some("docs"));
}

#[test]
fn git_ecosystem_without_ref_falls_back_to_default_with_warning() {
    let tmp = unique_tmp("resolve-git");
    let repo = fixture_repo(&tmp.join("upstream"));
    let cache_root = tmp.join("cache");
    let project = tmp.join("proj");
    std::fs::create_dir_all(&project).unwrap();

    let entry = LibEntry {
        name: "mylib".into(),
        ecosystem: Some(Ecosystem::Git),
        repo: Some(repo),
        ..Default::default()
    };
    let r = resolve(&entry, &project, &cache_root).unwrap();
    assert_eq!(r.worktree, "default");
    assert_eq!(r.version, "main");
    assert_eq!(
        r.warnings.len(),
        1,
        "git fallback must warn: {:?}",
        r.warnings
    );
    assert!(r.warnings[0].contains("no ref pinned"));
}

#[test]
fn lockfile_resolved_from_subdir_records_lockfile_holding_dir() {
    let tmp = unique_tmp("resolve-subdir");
    let repo = fixture_repo(&tmp.join("upstream"));
    let cache_root = tmp.join("cache");
    let project = tmp.join("proj");
    let deep = project.join("crates/app/src");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(
        project.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"mylib\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();

    let entry = LibEntry {
        name: "mylib".into(),
        ecosystem: Some(Ecosystem::Rust),
        repo: Some(repo),
        ..Default::default()
    };
    // Resolve starting DEEP below the lockfile dir; there is no devkit.toml
    // anywhere, so project_root(start) would be `deep`, distinct from the
    // lockfile's holding dir `project`.
    let r = resolve(&entry, &deep, &cache_root).unwrap();
    assert_eq!(r.worktree, "1.0.0");

    let data = RefStore::at(&cache_root).snapshot();
    assert_eq!(data.rows.len(), 1);
    assert_eq!(
        data.rows[0].project,
        project.to_string_lossy(),
        "reference must be attributed to the lockfile's holding dir, not the start dir"
    );
}
