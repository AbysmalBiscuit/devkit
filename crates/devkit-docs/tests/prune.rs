mod common;

use devkit_docs::manifest::{Ecosystem, LibEntry};
use devkit_docs::refs::{self, RefStore};
use std::collections::{BTreeMap, BTreeSet};
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
                r#ref: Some(std::env::var("DEVKIT_DOCS_TEST_REF").unwrap()),
                ..Default::default()
            };
            devkit_docs::resolve::resolve(
                &entry,
                &project,
                &cache_root,
                &devkit_docs::resolve::Options::default(),
            )
            .unwrap();
        } else {
            let manifest_libs = BTreeSet::from(["up".to_string()]);
            refs::prune_with_lock(
                &cache_root,
                &manifest_libs,
                Some(&project.join("missing-global.toml")),
            )
            .unwrap();
        }
        return;
    }

    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let repo = common::fixture_repo(&root.join("upstream"));
    let cache_root = root.join("cache");
    let project = root.join("project");
    let barrier = root.join("resolve");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("devkit.toml"),
        format!("[[docs.libs]]\nname = 'up'\necosystem = 'git'\nrepo = '{repo}'\n"),
    )
    .unwrap();
    let exe = std::env::current_exe().unwrap();
    let spawn_worker = |role: &str, git_ref: &str, pause: bool| {
        let mut command = Command::new(&exe);
        command
            .env("DEVKIT_DOCS_TEST_PRUNE_RACE", role)
            .env("DEVKIT_DOCS_TEST_CACHE_ROOT", &cache_root)
            .env("DEVKIT_DOCS_TEST_PROJECT", &project)
            .env("DEVKIT_DOCS_TEST_REPO", &repo)
            .env("DEVKIT_DOCS_TEST_REF", git_ref)
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

    assert_worker_succeeded(
        wait_for_child(spawn_worker("seed", "v1.0.0", false), "seed"),
        "seed",
    );
    let seeded = RefStore::at(&cache_root).snapshot();
    assert_eq!(seeded.rows.len(), 1);
    assert_eq!(seeded.rows[0].version, "v1.0.0");

    // The second resolve materializes `main`, a checkout the registry does not
    // reference yet — exactly what a plan taken before the row is committed
    // reads as garbage.
    let resolve = spawn_worker("resolve", "main", true);
    wait_for(&barrier.with_extension("materialized"));
    let checkout = cache_root.join("up/main");
    assert!(checkout.is_dir());

    let mut prune = spawn_worker("prune", "main", true);
    let contended =
        wait_for_contention_or_exit(&mut prune, &barrier.with_extension("contended.up"));
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
    assert!(
        !cache_root.join("up/v1.0.0").exists(),
        "prune kept a checkout the committed row no longer references"
    );
    let data = RefStore::at(&cache_root).snapshot();
    assert_eq!(data.rows.len(), 1);
    assert_eq!(data.rows[0].version, "main");
    assert_eq!(data.rows[0].revision, 1);
    assert!(seeded.rows[0].resolved_at <= data.rows[0].resolved_at);
}

// Regression: a LIVE project whose devkit.toml fails to parse must not have
// its checkouts silently reclaimed — a read/parse error is not "unreferenced".
#[test]
fn prune_keeps_rows_for_a_project_with_an_unreadable_manifest() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let tmp = tmp_dir.path();
    let repo = common::fixture_repo(&tmp.join("upstream"));
    let cache_root = tmp.join("cache");
    let global = tmp.join("docs.toml");
    std::fs::write(&global, "").unwrap();
    let proj_b = tmp.join("B");
    std::fs::create_dir_all(&proj_b).unwrap();
    // The row has to exist before the manifest becomes unreadable, so the
    // checkout is materialized against a devkit.toml that still parses.
    std::fs::write(proj_b.join("devkit.toml"), "[defaults]\n").unwrap();
    let entry = LibEntry {
        name: "libZ".into(),
        repo: Some(repo),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    let resolved = devkit_docs::resolve::resolve(
        &entry,
        &proj_b,
        &cache_root,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();
    std::fs::write(proj_b.join("devkit.toml"), "this = = not valid toml [[[").unwrap();

    let pruned = refs::prune_with_lock(&cache_root, &BTreeSet::new(), Some(&global)).unwrap();

    assert!(
        pruned.removed.is_empty(),
        "a live project's checkout was reclaimed because its manifest failed to parse: {:?}",
        pruned.removed
    );
    assert!(resolved.path.is_dir());
    assert_eq!(RefStore::at(&cache_root).snapshot().rows.len(), 1);
}

#[test]
fn a_scoped_library_is_one_directory_and_prune_leaves_it_alone() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let lib = devkit_docs::cache::LibCache::new(root, "@types/node").unwrap();
    assert!(lib.dir.ends_with("@types~node"));

    std::fs::create_dir_all(root.join("@scope/pkg")).unwrap();
    let stray = devkit_docs::cache::LibCache::new(root, "@scope").unwrap();
    assert!(stray.version_worktrees().is_empty());
}

#[test]
fn whole_library_deletion_rechecks_fresh_references() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let cache_root = root.join("cache");
    let project = root.join("project");
    let global = root.join("docs.toml");
    std::fs::create_dir_all(cache_root.join("up/repo.git")).unwrap();
    std::fs::create_dir_all(&project).unwrap();

    let pruned = refs::prune_with_lock(&cache_root, &BTreeSet::new(), Some(&global)).unwrap();
    assert_eq!(pruned.removable_libs, ["up"]);

    RefStore::at(&cache_root)
        .commit(|data| {
            data.record(project.to_str().unwrap(), "up", "v1.0.0", "v1.0.0", "aaa");
            Ok(())
        })
        .unwrap();

    let removed = devkit_docs::cache::LibCache::new(&cache_root, "up")
        .unwrap()
        .remove_if_unreferenced()
        .unwrap();

    assert!(!removed);
    assert!(cache_root.join("up").is_dir());
}

/// `docm prune` offers an unregistered library for deletion, then reads the
/// registry again after the confirmation before removing it. Another process
/// resolving the library in that window re-creates the row prune dropped, and
/// the deletion has to see it across the process boundary.
#[test]
fn whole_library_deletion_spares_a_library_another_process_re_resolved() {
    if std::env::var_os("DEVKIT_DOCS_TEST_WHOLE_LIB_RERESOLVE").is_some() {
        let cache_root =
            std::path::PathBuf::from(std::env::var_os("DEVKIT_DOCS_TEST_CACHE_ROOT").unwrap());
        let project = std::env::var("DEVKIT_DOCS_TEST_PROJECT").unwrap();
        RefStore::at(&cache_root)
            .commit(|data| {
                data.record(&project, "up", "v1.0.0", "v1.0.0", "aaa");
                Ok(())
            })
            .unwrap();
        return;
    }

    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let cache_root = root.join("cache");
    let project = root.join("project");
    let global = root.join("docs.toml");
    std::fs::create_dir_all(cache_root.join("up/repo.git")).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let store = RefStore::at(&cache_root);
    store
        .commit(|data| {
            data.record(project.to_str().unwrap(), "up", "v1.0.0", "v1.0.0", "aaa");
            Ok(())
        })
        .unwrap();

    let pruned = refs::prune_with_lock(&cache_root, &BTreeSet::new(), Some(&global)).unwrap();
    assert_eq!(pruned.removable_libs, ["up"]);
    // The registry prune leaves behind is what the deletion measures against.
    let snapshot = store.snapshot();
    assert!(snapshot.rows.is_empty(), "prune kept an unreferenced row");

    let output = Command::new(std::env::current_exe().unwrap())
        .env("DEVKIT_DOCS_TEST_WHOLE_LIB_RERESOLVE", "1")
        .env("DEVKIT_DOCS_TEST_CACHE_ROOT", &cache_root)
        .env("DEVKIT_DOCS_TEST_PROJECT", &project)
        .args([
            "--exact",
            "whole_library_deletion_spares_a_library_another_process_re_resolved",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert_worker_succeeded(output, "re-resolve");

    let removed = devkit_docs::cache::LibCache::new(&cache_root, "up")
        .unwrap()
        .remove_if_unreferenced()
        .unwrap();

    assert!(!removed, "whole-library deletion missed the re-created row");
    assert!(cache_root.join("up").is_dir());
    let fresh = store.snapshot();
    assert_eq!(fresh.rows.len(), 1);
    assert_eq!(fresh.rows[0].lib, "up");
}

/// `docm prune` offers a whole library for deletion, then waits for an
/// interactive confirmation before acting. Another process resolving that
/// library while the human decides commits its row inside that interval, so
/// the recheck guarding the deletion has to refuse on the row's mere presence.
/// A recheck that instead asked whether the row was new relative to some
/// earlier registry read would find it already present in both and delete a
/// live checkout.
#[test]
fn whole_library_deletion_spares_a_library_resolved_before_the_snapshot() {
    if std::env::var_os("DEVKIT_DOCS_TEST_WHOLE_LIB_RESOLVED_BEFORE_SNAPSHOT").is_some() {
        let cache_root =
            std::path::PathBuf::from(std::env::var_os("DEVKIT_DOCS_TEST_CACHE_ROOT").unwrap());
        let project = std::env::var("DEVKIT_DOCS_TEST_PROJECT").unwrap();
        RefStore::at(&cache_root)
            .commit(|data| {
                data.record(&project, "up", "v1.0.0", "v1.0.0", "aaa");
                Ok(())
            })
            .unwrap();
        return;
    }

    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let cache_root = root.join("cache");
    let project = root.join("project");
    let global = root.join("docs.toml");
    std::fs::create_dir_all(cache_root.join("up/repo.git")).unwrap();
    std::fs::create_dir_all(&project).unwrap();

    let pruned = refs::prune_with_lock(&cache_root, &BTreeSet::new(), Some(&global)).unwrap();
    assert_eq!(pruned.removable_libs, ["up"]);

    let output = Command::new(std::env::current_exe().unwrap())
        .env("DEVKIT_DOCS_TEST_WHOLE_LIB_RESOLVED_BEFORE_SNAPSHOT", "1")
        .env("DEVKIT_DOCS_TEST_CACHE_ROOT", &cache_root)
        .env("DEVKIT_DOCS_TEST_PROJECT", &project)
        .args([
            "--exact",
            "whole_library_deletion_spares_a_library_resolved_before_the_snapshot",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert_worker_succeeded(output, "resolve-before-snapshot");

    // Establishes that the child's row is committed and visible before the
    // deletion runs, so a refusal below is the recheck and not a lost write.
    let snapshot = RefStore::at(&cache_root).snapshot();
    assert_eq!(snapshot.rows.len(), 1);

    let removed = devkit_docs::cache::LibCache::new(&cache_root, "up")
        .unwrap()
        .remove_if_unreferenced()
        .unwrap();

    assert!(
        !removed,
        "whole-library deletion missed a row committed before the snapshot"
    );
    assert!(cache_root.join("up").is_dir());
}

#[test]
fn whole_library_deletion_ignores_rows_already_rejected_by_the_plan() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let cache_root = root.join("cache");
    let global = root.join("docs.toml");
    std::fs::create_dir_all(cache_root.join("up/repo.git")).unwrap();
    let store = RefStore::at(&cache_root);
    store
        .commit(|data| {
            data.record("/missing/project", "up", "v1.0.0", "v1.0.0", "aaa");
            Ok(())
        })
        .unwrap();

    let pruned = refs::prune_with_lock(&cache_root, &BTreeSet::new(), Some(&global)).unwrap();
    assert_eq!(pruned.removable_libs, ["up"]);

    let removed = devkit_docs::cache::LibCache::new(&cache_root, "up")
        .unwrap()
        .remove_if_unreferenced()
        .unwrap();

    assert!(removed);
    assert!(!cache_root.join("up").exists());
}

// Regression: whole-library deletion must fail closed on an unreadable
// registry, not read it as having no rows and delete the library directory.
// A directory in registry.json's place is the CI-portable way to make it
// unreadable (see `prune_aborts_when_the_registry_is_unreadable` for why
// `chmod` is not portable here).
#[test]
fn whole_library_deletion_aborts_when_the_registry_is_unreadable() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let cache_root = root.join("cache");
    std::fs::create_dir_all(cache_root.join("up/repo.git")).unwrap();
    std::fs::create_dir_all(cache_root.join("registry.json")).unwrap();

    let result = devkit_docs::cache::LibCache::new(&cache_root, "up")
        .unwrap()
        .remove_if_unreferenced();

    assert!(
        result.is_err(),
        "whole-library deletion must fail closed on an unreadable registry \
         instead of treating it as having no rows"
    );
    assert!(
        cache_root.join("up").is_dir(),
        "whole-library deletion removed the library despite the unreadable registry"
    );
    assert!(
        cache_root.join("registry.json").is_dir(),
        "whole-library deletion mutated the unreadable registry path"
    );
}

#[test]
fn prune_preserves_every_ref_named_checkout_recorded_by_resolve() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let tmp = tmp_dir.path();
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
        "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"locked\"]\n\n[[package]]\nname = \"locked\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
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
        .map(|entry| {
            devkit_docs::resolve::resolve(
                entry,
                &project,
                &cache_root,
                &devkit_docs::resolve::Options {
                    allow_default_branch: true,
                },
            )
            .unwrap()
        })
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
    let global = tmp.join("docs.toml");
    std::fs::write(&global, "").unwrap();
    let pruned = refs::prune_with_lock(&cache_root, &manifest_libs, Some(&global)).unwrap();

    for resolution in &resolved {
        assert!(
            resolution.path.is_dir(),
            "prune removed the live {} checkout {} via {:?}",
            resolution.name,
            resolution.worktree,
            pruned.removed
        );
    }
    assert!(
        pruned.removed.is_empty(),
        "prune removed live ref-named checkouts: {:?}",
        pruned.removed
    );
}

#[test]
fn two_workspaces_sharing_a_lockfile_keep_separate_rows() {
    let mut d = refs::Data::default();
    d.record("/repo/apps/api", "h3", "v1.15.11", "v1.15.11", "aaa");
    d.record("/repo/apps/web", "h3", "v2.0.1", "v2.0.1", "bbb");
    assert_eq!(
        d.rows.len(),
        2,
        "a workspace key must not overwrite its sibling"
    );
}

#[test]
fn a_legacy_row_protects_default_until_a_real_materialization_retires_it() {
    let mut d = refs::Data::default();
    d.record_legacy("/repo", "h3", "default");
    assert_eq!(refs::row_dirname(&d.rows[0]), "default");

    d.record("/repo/apps/api", "h3", "v1.15.11", "v1.15.11", "aaa");
    d.retire_legacy("/repo/apps/api", "h3");
    assert_eq!(d.rows.len(), 1);
    assert_eq!(d.rows[0].version, "v1.15.11");
}

#[test]
fn resolving_the_workspace_that_owns_a_legacy_row_upserts_it() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let tmp = tmp_dir.path();
    let repo = common::fixture_repo(&tmp.join("upstream"));
    let cache_root = tmp.join("cache");
    let project = tmp.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("devkit.toml"), "[defaults]\n").unwrap();
    RefStore::at(&cache_root)
        .commit(|data| {
            data.record_legacy(project.to_str().unwrap(), "up", "default");
            Ok(())
        })
        .unwrap();

    let entry = LibEntry {
        name: "up".into(),
        ecosystem: Some(Ecosystem::Git),
        repo: Some(repo),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    devkit_docs::resolve::resolve(
        &entry,
        &project,
        &cache_root,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();

    let rows = RefStore::at(&cache_root).snapshot().rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].version, "v1.0.0");
    assert!(!rows[0].commit.is_empty());
    assert_eq!(
        rows[0].revision, 1,
        "the legacy row was retired and re-inserted rather than upserted; a \
         fresh row restarts the revision at 0, and that is the value reconcile \
         compares, so a prune holding the pre-resolve snapshot would drop this row"
    );
}

#[test]
fn a_legacy_row_keeps_its_checkout_until_a_workspace_row_retires_it() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let lockfile_dir = root.join("repo");
    let member = lockfile_dir.join("apps/api");
    std::fs::create_dir_all(&member).unwrap();
    let worktrees = BTreeMap::from([(
        "h3".to_string(),
        vec!["default".to_string(), "v1.15.11".to_string()],
    )]);
    let libs = BTreeSet::from(["h3".to_string()]);

    let mut data = refs::Data::default();
    data.record_legacy(lockfile_dir.to_str().unwrap(), "h3", "default");
    let before = refs::plan(&data, &worktrees, &libs, |_, _| Some("default".to_string()));
    assert!(
        !before
            .delete
            .contains(&("h3".to_string(), "default".to_string())),
        "a legacy row must keep protecting the checkout it references: {:?}",
        before.delete
    );

    data.record(
        member.to_str().unwrap(),
        "h3",
        "v1.15.11",
        "v1.15.11",
        "aaa",
    );
    data.retire_legacy(member.to_str().unwrap(), "h3");

    let after = refs::plan(&data, &worktrees, &libs, |_, _| {
        Some("v1.15.11".to_string())
    });
    assert_eq!(
        after.delete,
        [("h3".to_string(), "default".to_string())],
        "a retired legacy row must stop protecting its checkout"
    );
}

#[test]
fn prune_never_enumerates_a_control_entry_as_a_library() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::create_dir_all(root.join("registry.locks")).unwrap();
    std::fs::write(root.join("registry.json"), "{}").unwrap();

    let pruned = refs::prune_with_lock(root, &BTreeSet::new(), None).unwrap();

    assert!(
        pruned.removed.is_empty() && pruned.removable_libs.is_empty(),
        "the registry's own files are not unreferenced libraries: {pruned:?}"
    );
    assert!(
        pruned.skipped.is_empty(),
        "the registry's own files were reported as malformed libraries: {:?}",
        pruned.skipped
    );
    assert!(
        !root.join("registry.locks/registry.locks.lock").exists(),
        "prune locked a control entry as if it were a library"
    );
}

// Bug #1 regression on the production path: pruning from project A must not
// reclaim project B's overlay-only lib, whose checkouts A's manifest never
// mentions.
#[test]
fn prune_with_lock_preserves_another_projects_overlay_lib() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let tmp = tmp_dir.path();
    let repo = common::fixture_repo(&tmp.join("upstream"));
    let cache_root = tmp.join("cache");
    let global = tmp.join("docs.toml");
    let proj_a = tmp.join("A");
    let proj_b = tmp.join("B");
    for project in [&proj_a, &proj_b] {
        std::fs::create_dir_all(project).unwrap();
        std::fs::write(project.join("devkit.toml"), "[defaults]\n").unwrap();
    }

    let lib_x = LibEntry {
        name: "libX".into(),
        repo: Some(repo.clone()),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    let lib_y = LibEntry {
        name: "libY".into(),
        repo: Some(repo),
        r#ref: Some("v1.1.0".into()),
        ..Default::default()
    };
    devkit_docs::manifest::upsert_global(&global, &lib_x, &cache_root).unwrap();
    devkit_docs::manifest::upsert_project(&proj_b.join("devkit.toml"), &lib_y, &cache_root)
        .unwrap();

    let x = devkit_docs::resolve::resolve(
        &lib_x,
        &proj_a,
        &cache_root,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();
    let y = devkit_docs::resolve::resolve(
        &lib_y,
        &proj_b,
        &cache_root,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();

    // Pruning "as if from A": A's manifest sees only libX.
    let a_libs = BTreeSet::from(["libX".to_string()]);
    let pruned = refs::prune_with_lock(&cache_root, &a_libs, Some(&global)).unwrap();

    assert!(
        pruned.removed.is_empty(),
        "prune from A reclaimed a checkout it does not own: {:?}",
        pruned.removed
    );
    assert!(
        pruned.removable_libs.is_empty(),
        "prune from A offered another project's lib for deletion: {:?}",
        pruned.removable_libs
    );
    assert!(x.path.is_dir() && y.path.is_dir());
    assert_eq!(RefStore::at(&cache_root).snapshot().rows.len(), 2);
}

#[test]
fn prune_reports_a_cache_entry_that_is_not_a_library() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::create_dir_all(root.join("@scope/pkg")).unwrap();

    let pruned = refs::prune_with_lock(root, &BTreeSet::new(), None).unwrap();

    assert_eq!(
        pruned.skipped.iter().map(|s| &s.entry).collect::<Vec<_>>(),
        ["@scope"],
        "a directory without repo.git must be reported, not silently ignored"
    );
    assert_eq!(
        pruned.skipped[0].reason,
        "no repo.git, so it is not a library"
    );
    assert!(pruned.removed.is_empty() && pruned.removable_libs.is_empty());
    assert!(root.join("@scope/pkg").is_dir());
}

#[test]
fn prune_reports_an_unmanageable_directory_and_keeps_going() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let cache_root = root.join("cache");
    let project = root.join("project");
    let global = root.join("docs.toml");
    std::fs::create_dir_all(cache_root.join("manifest/repo.git")).unwrap();
    std::fs::create_dir_all(cache_root.join("up/repo.git")).unwrap();
    std::fs::create_dir_all(&project).unwrap();

    let pruned = refs::prune_with_lock(&cache_root, &BTreeSet::new(), Some(&global)).unwrap();

    assert!(
        pruned.removable_libs.contains(&"up".to_string()),
        "the unmanageable directory stopped the normal library from being processed: {:?}",
        pruned.removable_libs
    );
    let manifest_entry = pruned
        .skipped
        .iter()
        .find(|s| s.entry == "manifest")
        .unwrap_or_else(|| panic!("manifest not reported as skipped: {:?}", pruned.skipped));
    assert!(
        manifest_entry.reason.contains("reserved"),
        "reason does not mention the name is reserved: {}",
        manifest_entry.reason
    );
    let expected_path = cache_root.join("manifest");
    assert!(
        manifest_entry
            .reason
            .contains(&expected_path.display().to_string()),
        "reason does not mention the absolute path {}: {}",
        expected_path.display(),
        manifest_entry.reason
    );
}

/// `names::representable` allows a directory name up to 255 bytes, but
/// `locks::lock_path_for_dir` appends `.lock` and then re-applies the same
/// 255-byte ceiling, so a name from 251 to 255 bytes passes name validation
/// yet still cannot be locked. `scan_cache` has to reject on the full
/// `with_lib_dir` precondition (`locks::lock_path`), not just `names::lib_dir`,
/// or this directory reaches `scan.libs` and aborts the whole prune the same
/// way an outright-invalid name does.
#[test]
fn prune_reports_a_directory_too_long_for_its_lock_file_and_keeps_going() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let cache_root = root.join("cache");
    let project = root.join("project");
    let global = root.join("docs.toml");
    let too_long = "a".repeat(255);
    std::fs::create_dir_all(cache_root.join(&too_long).join("repo.git")).unwrap();
    std::fs::create_dir_all(cache_root.join("up/repo.git")).unwrap();
    std::fs::create_dir_all(&project).unwrap();

    let pruned = refs::prune_with_lock(&cache_root, &BTreeSet::new(), Some(&global)).unwrap();

    assert!(
        pruned.removable_libs.contains(&"up".to_string()),
        "the too-long directory stopped the normal library from being processed: {:?}",
        pruned.removable_libs
    );
    let skipped_entry = pruned
        .skipped
        .iter()
        .find(|s| s.entry == too_long)
        .unwrap_or_else(|| {
            panic!(
                "too-long name not reported as skipped: {:?}",
                pruned.skipped
            )
        });
    let expected_path = cache_root.join(&too_long);
    assert!(
        skipped_entry
            .reason
            .contains(&expected_path.display().to_string()),
        "reason does not mention the absolute path {}: {}",
        expected_path.display(),
        skipped_entry.reason
    );
}

#[test]
fn prune_drops_a_row_whose_project_directory_is_gone() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    RefStore::at(root)
        .commit(|data| {
            data.record("/gone/nowhere", "libX", "v1.0.0", "v1.0.0", "aaa");
            Ok(())
        })
        .unwrap();

    refs::prune_with_lock(root, &BTreeSet::new(), None).unwrap();

    assert!(
        RefStore::at(root).snapshot().rows.is_empty(),
        "a row whose holder directory is gone outlived prune, and no cache \
         directory exists to reach it through"
    );
}

// Regression: an unreadable registry must never read as an empty one. A
// directory in registry.json's place is the portable way to make it
// unreadable across all three CI platforms — `chmod 000` is a no-op when the
// suite runs as root (common in CI containers), and Windows does not honor
// POSIX permission bits at all. `fs::read_to_string` on a directory fails
// with an I/O error on every platform, so this exercises the same failure
// mode a permission-denied file or a mid-migration mount would.
#[test]
fn prune_aborts_when_the_registry_is_unreadable() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let tmp = tmp_dir.path();
    let repo = common::fixture_repo(&tmp.join("upstream"));
    let cache_root = tmp.join("cache");
    let project = tmp.join("project");
    let manifest_path = project.join("devkit.toml");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(&manifest_path, "[defaults]\n").unwrap();

    let entry = LibEntry {
        name: "up".into(),
        repo: Some(repo),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    // Without this, the project's manifest never lists "up", `live_reference`
    // finds no entry regardless of the registry's readability, and prune
    // deletes the checkout either way — proving nothing about the registry.
    devkit_docs::manifest::upsert_project(&manifest_path, &entry, &cache_root).unwrap();

    let resolved = devkit_docs::resolve::resolve(
        &entry,
        &project,
        &cache_root,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();
    assert!(
        resolved.path.is_dir(),
        "fixture must materialize a checkout"
    );
    assert!(
        cache_root.join("up/repo.git").is_dir(),
        "fixture must produce a real library (repo.git) or prune never reaches the registry read"
    );

    let manifest_libs = BTreeSet::from(["up".to_string()]);

    // Control: with the registry still readable, the manifest entry above
    // genuinely keeps the checkout. This proves the deletion this test
    // guards against comes from registry unreadability specifically, not
    // from a fixture that was never live in the first place.
    let control = refs::prune_with_lock(&cache_root, &manifest_libs, None).unwrap();
    assert!(
        control.removed.is_empty(),
        "fixture is not live: a readable registry should already keep the checkout: {:?}",
        control.removed
    );
    assert!(resolved.path.is_dir());

    let registry_path = cache_root.join("registry.json");
    assert!(
        registry_path.is_file(),
        "fixture must have a real registry file to shadow with a directory"
    );
    std::fs::remove_file(&registry_path).unwrap();
    std::fs::create_dir_all(&registry_path).unwrap();

    let result = refs::prune_with_lock(&cache_root, &manifest_libs, None);

    assert!(
        result.is_err(),
        "prune must fail closed on an unreadable registry instead of treating it as empty"
    );
    assert!(
        resolved.path.is_dir(),
        "prune deleted a live checkout while the registry was unreadable"
    );
    assert!(
        registry_path.is_dir(),
        "prune mutated the unreadable registry path"
    );
}

// Regression: an unparsable lockfile must not read as "package no longer
// referenced". A live project whose Cargo.lock fails to parse must keep its
// checkout, the same protection `live_reference` already gives an unreadable
// devkit.toml.
#[test]
fn prune_keeps_checkout_when_a_lockfile_fails_to_parse() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let tmp = tmp_dir.path();
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
        "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"locked\"]\n\n[[package]]\nname = \"locked\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
    )
    .unwrap();

    let entry = LibEntry {
        name: "locked".into(),
        ecosystem: Some(Ecosystem::Rust),
        repo: Some(repo),
        ..Default::default()
    };
    devkit_docs::manifest::upsert_project(&manifest_path, &entry, &cache_root).unwrap();

    let resolved = devkit_docs::resolve::resolve(
        &entry,
        &project,
        &cache_root,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();
    assert!(
        resolved.path.is_dir(),
        "fixture must materialize a checkout"
    );
    assert!(
        cache_root.join("locked/repo.git").is_dir(),
        "fixture must produce a real library (repo.git) or prune never reaches the lockfile read"
    );

    let global = tmp.join("docs.toml");
    std::fs::write(&global, "").unwrap();
    let manifest_libs = BTreeSet::from(["locked".to_string()]);

    // Control: with the lockfile still valid, prune must actually reach and
    // exercise `current_version`'s lockfile branch — otherwise the negative
    // assertion below would pass vacuously.
    let control = refs::prune_with_lock(&cache_root, &manifest_libs, Some(&global)).unwrap();
    assert!(
        control.removed.is_empty(),
        "fixture is not live: a valid lockfile should already keep the checkout: {:?}",
        control.removed
    );
    assert!(resolved.path.is_dir());

    std::fs::write(project.join("Cargo.lock"), "this = = not valid toml [[[").unwrap();

    let pruned = refs::prune_with_lock(&cache_root, &manifest_libs, Some(&global)).unwrap();

    assert!(
        pruned.removed.is_empty(),
        "a live project's checkout was reclaimed because its lockfile failed to parse: {:?}",
        pruned.removed
    );
    assert!(resolved.path.is_dir());
}

// A lockfile that still parses fine, and genuinely no longer lists the
// package, is real evidence the project stopped referencing it — that
// checkout must still be reclaimed.
#[test]
fn prune_reclaims_a_checkout_once_the_valid_lockfile_drops_the_package() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let tmp = tmp_dir.path();
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
        "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"locked\"]\n\n[[package]]\nname = \"locked\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
    )
    .unwrap();

    let entry = LibEntry {
        name: "locked".into(),
        ecosystem: Some(Ecosystem::Rust),
        repo: Some(repo),
        ..Default::default()
    };
    devkit_docs::manifest::upsert_project(&manifest_path, &entry, &cache_root).unwrap();

    let resolved = devkit_docs::resolve::resolve(
        &entry,
        &project,
        &cache_root,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();
    assert!(
        resolved.path.is_dir(),
        "fixture must materialize a checkout"
    );

    let global = tmp.join("docs.toml");
    std::fs::write(&global, "").unwrap();
    let manifest_libs = BTreeSet::from(["locked".to_string()]);

    let control = refs::prune_with_lock(&cache_root, &manifest_libs, Some(&global)).unwrap();
    assert!(
        control.removed.is_empty(),
        "fixture is not live: a valid lockfile listing the package should keep the checkout: {:?}",
        control.removed
    );

    // Still valid TOML, but "locked" is no longer a `[[package]]` entry.
    std::fs::write(
        project.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let pruned = refs::prune_with_lock(&cache_root, &manifest_libs, Some(&global)).unwrap();

    assert_eq!(
        pruned.removed,
        vec!["locked/v1.0.0".to_string()],
        "a package genuinely dropped from a valid lockfile must still be reclaimed"
    );
    assert!(
        !resolved.path.is_dir(),
        "the checkout should have been reclaimed once the valid lockfile stopped listing it"
    );
}
