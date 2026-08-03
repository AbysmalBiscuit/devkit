mod common;

use common::unique_tmp;
use devkit_docs::manifest::{Ecosystem, LibEntry};
use devkit_docs::refs::{self, RefStore};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(60);

fn wait_for(path: &Path) {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    while !path.try_exists().unwrap() {
        assert!(
            Instant::now() <= deadline,
            "timed out waiting for {}",
            path.display()
        );
        std::thread::yield_now();
    }
}

fn wait_for_contention_or_exit(child: &mut Child, contended: &Path) -> bool {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        if contended.try_exists().unwrap() {
            return true;
        }
        if child.try_wait().unwrap().is_some() {
            return false;
        }
        assert!(
            Instant::now() <= deadline,
            "timed out waiting for prune contention or completion"
        );
        std::thread::yield_now();
    }
}

fn wait_for_child(mut child: Child, label: &str) -> Output {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!("{label} worker timed out");
        }
        std::thread::yield_now();
    }
}

fn assert_worker_succeeded(output: Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} worker failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn prune_waits_for_an_in_flight_resolve_registry_commit() {
    if let Ok(role) = std::env::var("DEVKIT_DOCS_TEST_PRUNE_RACE") {
        let cache_root =
            std::path::PathBuf::from(std::env::var_os("DEVKIT_DOCS_TEST_CACHE_ROOT").unwrap());
        let project =
            std::path::PathBuf::from(std::env::var_os("DEVKIT_DOCS_TEST_PROJECT").unwrap());
        if role == "resolve" || role == "seed" {
            let entry = LibEntry {
                name: "up".into(),
                ecosystem: Some(Ecosystem::Git),
                repo: Some(std::env::var("DEVKIT_DOCS_TEST_REPO").unwrap()),
                r#ref: Some("v1.0.0".into()),
                ..Default::default()
            };
            devkit_docs::resolve::resolve(&entry, &project, &cache_root).unwrap();
        } else {
            let store = RefStore::at(&cache_root);
            let snapshot = store.snapshot();
            let manifest_libs = BTreeSet::from(["up".to_string()]);
            let plan = refs::plan_for_cache(
                &cache_root,
                &snapshot,
                &manifest_libs,
                Some(&project.join("missing-global.toml")),
            )
            .unwrap();
            assert_eq!(plan.delete, [("up".to_string(), "v1.0.0".to_string())]);
            for (lib, worktree) in &plan.delete {
                devkit_docs::cache::LibCache::new(&cache_root, lib)
                    .unwrap()
                    .remove_worktree(worktree, &snapshot)
                    .unwrap();
            }
            store
                .commit(|data| {
                    refs::reconcile(data, &snapshot, &plan.keep);
                    Ok(())
                })
                .unwrap();
        }
        return;
    }

    let root = unique_tmp("resolve-prune-race");
    let repo = common::fixture_repo(&root.join("upstream"));
    let cache_root = root.join("cache");
    let project = root.join("project");
    let barrier = root.join("resolve");
    std::fs::create_dir_all(&project).unwrap();
    let exe = std::env::current_exe().unwrap();
    let spawn_worker = |role: &str, pause: bool| {
        let mut command = Command::new(&exe);
        command
            .env("DEVKIT_DOCS_TEST_PRUNE_RACE", role)
            .env("DEVKIT_DOCS_TEST_CACHE_ROOT", &cache_root)
            .env("DEVKIT_DOCS_TEST_PROJECT", &project)
            .env("DEVKIT_DOCS_TEST_REPO", &repo)
            .args([
                "--exact",
                "prune_waits_for_an_in_flight_resolve_registry_commit",
                "--nocapture",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if pause {
            command.env(devkit_docs::barrier::VAR, &barrier);
        } else {
            command.env_remove(devkit_docs::barrier::VAR);
        }
        command.spawn().unwrap()
    };

    assert_worker_succeeded(wait_for_child(spawn_worker("seed", false), "seed"), "seed");
    RefStore::at(&cache_root)
        .commit(|data| {
            data.rows[0].resolved_at = u64::MAX;
            data.rows[0].revision = u64::MAX;
            Ok(())
        })
        .unwrap();
    let stale = RefStore::at(&cache_root).snapshot();
    assert_eq!(stale.rows.len(), 1);
    assert_eq!(stale.rows[0].resolved_at, u64::MAX);

    let resolve = spawn_worker("resolve", true);
    wait_for(&barrier.with_extension("materialized"));
    let checkout = cache_root.join("up/v1.0.0");
    assert!(checkout.is_dir());

    let mut prune = spawn_worker("prune", true);
    let contended = wait_for_contention_or_exit(&mut prune, &barrier.with_extension("contended"));
    let in_flight_checkout_survived = checkout.is_dir();

    std::fs::write(barrier.with_extension("commit"), "").unwrap();
    assert_worker_succeeded(wait_for_child(resolve, "resolve"), "resolve");
    assert_worker_succeeded(wait_for_child(prune, "prune"), "prune");

    assert!(contended, "prune did not block on the per-library lock");
    assert!(
        in_flight_checkout_survived,
        "prune deleted the in-flight checkout"
    );
    assert!(checkout.is_dir(), "prune deleted the committed checkout");
    let data = RefStore::at(&cache_root).snapshot();
    assert_eq!(data.rows.len(), 1);
    assert_eq!(data.rows[0].version, "v1.0.0");
    assert!(data.rows[0].resolved_at < stale.rows[0].resolved_at);
    assert_eq!(data.rows[0].revision, 0);
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
fn whole_library_deletion_rechecks_fresh_references() {
    let root = unique_tmp("whole-lib-race");
    let cache_root = root.join("cache");
    let project = root.join("project");
    std::fs::create_dir_all(cache_root.join("up/repo.git")).unwrap();
    std::fs::create_dir_all(&project).unwrap();

    let snapshot = RefStore::at(&cache_root).snapshot();
    let plan = refs::plan_for_cache(&cache_root, &snapshot, &BTreeSet::new(), None).unwrap();
    assert_eq!(plan.removable_libs, ["up"]);

    RefStore::at(&cache_root)
        .commit(|data| {
            data.record(project.to_str().unwrap(), "up", "v1.0.0");
            Ok(())
        })
        .unwrap();

    let removed = devkit_docs::cache::LibCache::new(&cache_root, "up")
        .unwrap()
        .remove_if_unreferenced(&snapshot)
        .unwrap();

    assert!(!removed);
    assert!(cache_root.join("up").is_dir());
}

#[test]
fn whole_library_deletion_detects_a_same_row_refresh() {
    if std::env::var_os("DEVKIT_DOCS_TEST_WHOLE_LIB_ABA").is_some() {
        let cache_root =
            std::path::PathBuf::from(std::env::var_os("DEVKIT_DOCS_TEST_CACHE_ROOT").unwrap());
        let project = std::env::var("DEVKIT_DOCS_TEST_PROJECT").unwrap();
        RefStore::at(&cache_root)
            .commit(|data| {
                data.record(&project, "up", "v1.0.0");
                Ok(())
            })
            .unwrap();
        return;
    }

    let root = unique_tmp("whole-lib-same-row-race");
    let cache_root = root.join("cache");
    let project = root.join("project");
    std::fs::create_dir_all(cache_root.join("up/repo.git")).unwrap();
    std::fs::create_dir_all(cache_root.join("up/v1.0.0")).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let store = RefStore::at(&cache_root);
    store
        .commit(|data| {
            data.rows.push(refs::RefRow {
                project: project.to_string_lossy().into_owned(),
                lib: "up".into(),
                version: "v1.0.0".into(),
                resolved_at: u64::MAX,
                revision: u64::MAX,
            });
            Ok(())
        })
        .unwrap();
    let snapshot = store.snapshot();
    let plan = refs::plan_for_cache(&cache_root, &snapshot, &BTreeSet::new(), None).unwrap();
    assert_eq!(plan.removable_libs, ["up"]);

    let output = Command::new(std::env::current_exe().unwrap())
        .env("DEVKIT_DOCS_TEST_WHOLE_LIB_ABA", "1")
        .env("DEVKIT_DOCS_TEST_CACHE_ROOT", &cache_root)
        .env("DEVKIT_DOCS_TEST_PROJECT", &project)
        .args([
            "--exact",
            "whole_library_deletion_detects_a_same_row_refresh",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert_worker_succeeded(output, "refresh");

    let removed = devkit_docs::cache::LibCache::new(&cache_root, "up")
        .unwrap()
        .remove_if_unreferenced(&snapshot)
        .unwrap();
    store
        .commit(|data| {
            refs::reconcile(data, &snapshot, &plan.keep);
            Ok(())
        })
        .unwrap();

    assert!(!removed, "whole-library prune missed the refreshed row");
    assert!(cache_root.join("up").is_dir());
    let fresh = store.snapshot();
    assert_eq!(fresh.rows.len(), 1);
    assert!(fresh.rows[0].resolved_at < snapshot.rows[0].resolved_at);
    assert_eq!(fresh.rows[0].revision, 0);
}

#[test]
fn whole_library_deletion_ignores_rows_already_rejected_by_the_plan() {
    let root = unique_tmp("whole-lib-stale-row");
    let cache_root = root.join("cache");
    std::fs::create_dir_all(cache_root.join("up/repo.git")).unwrap();
    let store = RefStore::at(&cache_root);
    store
        .commit(|data| {
            data.record("/missing/project", "up", "v1.0.0");
            Ok(())
        })
        .unwrap();

    let snapshot = store.snapshot();
    let plan = refs::plan_for_cache(&cache_root, &snapshot, &BTreeSet::new(), None).unwrap();
    assert_eq!(plan.removable_libs, ["up"]);

    let removed = devkit_docs::cache::LibCache::new(&cache_root, "up")
        .unwrap()
        .remove_if_unreferenced(&snapshot)
        .unwrap();

    assert!(removed);
    assert!(!cache_root.join("up").exists());
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
        project.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nlocked = \"1\"\n",
    )
    .unwrap();
    std::fs::write(
        project.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"locked\"]\n\n[[package]]\nname = \"locked\"\nversion = \"1.0.0\"\n",
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
            .remove_worktree(worktree, &snapshot)
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
