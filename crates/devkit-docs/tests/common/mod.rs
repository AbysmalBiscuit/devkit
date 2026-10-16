//! Shared integration-test helpers: a local fixture git repo with two tagged
//! versions (v1.0.0 -> "// v1", v1.1.0 tip -> "// v2").

use std::path::Path;

use devkit_git::template::{Step, Template};

fn sh(args: &[&str], cwd: &Path) {
    devkit_git::Git::fixture(cwd)
        .args(args.iter().copied())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
}

const FIXTURE: [Step; 10] = [
    Step::Git(&["init", "-b", "main"]),
    Step::Write("docs/guide.md", "# guide"),
    Step::Write("src/lib.rs", "// v1"),
    Step::Git(&["add", "."]),
    Step::Git(&["commit", "-m", "v1"]),
    Step::Git(&["tag", "v1.0.0"]),
    Step::Write("src/lib.rs", "// v2"),
    Step::Git(&["add", "."]),
    Step::Git(&["commit", "-m", "v2"]),
    Step::Git(&["tag", "v1.1.0"]),
];

/// Copies the fixture repo into `dir` and returns its path as a clone URL
/// (plain local path).
pub fn fixture_repo(dir: &Path) -> String {
    Template::of(Path::new(env!("CARGO_TARGET_TMPDIR")), &FIXTURE).copy_to(dir);
    dir.to_str().unwrap().to_string()
}

/// Serve an already-built fixture the way a hosting service does, as a
/// `file://` URL that honors `--filter`. git ignores `--filter` for a clone from
/// a plain path, so a partial clone needs this URL to be partial at all.
#[allow(dead_code)]
pub fn serve_partial(dir: &Path) -> String {
    sh(&["config", "uploadpack.allowFilter", "true"], dir);
    sh(&["config", "uploadpack.allowAnySHA1InWant", "true"], dir);
    let path = dir.to_str().unwrap().replace('\\', "/");
    match path.strip_prefix('/') {
        Some(rooted) => format!("file:///{rooted}"),
        None => format!("file:///{path}"),
    }
}

/// Object ids of one type (`tree`, `blob`) held locally by `repo`, leaving out
/// what a partial clone could still fetch on demand.
#[allow(dead_code)]
pub fn local_objects(repo: &Path, kind: &str) -> Vec<String> {
    devkit_git::Git::fixture(repo)
        .args([
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname) %(objecttype)",
        ])
        .output()
        .unwrap()
        .lines()
        .filter_map(|line| line.split_once(' '))
        .filter(|(_, object_kind)| *object_kind == kind)
        .map(|(id, _)| id.to_string())
        .collect()
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
