//! `issue setup`, `issue status` and `issue pr checkout` work where GitHub
//! refuses GraphQL, as the Claude Code cloud proxy does.
//!
//! Single reads go over REST. Each batched read keeps its one GraphQL request
//! where GraphQL answers and falls back to REST per item where it is refused.

#[path = "common/ghfake.rs"]
mod ghfake;

use std::path::{Path, PathBuf};

/// Config naming GitHub Issues as the tracker, which detection alone does not.
const GITHUB_TRACKER: &str = "\n[tracker]\nkind = \"github\"";

fn pr(number: u64, state: &'static str) -> ghfake::Pr {
    ghfake::Pr {
        number,
        state,
        is_draft: false,
        author: "LevValle",
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    devkit_git::Git::fixture(dir)
        .args(args.iter().copied())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e:#}"))
}

/// A bare `origin` holding the project's commit as `main` and as PR 7's head
/// ref, the way GitHub publishes it on the base repository.
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
    git(fake.project(), &[
        "push",
        "-q",
        "origin",
        "HEAD:refs/pull/7/head",
    ]);
    origin
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[test]
fn setup_writes_the_issue_summary_when_graphql_is_refused() {
    let fake = ghfake::Fake::without_pr(GITHUB_TRACKER);
    fake.refuse_graphql();
    fake.serve_issue("The summary devkit read over REST.");
    let _origin = with_origin(&fake);

    let out = fake.issue(&["setup", "7", "--slug", "fix", "--summary", "--no-gitignore"]);

    let calls = fake.calls();
    assert!(out.status.success(), "{out:?}\n{calls}");
    assert!(calls.contains("--method GET repos/o/r/issues/7"), "{calls}");
    let summary = files_under(&fake.project().join("wts"))
        .into_iter()
        .filter(|p| !p.components().any(|c| c.as_os_str() == ".git"))
        .find(|p| {
            std::fs::read_to_string(p)
                .is_ok_and(|text| text.contains("The summary devkit read over REST."))
        });
    assert!(
        summary.is_some(),
        "no summary file holds the issue body\n{calls}"
    );
}

#[test]
fn pr_checkout_checks_out_the_pr_when_graphql_is_refused() {
    let fake = ghfake::Fake::without_pr("");
    fake.refuse_graphql();
    fake.serve_pr(&pr(7, "OPEN"));
    let _origin = with_origin(&fake);
    git(fake.project(), &["checkout", "-q", "-b", "main"]);
    let scratch = tempfile::tempdir().unwrap();
    let worktree = scratch.path().join("pr-7");

    let out = fake.issue(&["pr", "checkout", "#7", worktree.to_str().unwrap()]);

    let calls = fake.calls();
    assert!(out.status.success(), "{out:?}\n{calls}");
    assert_eq!(
        git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "lev/eng-1-fix"
    );
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]).trim(), fake.head());
    let record = std::fs::read_to_string(worktree.join(".devkit").join("issue.toml")).unwrap();
    assert!(record.contains("number = 7"), "{record}");
}

/// Two issue worktrees: `wts/a` on the PR branch with issue 7 and no recorded
/// PR, so its PR is found by head branch, and `wts/b` with no issue and PR 9
/// recorded, so its PR is read by number.
fn status_project() -> (ghfake::Fake, tempfile::TempDir) {
    let fake = ghfake::Fake::new(GITHUB_TRACKER, &pr(7, "OPEN"));
    let origin = with_origin(&fake);
    git(fake.project(), &["checkout", "-q", "-b", "main"]);
    let a = fake.project().join("wts").join("a");
    let b = fake.project().join("wts").join("b");
    git(fake.project(), &[
        "worktree",
        "add",
        "-q",
        a.to_str().unwrap(),
        "lev/eng-1-fix",
    ]);
    git(fake.project(), &[
        "worktree",
        "add",
        "-q",
        "-b",
        "lev/other",
        b.to_str().unwrap(),
    ]);
    let record = |dir: &Path, issue: &str, pr: Option<u64>| {
        devkit_common::record::write(dir, &devkit_common::record::IssueRecord {
            issue: issue.into(),
            slug: "fix".into(),
            pr: pr.map(|number| devkit_common::forge::PrLocator {
                repo: Some("o/r".into()),
                number,
            }),
            ..Default::default()
        })
        .expect("write issue record");
    };
    record(&a, "7", None);
    record(&b, "", Some(9));
    (fake, origin)
}

/// The GraphQL answer to every batched read `issue status` makes, under the
/// aliases each query gives its items.
fn status_graphql(head: &str) -> String {
    let node = |n: u64, state: &str| {
        serde_json::json!({
            "number": n, "state": state, "url": format!("https://github.com/o/r/pull/{n}"),
            "title": "t", "headRefName": "lev/eng-1-fix", "headRefOid": head, "isDraft": false,
            "author": { "login": "LevValle" }, "headRepositoryOwner": { "login": "o" }
        })
    };
    serde_json::json!({ "data": {
        "repository": {
            "b0": { "totalCount": 1, "nodes": [node(7, "OPEN")] },
            "i0": { "state": "CLOSED", "stateReason": "COMPLETED" }
        },
        "r0": { "p0": node(9, "MERGED") }
    } })
    .to_string()
}

#[test]
fn status_reports_the_same_prs_and_states_when_graphql_is_refused() {
    let (graphql, _graphql_origin) = status_project();
    graphql.serve_graphql(&status_graphql(graphql.head()));
    let (refused, _refused_origin) = status_project();
    refused.serve_pr(&pr(9, "MERGED"));
    refused.serve_rest_issue(7, "closed", Some("completed"), "");
    refused.refuse_graphql();

    let want = graphql.issue(&["status"]);
    let got = refused.issue(&["status"]);

    let want_out = String::from_utf8_lossy(&want.stdout);
    let got_out = String::from_utf8_lossy(&got.stdout);
    assert!(want.status.success(), "{want:?}\n{}", graphql.calls());
    assert!(got.status.success(), "{got:?}\n{}", refused.calls());
    for cell in ["#7", "#9", "OPEN", "MERGED", "Done"] {
        assert!(want_out.contains(cell), "{cell} missing: {want_out}");
    }
    assert_eq!(got_out, want_out, "{}", refused.calls());
    let calls = refused.calls();
    for rest in [
        "--method GET repos/o/r/issues/7",
        "--method GET repos/o/r/pulls/9",
        "--method GET repos/o/r/pulls?head=o%3Alev%2Feng-1-fix&state=all",
    ] {
        assert!(calls.contains(rest), "{rest} missing: {calls}");
    }
}

/// Each batched read `issue status` makes is one GraphQL request where GraphQL
/// answers, sent nowhere else.
#[test]
fn status_makes_one_graphql_request_per_batched_read() {
    let (fake, _origin) = status_project();
    fake.serve_graphql(&status_graphql(fake.head()));

    let out = fake.issue(&["status"]);

    let calls = fake.calls();
    assert!(out.status.success(), "{out:?}\n{calls}");
    let graphql: Vec<&str> = calls
        .lines()
        .filter(|l| l.starts_with("api graphql"))
        .collect();
    for (read, marker) in [
        ("PRs by head branch", "pullRequests(headRefName"),
        ("PRs by number", "pullRequest(number"),
        ("issue states", "issue(number"),
    ] {
        assert_eq!(
            graphql.iter().filter(|l| l.contains(marker)).count(),
            1,
            "{read}: {calls}"
        );
    }
    assert_eq!(graphql.len(), 3, "{calls}");
    assert!(!calls.contains("--method GET"), "a REST read: {calls}");
}
