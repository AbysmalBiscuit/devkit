mod common;

use common::{fixture_repo, unique_tmp};
use devkit_docs::manifest::{Ecosystem, LibEntry};
use devkit_docs::refs::RefStore;
use devkit_docs::resolve::{Options, resolve};

#[test]
fn lockfile_version_resolves_to_tag_worktree_and_records_ref() {
    let tmp = unique_tmp("resolve");
    let repo = fixture_repo(&tmp.join("upstream"));
    let cache_root = tmp.join("cache");
    let project = tmp.join("proj");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nmylib = \"1\"\n",
    )
    .unwrap();
    std::fs::write(
        project.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"mylib\"]\n\n[[package]]\nname = \"mylib\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();

    let entry = LibEntry {
        name: "mylib".into(),
        ecosystem: Some(Ecosystem::Rust),
        repo: Some(repo),
        ..Default::default()
    };
    let r = resolve(&entry, &project, &cache_root, &Options::default()).unwrap();
    assert_eq!(r.worktree, "v1.0.0");
    assert_eq!(r.version, "1.0.0");
    // Tag content, not tip: v1.0.0 has "// v1".
    assert_eq!(
        std::fs::read_to_string(r.path.join("src/lib.rs")).unwrap(),
        "// v1"
    );
    assert_eq!(r.layout.docs_dir.as_deref(), Some("docs"));
    assert_eq!(r.warnings.len(), 1);
    assert!(r.warnings[0].contains("Cargo.lock"), "{:?}", r.warnings);

    let data = RefStore::at(&cache_root).snapshot();
    assert_eq!(data.rows.len(), 1);
    assert_eq!(data.rows[0].project, project.to_string_lossy());
    assert_eq!(data.rows[0].version, "v1.0.0");

    // Cached tag pattern short-circuits the next probe.
    let meta = devkit_docs::cache::read_meta(&cache_root.join("mylib"));
    assert_eq!(meta.tag_pattern, Some(devkit_docs::tags::TagPattern::V));
}

#[test]
fn ref_pin_wins_and_no_lockfile_falls_back_to_default_branch_with_warning() {
    let tmp = unique_tmp("resolve-pin");
    let repo = fixture_repo(&tmp.join("upstream"));
    let cache_root = tmp.join("cache");
    let project = tmp.join("proj");
    std::fs::create_dir_all(&project).unwrap();

    let pinned = LibEntry {
        name: "mylib".into(),
        repo: Some(repo.clone()),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    let r = resolve(&pinned, &project, &cache_root, &Options::default()).unwrap();
    assert_eq!(r.worktree, "v1.0.0");
    assert_eq!(r.version, "v1.0.0");
    assert_eq!(
        std::fs::read_to_string(r.path.join("src/lib.rs")).unwrap(),
        "// v1"
    );

    let cache2 = tmp.join("cache2");
    let unpinned = LibEntry {
        name: "mylib".into(),
        ecosystem: Some(Ecosystem::Rust),
        repo: Some(repo),
        ..Default::default()
    };
    let r2 = resolve(
        &unpinned,
        &project,
        &cache2,
        &Options {
            allow_default_branch: true,
        },
    )
    .unwrap();
    assert_eq!(r2.worktree, "main");
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
    let r = resolve(&entry, &tmp, &cache_root, &Options::default()).unwrap();
    assert_eq!(r.layout.docs_dir.as_deref(), Some("docs/special")); // override wins
    let meta = devkit_docs::cache::read_meta(&cache_root.join("mylib"));
    // meta stores the DETECTED layout (docs), not the override.
    assert_eq!(meta.layouts["v1.0.0"].docs_dir.as_deref(), Some("docs"));
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
    let r = resolve(
        &entry,
        &project,
        &cache_root,
        &Options {
            allow_default_branch: true,
        },
    )
    .unwrap();
    assert_eq!(r.worktree, "main");
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
fn lockfile_resolved_from_subdir_records_selected_workspace() {
    let tmp = unique_tmp("resolve-subdir");
    let repo = fixture_repo(&tmp.join("upstream"));
    let cache_root = tmp.join("cache");
    let project = tmp.join("proj");
    let member = project.join("crates/app");
    let deep = member.join("src");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/app\"]\n",
    )
    .unwrap();
    std::fs::write(
        member.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nmylib = \"1\"\n",
    )
    .unwrap();
    std::fs::write(
        project.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"mylib\"]\n\n[[package]]\nname = \"mylib\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();

    let entry = LibEntry {
        name: "mylib".into(),
        ecosystem: Some(Ecosystem::Rust),
        repo: Some(repo),
        ..Default::default()
    };
    let r = resolve(&entry, &deep, &cache_root, &Options::default()).unwrap();
    assert_eq!(r.worktree, "v1.0.0");

    let data = RefStore::at(&cache_root).snapshot();
    assert_eq!(data.rows.len(), 1);
    assert_eq!(
        data.rows[0].project,
        member.to_string_lossy(),
        "reference must be attributed to the selected member workspace"
    );
    assert_eq!(
        devkit_docs::refs::current_version(&entry, &member),
        Some("1.0.0".to_string())
    );
}

#[test]
fn a_changed_pin_gets_its_own_directory_and_never_returns_the_old_checkout() {
    let base = common::unique_tmp("repin");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");

    let mut entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(repo.clone()),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    let first = devkit_docs::resolve::resolve(
        &entry,
        &base,
        &cache,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();
    assert!(first.path.ends_with("v1.0.0"));
    assert_eq!(
        std::fs::read_to_string(first.path.join("src/lib.rs")).unwrap(),
        "// v1"
    );

    entry.r#ref = Some("v1.1.0".into());
    let second = devkit_docs::resolve::resolve(
        &entry,
        &base,
        &cache,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();
    assert!(
        second.path.ends_with("v1.1.0"),
        "a re-pin must not reuse the old directory"
    );
    assert_eq!(
        std::fs::read_to_string(second.path.join("src/lib.rs")).unwrap(),
        "// v2"
    );
    assert_eq!(
        std::fs::read_to_string(first.path.join("src/lib.rs")).unwrap(),
        "// v1"
    );
    assert_ne!(first.commit, second.commit);
}

#[test]
fn a_corrupted_head_is_repaired_and_reported() {
    let base = common::unique_tmp("repair");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(repo),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    let r = devkit_docs::resolve::resolve(
        &entry,
        &base,
        &cache,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();
    devkit_common::cmd::capture(
        "git",
        &["checkout", "--detach", "v1.1.0"],
        Some(r.path.to_str().unwrap()),
    )
    .unwrap();

    let again = devkit_docs::resolve::resolve(
        &entry,
        &base,
        &cache,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();
    assert_eq!(again.status, devkit_docs::resolve::Status::Repaired);
    assert_eq!(again.commit, r.commit);
}

#[test]
fn a_tracked_dirty_checkout_is_a_hard_error() {
    let base = common::unique_tmp("dirty");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(repo),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    let r = devkit_docs::resolve::resolve(
        &entry,
        &base,
        &cache,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();

    std::fs::write(r.path.join("src/lib.rs"), "// tampered").unwrap();
    assert!(
        devkit_docs::resolve::resolve(
            &entry,
            &base,
            &cache,
            &devkit_docs::resolve::Options::default()
        )
        .is_err()
    );
}

#[test]
fn an_untracked_dirty_checkout_is_a_hard_error() {
    let base = common::unique_tmp("dirty-untracked");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(repo),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    let r = devkit_docs::resolve::resolve(
        &entry,
        &base,
        &cache,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();

    std::fs::write(r.path.join("src/planted.rs"), "// planted").unwrap();
    assert!(
        devkit_docs::resolve::resolve(
            &entry,
            &base,
            &cache,
            &devkit_docs::resolve::Options::default()
        )
        .is_err()
    );
}

#[test]
fn a_repo_url_change_is_refused_rather_than_reusing_the_clone() {
    let base = common::unique_tmp("origin");
    let a = common::fixture_repo(&base.join("a"));
    let b = common::fixture_repo(&base.join("b"));
    let cache = base.join("cache");
    let mut entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(a),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    devkit_docs::resolve::resolve(
        &entry,
        &base,
        &cache,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();
    entry.repo = Some(b);
    let err = devkit_docs::resolve::resolve(
        &entry,
        &base,
        &cache,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("origin"),
        "error must name the mismatch: {err}"
    );
}

#[test]
fn a_tag_moved_upstream_is_seen_after_fetch_not_on_a_plain_resolve() {
    let base = common::unique_tmp("movedtag");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(repo.clone()),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    let first = devkit_docs::resolve::resolve(
        &entry,
        &base,
        &cache,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();

    devkit_common::cmd::capture("git", &["tag", "-f", "v1.0.0", "v1.1.0"], Some(&repo)).unwrap();

    let second = devkit_docs::resolve::resolve(
        &entry,
        &base,
        &cache,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();
    assert_eq!(
        second.commit, first.commit,
        "resolve must not fetch for a ref it already has"
    );

    let lib = devkit_docs::cache::LibCache::new(&cache, "up").unwrap();
    lib.fetch().unwrap();
    let third = devkit_docs::resolve::resolve(
        &entry,
        &base,
        &cache,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();
    assert_ne!(
        third.commit, first.commit,
        "after a fetch the checkout follows the moved tag"
    );
    assert_eq!(
        third.path, first.path,
        "the ref names the directory, so it is reused"
    );
    assert_eq!(third.status, devkit_docs::resolve::Status::Repaired);
    assert!(
        third
            .warnings
            .iter()
            .any(|warning| warning.contains("re-pointed")),
        "moved-tag repair was not reported: {:?}",
        third.warnings
    );
    let head = devkit_common::cmd::capture(
        "git",
        &["rev-parse", "HEAD"],
        Some(third.path.to_str().unwrap()),
    )
    .unwrap();
    assert_eq!(head.trim(), third.commit);
    assert_eq!(
        std::fs::read_to_string(third.path.join("src/lib.rs")).unwrap(),
        "// v2"
    );
}

#[test]
fn a_failed_moved_tag_repair_does_not_report_success() {
    let base = common::unique_tmp("movedtag-failed-repair");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(repo.clone()),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    let first = devkit_docs::resolve::resolve(
        &entry,
        &base,
        &cache,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();
    devkit_common::cmd::capture("git", &["tag", "-f", "v1.0.0", "v1.1.0"], Some(&repo)).unwrap();
    let lib = devkit_docs::cache::LibCache::new(&cache, "up").unwrap();
    lib.fetch().unwrap();
    std::fs::write(first.path.join("src/lib.rs"), "// local change").unwrap();

    let error = devkit_docs::resolve::resolve(
        &entry,
        &base,
        &cache,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap_err()
    .to_string();

    assert!(
        !error.contains("re-pointed"),
        "failed repair reported success: {error}"
    );
}

#[test]
fn a_tag_deleted_upstream_is_a_hard_error_after_prune_tags() {
    let base = common::unique_tmp("deltag");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    devkit_common::cmd::capture("git", &["tag", "v2.0.0", "v1.1.0"], Some(&repo)).unwrap();
    let entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(repo.clone()),
        r#ref: Some("v2.0.0".into()),
        ..Default::default()
    };
    devkit_docs::resolve::resolve(
        &entry,
        &base,
        &cache,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();

    devkit_common::cmd::capture("git", &["tag", "-d", "v2.0.0"], Some(&repo)).unwrap();
    let lib = devkit_docs::cache::LibCache::new(&cache, "up").unwrap();
    lib.fetch().unwrap();
    assert!(
        devkit_common::cmd::capture(
            "git",
            &["rev-parse", "--verify", "refs/tags/v2.0.0"],
            Some(lib.bare().to_str().unwrap()),
        )
        .is_err(),
        "--prune-tags must delete the local tag"
    );

    let err = devkit_docs::resolve::resolve(
        &entry,
        &base,
        &cache,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("v2.0.0"),
        "the error must name the withdrawn ref: {err}"
    );
}

#[test]
fn a_pin_that_changes_from_a_tag_to_a_branch_is_refused() {
    let base = common::unique_tmp("kindchange");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    devkit_common::cmd::capture("git", &["tag", "release", "v1.0.0"], Some(&repo)).unwrap();
    let entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(repo.clone()),
        r#ref: Some("release".into()),
        ..Default::default()
    };
    devkit_docs::resolve::resolve(
        &entry,
        &base,
        &cache,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();

    devkit_common::cmd::capture("git", &["tag", "-d", "release"], Some(&repo)).unwrap();
    devkit_common::cmd::capture("git", &["branch", "release", "v1.1.0"], Some(&repo)).unwrap();
    let lib = devkit_docs::cache::LibCache::new(&cache, "up").unwrap();
    lib.fetch().unwrap();

    let err = devkit_docs::resolve::resolve(
        &entry,
        &base,
        &cache,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("refs/tags/release"),
        "the error must name the previous kind: {err}"
    );
}

#[test]
fn a_ref_published_after_the_last_fetch_resolves_without_a_manual_sync() {
    let base = common::unique_tmp("newtag");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let mut entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(repo.clone()),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    devkit_docs::resolve::resolve(
        &entry,
        &base,
        &cache,
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap();

    devkit_common::cmd::capture("git", &["tag", "v3.0.0", "v1.1.0"], Some(&repo)).unwrap();

    entry.r#ref = Some("v3.0.0".into());
    let r = devkit_docs::resolve::resolve(
        &entry,
        &base,
        &cache,
        &devkit_docs::resolve::Options::default(),
    )
    .expect("a miss must fetch once and retry before failing");
    assert!(r.path.ends_with("v3.0.0"));
}

#[test]
fn a_version_with_no_tag_is_a_hard_error_listing_what_was_tried() {
    let base = common::unique_tmp("notag");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    // The lockfile needs `app`'s own entry with its edge to `up`, or importer
    // selection fails first and this test goes RED for the wrong reason —
    // proving nothing about the missing tag it exists to check.
    std::fs::write(
        base.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \"up\",\n]\n\n[[package]]\nname = \"up\"\nversion = \"9.9.9\"\n",
    )
    .unwrap();
    std::fs::write(
        base.join("Cargo.toml"),
        "[package]\nname = \"app\"\n\n[dependencies]\nup = \"9.9.9\"\n",
    )
    .unwrap();

    let entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Rust),
        repo: Some(repo),
        ..Default::default()
    };
    let opts = devkit_docs::resolve::Options::default();
    let err = devkit_docs::resolve::resolve(&entry, &base, &cache, &opts)
        .unwrap_err()
        .to_string();
    assert!(err.contains("9.9.9"), "{err}");
    assert!(
        err.contains("v9.9.9"),
        "the error must list the patterns tried: {err}"
    );
    assert!(err.contains("--allow-default-branch"), "{err}");

    let opts = devkit_docs::resolve::Options {
        allow_default_branch: true,
    };
    assert!(devkit_docs::resolve::resolve(&entry, &base, &cache, &opts).is_ok());
}

#[test]
fn a_git_entry_with_no_ref_is_a_hard_error_naming_sync() {
    let base = common::unique_tmp("noref");
    let repo = common::fixture_repo(&base.join("src"));
    let entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(repo),
        ..Default::default()
    };
    let err = devkit_docs::resolve::resolve(
        &entry,
        &base,
        &base.join("cache"),
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("docm sync"), "{err}");
}
