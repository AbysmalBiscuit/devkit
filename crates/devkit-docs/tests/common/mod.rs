//! Shared integration-test helpers: unique temp dirs and a local fixture
//! git repo with two tagged versions (v1.0.0 → "// v1", v1.1.0 tip → "// v2").

use std::path::Path;

pub fn unique_tmp(tag: &str) -> devkit_testtmp::TmpDir {
    devkit_testtmp::dir(&format!("devkit-docs-it-{tag}"))
}

fn sh(args: &[&str], cwd: &Path) {
    devkit_common::cmd::capture("git", args, Some(cwd.to_str().unwrap())).unwrap();
}

/// Returns the repo path as a clone URL (plain local path).
pub fn fixture_repo(dir: &Path) -> String {
    std::fs::create_dir_all(dir).unwrap();
    sh(&["init", "-b", "main"], dir);
    sh(&["config", "user.email", "t@t"], dir);
    sh(&["config", "user.name", "t"], dir);
    sh(&["config", "commit.gpgsign", "false"], dir);
    sh(&["config", "tag.gpgsign", "false"], dir);
    std::fs::create_dir_all(dir.join("docs")).unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("docs/guide.md"), "# guide").unwrap();
    std::fs::write(dir.join("src/lib.rs"), "// v1").unwrap();
    sh(&["add", "."], dir);
    sh(&["commit", "-m", "v1"], dir);
    sh(&["tag", "v1.0.0"], dir);
    std::fs::write(dir.join("src/lib.rs"), "// v2").unwrap();
    sh(&["add", "."], dir);
    sh(&["commit", "-m", "v2"], dir);
    sh(&["tag", "v1.1.0"], dir);
    dir.to_str().unwrap().to_string()
}

/// `anyhow`'s `{:?}` appends a captured backtrace whenever `RUST_BACKTRACE` is
/// set — CI sets it globally. Two errors raised at different call sites capture
/// different ones, and the frames name registry paths and rustc hashes that no
/// golden can record. Assertions about a message and its cause chain compare
/// the part above it.
// `common` compiles into each integration test binary separately, and only the
// two comparing error renderings call this.
#[allow(dead_code)]
pub fn message(rendered: &str) -> &str {
    rendered
        .split_once("\n\nStack backtrace:")
        .map_or(rendered, |(head, _)| head)
}
