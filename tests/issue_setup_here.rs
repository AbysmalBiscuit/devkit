//! `issue setup --here` binds the checkout it runs in to an issue, for a
//! session that works on a branch it was handed and may create no worktree.

#[path = "common/ghfake.rs"]
mod ghfake;

use std::{path::Path, process::Output};

/// GitHub Issues as the tracker, and a `pr_body` that closes the record's
/// issue the way a GitHub project's own template does.
const CONFIG: &str = r#"
[tracker]
kind = "github"

[templates]
pr_body = "Closes #{{ issue }}\n\n{{ input }}"
"#;

fn git(dir: &Path, args: &[&str]) -> String {
    devkit_git::Git::fixture(dir)
        .args(args.iter().copied())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e:#}"))
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn setup_here_binds_the_checkout_without_a_worktree_or_branch() {
    let fake = ghfake::Fake::without_pr(CONFIG);
    let branches = git(fake.project(), &["branch", "--format=%(refname:short)"]);

    let out = fake.issue(&["setup", "--here", "7", "--slug", "fix", "--no-gitignore"]);

    assert!(out.status.success(), "{}", stderr(&out));
    let record = devkit_common::record::read(fake.project()).expect("an issue record");
    assert_eq!(record.issue, "7");
    assert_eq!(record.branch.as_deref(), Some("lev/eng-1-fix"));
    assert_eq!(
        git(fake.project(), &["branch", "--format=%(refname:short)"]),
        branches,
        "no branch was created"
    );
    assert_eq!(
        git(fake.project(), &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        1,
        "no worktree was created"
    );
    assert!(!fake.project().join("wts").exists());
}

#[test]
fn a_pr_opened_after_setup_here_closes_the_issue() {
    let fake = ghfake::Fake::without_pr(CONFIG);
    fake.create_opens(&ghfake::Pr {
        number: 9,
        state: "OPEN",
        is_draft: false,
        author: "LevValle",
    });
    let setup = fake.issue(&["setup", "--here", "7", "--slug", "fix", "--no-gitignore"]);
    assert!(setup.status.success(), "{}", stderr(&setup));

    let out = fake.issue(&[
        "pr",
        "create",
        "--no-push",
        "--pr-title",
        "t",
        "--pr-body",
        "Why.",
    ]);

    assert!(out.status.success(), "{}\n{}", stderr(&out), fake.calls());
    let body = fake.created_pr_arg("--body").expect("gh pr create ran");
    assert!(body.starts_with("Closes #7\n"), "{body}");
}

#[test]
fn setup_here_writes_the_summary_when_graphql_is_refused() {
    let fake = ghfake::Fake::without_pr(CONFIG);
    fake.refuse_graphql();
    fake.serve_issue("The summary devkit read over REST.");

    let out = fake.issue(&["setup", "--here", "7", "--summary", "--no-gitignore"]);

    assert!(out.status.success(), "{}\n{}", stderr(&out), fake.calls());
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let summary = report["summary"].as_str().expect("a summary path");
    assert_eq!(
        std::fs::read_to_string(summary).unwrap(),
        "The summary devkit read over REST."
    );
    let record = devkit_common::record::read(fake.project()).expect("an issue record");
    assert_eq!(record.summary.as_deref(), Some(summary));
}

#[test]
fn setup_here_refuses_the_default_branch_by_name() {
    let fake = ghfake::Fake::without_pr(CONFIG);
    git(fake.project(), &["checkout", "-q", "-b", "main"]);

    let out = fake.issue(&["setup", "--here", "7", "--slug", "fix", "--no-gitignore"]);

    let stderr = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("`main`"), "{stderr}");
    assert!(devkit_common::record::read(fake.project()).is_none());
}

#[test]
fn setup_here_refuses_a_checkout_bound_to_another_issue() {
    let fake = ghfake::Fake::without_pr(CONFIG);
    let first = fake.issue(&["setup", "--here", "7", "--slug", "fix", "--no-gitignore"]);
    assert!(first.status.success(), "{}", stderr(&first));

    let again = fake.issue(&["setup", "--here", "7", "--slug", "fix", "--no-gitignore"]);
    let other = fake.issue(&["setup", "--here", "8", "--slug", "fix", "--no-gitignore"]);

    assert!(again.status.success(), "{}", stderr(&again));
    assert!(!other.status.success());
    assert!(stderr(&other).contains("issue `7`"), "{}", stderr(&other));
    let record = devkit_common::record::read(fake.project()).expect("an issue record");
    assert_eq!(record.issue, "7");
}
