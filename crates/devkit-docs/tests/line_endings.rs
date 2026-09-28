//! A docs checkout mirrors upstream blobs byte for byte, so the user's own
//! end-of-line settings must not make one look modified.
//!
//! The user's config reaches every production git call through
//! `GIT_CONFIG_GLOBAL`, which is process-global. This file compiles to its own
//! binary and holds one `#[test]`, so no other thread reads the environment
//! while it is set.

use std::path::Path;

use devkit_docs::{
    manifest::{Ecosystem, LibEntry},
    resolve::{self, Options},
};

const CRLF: &[u8] = b"line one\r\nline two\r\n";

fn git(cwd: &Path, args: &[&str]) {
    devkit_git::Git::fixture(cwd)
        .args(args.iter().copied())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
}

/// An upstream that commits a CRLF file and says nothing about it in its own
/// `.gitattributes`.
fn crlf_upstream(dir: &Path) -> String {
    std::fs::create_dir_all(dir.join("skills")).unwrap();
    git(dir, &["init", "-b", "main"]);
    std::fs::write(dir.join("skills/SKILL.md"), CRLF).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "crlf"]);
    git(dir, &["tag", "v1"]);
    dir.to_str().unwrap().to_string()
}

/// Global git config that asks for end-of-line normalization the way a user's
/// own `~/.gitconfig` and attributes file do.
fn normalizing_global_config(dir: &Path) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let attributes = dir.join("attributes");
    std::fs::write(&attributes, "* text=auto\n*.md text\n").unwrap();
    let config = dir.join("gitconfig");
    std::fs::write(
        &config,
        format!(
            "[core]\n\tautocrlf = input\n\tattributesFile = {}\n",
            attributes.to_str().unwrap().replace('\\', "/")
        ),
    )
    .unwrap();
    config
}

#[test]
fn user_line_ending_normalization_leaves_crlf_checkouts_clean() {
    let base = tempfile::tempdir().unwrap();
    let repo = crlf_upstream(&base.path().join("upstream"));
    let config = normalizing_global_config(&base.path().join("home"));
    let cache = base.path().join("cache");
    let entry = LibEntry {
        name: "up".into(),
        ecosystem: Some(Ecosystem::Git),
        repo: Some(repo),
        r#ref: Some("v1".into()),
        ..Default::default()
    };

    // SAFETY: the only environment mutation in this binary.
    unsafe { std::env::set_var("GIT_CONFIG_GLOBAL", &config) };

    let resolved = resolve::resolve(&entry, base.path(), &cache, &Options::default())
        .expect("a fresh checkout of upstream's own bytes is clean");
    let skill = resolved.path.join("skills/SKILL.md");
    assert_eq!(std::fs::read(&skill).unwrap(), CRLF);
    let summary = devkit_docs::doctor_summary(&cache);
    assert!(summary.problems.is_empty(), "{:?}", summary.problems);

    // A clone made before docm pinned line endings has no `info/attributes`.
    // Rewriting the file with the same bytes makes git compare content again.
    std::fs::remove_file(cache.join("up/repo.git/info/attributes")).unwrap();
    std::fs::write(&skill, CRLF).unwrap();
    let summary = devkit_docs::doctor_summary(&cache);
    assert!(
        summary.problems.iter().any(|p| p.contains("SKILL.md")),
        "without the pin, user normalization reports the file: {:?}",
        summary.problems
    );

    resolve::resolve(&entry, base.path(), &cache, &Options::default())
        .expect("the next resolve repairs an existing clone");
    let summary = devkit_docs::doctor_summary(&cache);
    assert!(summary.problems.is_empty(), "{:?}", summary.problems);
}
