mod common;

use common::unique_tmp;
use devkit_docs::manifest::{Ecosystem, LibEntry};
use devkit_docs::refs::{self, RefStore};
use std::collections::BTreeSet;

// Bug #1 regression: pruning from project A must NOT delete project B's
// overlay-only lib worktree.
#[test]
fn prune_preserves_other_projects_overlay_lib() {
    let tmp = unique_tmp("xproj");
    let cache_root = tmp.join("cache");
    // Fake worktree dirs (no git needed): libX used by A, libY used by B.
    for p in ["libX/1.0.0", "libX/default", "libY/2.0.0", "libY/default"] {
        std::fs::create_dir_all(cache_root.join(p)).unwrap();
    }
    // Global manifest registers only libX; libY is a B-only overlay.
    let global = tmp.join("docs.toml");
    std::fs::write(
        &global,
        "[[libs]]\nname='libX'\necosystem='rust'\nrepo='rx'\n",
    )
    .unwrap();
    // Project A: pins libX@1.0.0 via Cargo.lock; sees only libX.
    let proj_a = tmp.join("A");
    std::fs::create_dir_all(&proj_a).unwrap();
    std::fs::write(proj_a.join("devkit.toml"), "[defaults]\n").unwrap();
    std::fs::write(
        proj_a.join("Cargo.lock"),
        "version=4\n[[package]]\nname='libX'\nversion='1.0.0'\n",
    )
    .unwrap();
    // Project B: overlay-registers libY, pins libY@2.0.0.
    let proj_b = tmp.join("B");
    std::fs::create_dir_all(&proj_b).unwrap();
    std::fs::write(
        proj_b.join("devkit.toml"),
        "[[docs.libs]]\nname='libY'\necosystem='rust'\nrepo='ry'\n",
    )
    .unwrap();
    std::fs::write(
        proj_b.join("Cargo.lock"),
        "version=4\n[[package]]\nname='libY'\nversion='2.0.0'\n",
    )
    .unwrap();
    // Registry: A→libX@1.0.0 (live), B→libY@2.0.0 (live).
    let store = RefStore::at(&cache_root);
    store
        .commit(|d| {
            d.record(proj_a.to_str().unwrap(), "libX", "1.0.0");
            d.record(proj_b.to_str().unwrap(), "libY", "2.0.0");
            Ok(())
        })
        .unwrap();

    // Prune "as if from A": A's manifest sees only libX.
    let a_libs: BTreeSet<String> = ["libX".to_string()].into_iter().collect();
    let snapshot = store.snapshot();
    let plan = refs::plan_for_cache(&cache_root, &snapshot, &a_libs, Some(&global)).unwrap();

    // libY/2.0.0 must survive: it is still referenced by the live project B.
    assert!(
        !plan
            .delete
            .contains(&("libY".to_string(), "2.0.0".to_string())),
        "prune from A wrongly deleted B's overlay lib worktree: {:?}",
        plan.delete
    );
    assert!(
        plan.keep
            .iter()
            .any(|r| r.lib == "libY" && r.version == "2.0.0")
    );
    // libX stays too.
    assert!(
        !plan
            .delete
            .contains(&("libX".to_string(), "1.0.0".to_string()))
    );
}

// Regression: a LIVE project whose devkit.toml fails to parse must not have
// its checkouts silently reclaimed — a read/parse error is not "unreferenced".
#[test]
fn prune_keeps_rows_for_a_project_with_an_unreadable_manifest() {
    let tmp = unique_tmp("brokenmanifest");
    let cache_root = tmp.join("cache");
    for p in ["libZ/1.0.0", "libZ/default"] {
        std::fs::create_dir_all(cache_root.join(p)).unwrap();
    }
    let global = tmp.join("docs.toml");
    std::fs::write(&global, "").unwrap();
    // Live project B with a MALFORMED devkit.toml.
    let proj_b = tmp.join("B");
    std::fs::create_dir_all(&proj_b).unwrap();
    std::fs::write(proj_b.join("devkit.toml"), "this = = not valid toml [[[").unwrap();
    let store = RefStore::at(&cache_root);
    store
        .commit(|d| {
            d.record(proj_b.to_str().unwrap(), "libZ", "1.0.0");
            Ok(())
        })
        .unwrap();

    let libs: BTreeSet<String> = BTreeSet::new();
    let snapshot = store.snapshot();
    let plan = refs::plan_for_cache(&cache_root, &snapshot, &libs, Some(&global)).unwrap();

    assert!(
        !plan
            .delete
            .contains(&("libZ".to_string(), "1.0.0".to_string())),
        "a live project's worktree was deleted because its manifest failed to parse: {:?}",
        plan.delete
    );
    assert!(
        plan.keep
            .iter()
            .any(|r| r.lib == "libZ" && r.version == "1.0.0")
    );
}

#[test]
fn a_scoped_library_is_one_directory_and_prune_leaves_it_alone() {
    let root = unique_tmp("scoped");
    let lib = devkit_docs::cache::LibCache::new(&root, "@types/node").unwrap();
    assert!(lib.dir.ends_with("@types~node"));

    std::fs::create_dir_all(root.join("@scope/pkg")).unwrap();
    let stray = devkit_docs::cache::LibCache::new(&root, "@scope").unwrap();
    assert!(stray.version_worktrees().is_empty());
}

#[test]
fn prune_never_schedules_a_stray_scoped_parent_for_deletion() {
    let root = unique_tmp("scoped-prune");
    let stray = root.join("@scope/pkg");
    std::fs::create_dir_all(&stray).unwrap();

    let plan = refs::plan_for_cache(&root, &Default::default(), &BTreeSet::new(), None).unwrap();

    assert!(
        plan.removable_libs.is_empty(),
        "a cache-root directory without repo.git must not reach docm prune's recursive deletion: {:?}",
        plan.removable_libs
    );
    assert!(stray.is_dir());
}

#[test]
fn prune_preserves_every_ref_named_checkout_recorded_by_resolve() {
    let tmp = unique_tmp("live-ref-checkouts");
    let repo = common::fixture_repo(&tmp.join("upstream"));
    let cache_root = tmp.join("cache");
    let project = tmp.join("project");
    let manifest_path = project.join("devkit.toml");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(&manifest_path, "[defaults]\n").unwrap();
    std::fs::write(
        project.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"locked\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();

    let entries = [
        LibEntry {
            name: "explicit".into(),
            repo: Some(repo.clone()),
            r#ref: Some("v1.0.0".into()),
            ..Default::default()
        },
        LibEntry {
            name: "git-default".into(),
            ecosystem: Some(Ecosystem::Git),
            repo: Some(repo.clone()),
            ..Default::default()
        },
        LibEntry {
            name: "locked".into(),
            ecosystem: Some(Ecosystem::Rust),
            repo: Some(repo),
            ..Default::default()
        },
    ];
    for entry in &entries {
        devkit_docs::manifest::upsert_project(&manifest_path, entry, &cache_root).unwrap();
    }

    let resolved: Vec<_> = entries
        .iter()
        .map(|entry| devkit_docs::resolve::resolve(entry, &project, &cache_root).unwrap())
        .collect();
    assert_eq!(
        resolved
            .iter()
            .map(|resolution| resolution.worktree.as_str())
            .collect::<Vec<_>>(),
        ["v1.0.0", "main", "v1.0.0"]
    );

    let manifest_libs = entries
        .iter()
        .map(|entry| entry.name.clone())
        .collect::<BTreeSet<_>>();
    let snapshot = RefStore::at(&cache_root).snapshot();
    let global = tmp.join("docs.toml");
    std::fs::write(&global, "").unwrap();
    let plan = refs::plan_for_cache(&cache_root, &snapshot, &manifest_libs, Some(&global)).unwrap();
    let scheduled = plan.delete.clone();

    for (lib, worktree) in &plan.delete {
        devkit_docs::cache::LibCache::new(&cache_root, lib)
            .unwrap()
            .remove_worktree(worktree)
            .unwrap();
    }

    for resolution in &resolved {
        assert!(
            resolution.path.is_dir(),
            "prune removed the live {} checkout {} via {:?}",
            resolution.name,
            resolution.worktree,
            scheduled
        );
    }
    assert!(
        scheduled.is_empty(),
        "prune scheduled live ref-named checkouts: {scheduled:?}"
    );
}
