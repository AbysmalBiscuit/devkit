//! `workspace end` on a worktree with no issue id or no PR. Such a worktree is
//! finished only when its tree is clean, its HEAD is on `origin/<default>`
//! after a fetch of that branch, and no open PR has its branch as head.

#[path = "common/ghfake.rs"]
mod ghfake;

use std::path::{Path, PathBuf};

/// A project that declares no tracker, so no issue state holds a worktree.
const NO_TRACKER: &str = "\n[tracker]\nkind = \"none\"\n";

/// A project on `main` whose `origin` is a local bare repository holding
/// `main`, with `origin/HEAD` naming it.
fn project() -> ghfake::Fake {
    let fake = ghfake::Fake::without_pr(NO_TRACKER);
    let origin = fake.state().join("origin.git");
    git(fake.state(), &[
        "init",
        "-q",
        "--bare",
        origin.to_str().unwrap(),
    ]);
    git(fake.project(), &["checkout", "-q", "main"]);
    git(fake.project(), &[
        "remote",
        "add",
        "origin",
        origin.to_str().unwrap(),
    ]);
    git(fake.project(), &["push", "-q", "origin", "main"]);
    git(fake.project(), &["remote", "set-head", "origin", "main"]);
    fake
}

/// A worktree on a new `branch` started at `main`.
fn worktree(fake: &ghfake::Fake, branch: &str) -> PathBuf {
    let wt = fake.project().join("wts").join(branch.replace('/', "-"));
    git(fake.project(), &[
        "worktree",
        "add",
        "-q",
        "-b",
        branch,
        wt.to_str().unwrap(),
        "main",
    ]);
    wt
}

/// Answers the PR lookup of the one worktree branch with `prs`.
fn serve_prs(fake: &ghfake::Fake, prs: &[serde_json::Value]) {
    fake.serve_graphql(
        &serde_json::json!({ "data": { "repository": {
            "b0": { "totalCount": prs.len(), "nodes": prs },
        } } })
        .to_string(),
    );
}

fn pr(number: u64, state: &str, branch: &str, head: &str) -> serde_json::Value {
    serde_json::json!({
        "number": number, "state": state,
        "url": format!("https://github.com/o/r/pull/{number}"),
        "title": "t", "headRefName": branch, "headRefOid": head,
        "isDraft": false, "author": { "login": "LevValle" },
        "headRepositoryOwner": { "login": "o" }
    })
}

fn end(fake: &ghfake::Fake) -> String {
    let out = fake.issue(&["end", "--yes", "--no-preserve"]);
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

fn git(cwd: &Path, args: &[&str]) {
    devkit_git::Git::fixture(cwd)
        .args(args.iter().copied())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
}

fn head(cwd: &Path) -> String {
    devkit_git::Git::fixture(cwd)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap()
        .trim()
        .to_string()
}

/// A scratch checkout made to look at a PR: no issue id, no PR, and nothing
/// that `origin/main` lacks. Its ignored build output goes with it.
#[test]
fn a_scratch_checkout_on_the_default_branch_is_removed_with_its_build_output() {
    let fake = project();
    std::fs::write(fake.project().join(".git/info/exclude"), "target/\n").unwrap();
    let wt = worktree(&fake, "check-pr");
    std::fs::create_dir_all(wt.join("target/debug")).unwrap();
    std::fs::write(wt.join("target/debug/app"), "build output").unwrap();
    serve_prs(&fake, &[]);

    let output = end(&fake);

    assert!(!wt.exists(), "the scratch checkout survived: {output}");
}

/// An issue closed through a PR from another branch: the worktree branch has
/// no PR of its own, and its HEAD is already on `origin/main`.
#[test]
fn an_issue_worktree_with_no_pr_on_the_default_branch_is_removed() {
    let fake = project();
    let wt = worktree(&fake, "lev/eng-50-count-asks");
    serve_prs(&fake, &[]);

    let output = end(&fake);

    assert!(!wt.exists(), "the worktree survived: {output}");
}

/// A merged PR's branch whose name carries no issue id.
#[test]
fn a_merged_branch_with_no_issue_id_is_removed() {
    let fake = project();
    let wt = worktree(&fake, "fix-web-open-the-page");
    git(&wt, &["commit", "-q", "--allow-empty", "-m", "the fix"]);
    git(&wt, &["push", "-q", "origin", "HEAD:main"]);
    serve_prs(&fake, &[pr(
        89,
        "MERGED",
        "fix-web-open-the-page",
        &head(&wt),
    )]);

    let output = end(&fake);

    assert!(!wt.exists(), "the worktree survived: {output}");
}

#[test]
fn a_dirty_scratch_checkout_is_held() {
    let fake = project();
    let wt = worktree(&fake, "check-pr");
    std::fs::write(wt.join("notes.txt"), "work").unwrap();
    serve_prs(&fake, &[]);

    let output = end(&fake);

    assert!(wt.exists(), "a dirty worktree was removed: {output}");
    assert!(
        output.contains("dirty") && !output.contains("not an issue worktree"),
        "the tree alone holds the row: {output}"
    );
}

#[test]
fn a_scratch_checkout_with_commits_off_the_default_branch_is_held() {
    let fake = project();
    let wt = worktree(&fake, "check-pr");
    git(&wt, &["commit", "-q", "--allow-empty", "-m", "local work"]);
    serve_prs(&fake, &[]);

    let output = end(&fake);

    assert!(wt.exists(), "unpushed work was removed: {output}");
    assert!(output.contains("HEAD not on origin/main"), "{output}");
}

#[test]
fn a_scratch_checkout_with_an_open_pr_is_held() {
    let fake = project();
    let wt = worktree(&fake, "check-pr");
    serve_prs(&fake, &[pr(12, "OPEN", "check-pr", &head(&wt))]);

    let output = end(&fake);

    assert!(
        wt.exists(),
        "a worktree with an open PR was removed: {output}"
    );
    assert!(output.contains("PR #12 open"), "{output}");
}

/// A fetch that fails leaves `origin/main` as stale as it was, so it proves
/// nothing about where HEAD landed.
#[test]
fn a_failed_fetch_holds_a_scratch_checkout() {
    let fake = project();
    let wt = worktree(&fake, "check-pr");
    let gone = fake.state().join("gone.git");
    git(fake.project(), &[
        "remote",
        "set-url",
        "origin",
        gone.to_str().unwrap(),
    ]);
    serve_prs(&fake, &[]);

    let output = end(&fake);

    assert!(wt.exists(), "the worktree was removed: {output}");
    assert!(output.contains("fetch of origin/main failed"), "{output}");
}

/// A worktree an older devkit set up: its record is `.devkit/issue.toml` and
/// its summary `ISSUE_SUMMARY_<ID>.md`, outside the worktree as a relative
/// `issue_summary_path` puts it.
#[test]
fn a_worktree_an_older_devkit_made_reports_and_ends_with_its_summary() {
    let fake = project();
    let wt = worktree(&fake, "count-asks");
    let summary = fake.project().join("wts").join("ISSUE_SUMMARY_ENG-50.md");
    std::fs::write(&summary, "# ENG-50\n").unwrap();
    std::fs::create_dir_all(wt.join(".devkit")).unwrap();
    std::fs::write(wt.join(".devkit").join(".gitignore"), "*\n").unwrap();
    std::fs::write(
        wt.join(".devkit").join("issue.toml"),
        format!(
            "issue = \"ENG-50\"\nslug = \"count-asks\"\napps = []\nsummary = {:?}\n",
            summary.to_str().unwrap()
        ),
    )
    .unwrap();
    serve_prs(&fake, &[]);

    let status = fake.issue(&["status"]);
    let text = String::from_utf8_lossy(&status.stdout);
    assert!(status.status.success(), "{status:?}");
    assert!(text.contains("ENG-50"), "the record's id is missing: {text}");

    serve_prs(&fake, &[]);
    let output = end(&fake);

    assert!(!wt.exists(), "the worktree survived: {output}");
    assert!(!summary.exists(), "the summary survived: {output}");
}
