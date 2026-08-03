use devkit_docs::refs::{self, RefStore};
use std::collections::BTreeSet;

fn unique_tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("devkit-docs-prune-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

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
