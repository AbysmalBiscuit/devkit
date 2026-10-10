//! `workspace setup --dry-run --summary` prints the summary text a real
//! `--summary` run writes, so a launcher can hand an issue to a cloud session
//! without creating a worktree to read it from.

#[path = "common/ghfake.rs"]
mod ghfake;

use std::path::{Path, PathBuf};

/// GitHub Issues as the tracker, which detection alone does not choose.
const GITHUB_TRACKER: &str = "\n[tracker]\nkind = \"github\"";

const BODY: &str = "## Plan\n\nRead the export path first.\n";

fn git(dir: &Path, args: &[&str]) -> String {
    devkit_git::Git::fixture(dir)
        .args(args.iter().copied())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e:#}"))
}

/// A bare `origin` holding the project's commit as `main`, the baseline a
/// real setup branches from.
fn with_origin(fake: &ghfake::Fake) -> tempfile::TempDir {
    let origin = tempfile::tempdir().unwrap();
    git(origin.path(), &["init", "-q", "--bare"]);
    git(fake.project(), &[
        "remote",
        "add",
        "origin",
        origin.path().to_str().unwrap(),
    ]);
    git(fake.project(), &["push", "-q", "origin", "HEAD:main"]);
    origin
}

/// Every path under `dir`, so a test can tell that a run created nothing.
fn tree(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(tree(&path));
        }
        out.push(path);
    }
    out.sort();
    out
}

fn json(out: &std::process::Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

#[test]
fn a_dry_run_prints_the_github_summary_a_real_setup_writes() {
    let fake = ghfake::Fake::without_pr(GITHUB_TRACKER);
    fake.serve_issue(BODY);
    let _origin = with_origin(&fake);
    let branches = git(fake.project(), &["branch", "--format=%(refname:short)"]);
    let before = tree(fake.project());

    let dry = json(&fake.issue(&[
        "setup",
        "7",
        "--slug",
        "fix",
        "--summary",
        "--dry-run",
        "--no-gitignore",
    ]));

    assert_eq!(dry["summary_text"], BODY, "{dry}");
    assert_eq!(tree(fake.project()), before, "the dry run created a file");
    assert_eq!(
        git(fake.project(), &["branch", "--format=%(refname:short)"]),
        branches,
        "the dry run created a branch"
    );

    let real = json(&fake.issue(&["setup", "7", "--slug", "fix", "--summary", "--no-gitignore"]));

    assert_eq!(real["summary"], dry["summary"]);
    let path = real["summary"].as_str().expect("a summary path");
    assert_eq!(std::fs::read_to_string(path).unwrap(), BODY);
}

#[test]
fn a_dry_run_without_summary_prints_no_summary_text_or_reads_the_issue() {
    let fake = ghfake::Fake::without_pr(GITHUB_TRACKER);
    fake.serve_issue(BODY);

    let dry = json(&fake.issue(&["setup", "7", "--slug", "fix", "--dry-run", "--no-gitignore"]));

    assert!(dry.get("summary_text").is_none(), "{dry}");
    let calls = fake.calls();
    assert!(!calls.contains("issues/7"), "{calls}");
    assert!(!calls.contains("issue view"), "{calls}");
}
