mod common;

use devkit_docs::manifest::{Ecosystem, LibEntry};
use devkit_docs::resolve::{self, Options};
use std::path::{Path, PathBuf};

/// Materialize `up` at `git_ref`, returning the cache root and the checkout.
fn materialize(tag: &str, git_ref: &str) -> (PathBuf, PathBuf) {
    let base = common::unique_tmp(tag);
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let entry = LibEntry {
        name: "up".into(),
        ecosystem: Some(Ecosystem::Git),
        repo: Some(repo),
        r#ref: Some(git_ref.into()),
        ..Default::default()
    };
    let resolved = resolve::resolve(&entry, &base, &cache, &Options::default()).unwrap();
    (cache, resolved.path)
}

fn git(cwd: &Path, args: &[&str]) {
    devkit_common::cmd::capture("git", args, Some(cwd.to_str().unwrap())).unwrap();
}

#[test]
fn a_materialized_cache_reports_no_problems() {
    let (cache, _) = materialize("doctor-clean", "v1.0.0");

    let summary = devkit_docs::doctor_summary(&cache);

    assert_eq!(summary.libs, 1);
    assert!(
        summary.problems.is_empty(),
        "a freshly resolved checkout is correct: {:?}",
        summary.problems
    );
}

#[test]
fn a_dirty_checkout_is_named() {
    let (cache, checkout) = materialize("doctor-dirty", "v1.0.0");
    std::fs::write(checkout.join("src/lib.rs"), "// local edit").unwrap();

    let summary = devkit_docs::doctor_summary(&cache);

    assert!(
        summary
            .problems
            .iter()
            .any(|p| p.contains("up/v1.0.0") && p.contains("src/lib.rs")),
        "source read from a modified checkout is not the released source: {:?}",
        summary.problems
    );
}

/// An untracked file inside a checkout is source a reader can find and cite,
/// so the sweep counts it exactly like a modified tracked file.
#[test]
fn an_untracked_file_in_a_checkout_is_named() {
    let (cache, checkout) = materialize("doctor-untracked", "v1.0.0");
    std::fs::write(checkout.join("src/new.rs"), "// planted").unwrap();

    let summary = devkit_docs::doctor_summary(&cache);

    assert!(
        summary.problems.iter().any(|p| p.contains("src/new.rs")),
        "{:?}",
        summary.problems
    );
}

#[test]
fn a_checkout_whose_head_drifted_from_its_recorded_commit_is_named() {
    let (cache, checkout) = materialize("doctor-head", "v1.0.0");
    git(&checkout, &["checkout", "--detach", "v1.1.0"]);
    let drifted = devkit_common::cmd::capture(
        "git",
        &["rev-parse", "HEAD"],
        Some(checkout.to_str().unwrap()),
    )
    .unwrap()
    .trim()
    .to_string();

    let summary = devkit_docs::doctor_summary(&cache);

    assert!(
        summary
            .problems
            .iter()
            .any(|p| p.contains("up/v1.0.0") && p.contains(&drifted)),
        "a checkout sitting at another commit must be reported: {:?}",
        summary.problems
    );
}

#[test]
fn the_sweep_never_descends_into_a_control_directory() {
    let (cache, _) = materialize("doctor-controls", "v1.0.0");
    std::fs::create_dir_all(cache.join("registry.locks/not-a-checkout")).unwrap();

    let summary = devkit_docs::doctor_summary(&cache);

    assert_eq!(summary.libs, 1);
    assert!(summary.problems.is_empty(), "{:?}", summary.problems);
}
