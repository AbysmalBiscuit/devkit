//! `workspace setup --here` binds the checkout it runs in to an issue, for a
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
    for baseline_ref in ["origin/main", "upstream/main"] {
        let fake = ghfake::Fake::without_pr(CONFIG);
        let config = fake.project().join("devkit.toml");
        let toml = std::fs::read_to_string(&config).unwrap().replace(
            r#"baseline_ref = "origin/main""#,
            &format!(r#"baseline_ref = "{baseline_ref}""#),
        );
        std::fs::write(&config, toml).unwrap();
        git(fake.project(), &[
            "remote",
            "add",
            "upstream",
            "https://github.com/o/r.git",
        ]);
        git(fake.project(), &["checkout", "-q", "-b", "main"]);

        let out = fake.issue(&["setup", "--here", "7", "--slug", "fix", "--no-gitignore"]);

        let stderr = stderr(&out);
        assert_eq!(out.status.code(), Some(1), "{baseline_ref}: {stderr}");
        assert!(stderr.contains("`main`"), "{baseline_ref}: {stderr}");
        assert!(devkit_common::record::read(fake.project()).is_none());
    }
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

#[test]
fn setup_here_again_refreshes_the_slug() {
    let fake = ghfake::Fake::without_pr(CONFIG);
    let first = fake.issue(&["setup", "--here", "7", "--slug", "fix", "--no-gitignore"]);
    assert!(first.status.success(), "{}", stderr(&first));

    let again = fake.issue(&["setup", "--here", "7", "--slug", "repair", "--no-gitignore"]);

    assert!(again.status.success(), "{}", stderr(&again));
    let record = devkit_common::record::read(fake.project()).expect("an issue record");
    assert_eq!(record.slug, "repair");
}

#[test]
fn setup_here_refuses_a_detached_head() {
    let fake = ghfake::Fake::without_pr(CONFIG);
    git(fake.project(), &["checkout", "-q", "--detach"]);

    let out = fake.issue(&["setup", "--here", "7", "--slug", "fix", "--no-gitignore"]);

    let stderr = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("HEAD is detached"), "{stderr}");
    assert!(devkit_common::record::read(fake.project()).is_none());
}

#[test]
fn setup_here_finds_the_default_branch_through_origin_head() {
    let fake = ghfake::Fake::without_pr(CONFIG);
    let config = fake.project().join("devkit.toml");
    let toml = std::fs::read_to_string(&config)
        .unwrap()
        .replace(r#"baseline_ref = "origin/main""#, r#"baseline_ref = """#);
    std::fs::write(&config, toml).unwrap();
    git(fake.project(), &[
        "update-ref",
        "refs/remotes/origin/main",
        "HEAD",
    ]);
    git(fake.project(), &[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ]);
    git(fake.project(), &["checkout", "-q", "-b", "main"]);

    let out = fake.issue(&["setup", "--here", "7", "--slug", "fix", "--no-gitignore"]);

    let stderr = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("refusing to bind the default branch `main`"),
        "{stderr}"
    );
    assert!(devkit_common::record::read(fake.project()).is_none());
}

#[test]
fn a_pr_from_another_branch_does_not_close_the_bound_issue() {
    let fake = ghfake::Fake::without_pr(
        r#"
[tracker]
kind = "github"

[templates]
pr_body = "{% if issue is defined %}Closes #{{ issue }}\n\n{% endif %}{{ input }}"
"#,
    );
    fake.create_opens(&ghfake::Pr {
        number: 9,
        state: "OPEN",
        is_draft: false,
        author: "LevValle",
    });
    let setup = fake.issue(&["setup", "--here", "7", "--slug", "fix", "--no-gitignore"]);
    assert!(setup.status.success(), "{}", stderr(&setup));
    git(fake.project(), &["switch", "-q", "-c", "lev/other-work"]);

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
    assert!(!body.contains("Closes #7"), "{body}");
}

#[test]
fn setup_here_refuses_a_pr_checkout() {
    let fake = ghfake::Fake::without_pr(CONFIG);
    let checkout = devkit_common::record::IssueRecord {
        issue: "7".into(),
        slug: "fix".into(),
        origin: Some(devkit_common::record::RecordOrigin::Checkout),
        events: Some(vec![]),
        ..Default::default()
    };
    devkit_common::record::write(fake.project(), &checkout).unwrap();

    let out = fake.issue(&["setup", "--here", "7", "--slug", "fix", "--no-gitignore"]);

    let stderr = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("`pr checkout`") && !stderr.contains("issue pr"),
        "{stderr}"
    );
    assert_eq!(devkit_common::record::read(fake.project()), Some(checkout));
}

#[test]
fn setup_here_writes_the_default_summary_inside_the_checkout_untracked() {
    let fake = ghfake::Fake::without_pr(CONFIG);
    fake.serve_issue("The summary devkit read over REST.");
    let status = || {
        git(fake.project(), &[
            "status",
            "--porcelain",
            "--untracked-files=all",
        ])
    };
    let before = status();

    let out = fake.issue(&["setup", "--here", "7", "--summary", "--no-gitignore"]);

    assert!(out.status.success(), "{}\n{}", stderr(&out), fake.calls());
    let expected = fake.project().join(".devkit").join("ISSUE_SUMMARY_7.md");
    assert!(expected.is_file(), "no summary at {}", expected.display());
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let reported = Path::new(report["summary"].as_str().expect("a summary path"));
    assert_eq!(
        reported.canonicalize().unwrap(),
        expected.canonicalize().unwrap()
    );
    assert_eq!(status(), before, "the summary shows up in git status");
}

/// A linked worktree of a bare repository on a feature branch: devkit derives
/// no `defaults.worktree_root` for it, and its config names none.
fn bare_main_checkout(
    fake: &ghfake::Fake,
    config: &str,
) -> (tempfile::TempDir, std::path::PathBuf) {
    let scratch = tempfile::tempdir().unwrap();
    let bare = scratch.path().join("origin.git");
    let wt = scratch.path().join("wt");
    git(scratch.path(), &[
        "clone",
        "-q",
        "--bare",
        fake.project().to_str().unwrap(),
        bare.to_str().unwrap(),
    ]);
    git(&bare, &[
        "worktree",
        "add",
        "-q",
        "-b",
        "lev/7-fix",
        wt.to_str().unwrap(),
    ]);
    let toml = std::fs::read_to_string(fake.project().join("devkit.toml"))
        .unwrap()
        .replace("worktree_root = \"wts\"\n", "")
        .replace("[templates]\n", &format!("[templates]\n{config}\n"));
    std::fs::write(wt.join("devkit.toml"), toml).unwrap();
    (scratch, wt)
}

#[test]
fn setup_here_refuses_a_relative_summary_path_without_a_worktree_root() {
    let fake = ghfake::Fake::without_pr(CONFIG);
    fake.serve_issue("The summary devkit read over REST.");
    let (_scratch, wt) = bare_main_checkout(
        &fake,
        r#"issue_summary_path = "ISSUE_SUMMARY_{{ issue }}.md""#,
    );

    let out = fake.devkit_with_stdin(
        &[
            "issue",
            "-C",
            wt.to_str().unwrap(),
            "setup",
            "--here",
            "7",
            "--summary",
            "--no-gitignore",
        ],
        b"",
    );

    let stderr = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("defaults.worktree_root"), "{stderr}");
    assert!(!wt.join("ISSUE_SUMMARY_7.md").exists());
    assert!(devkit_common::record::read(&wt).is_none());
}

#[test]
fn setup_adds_devkit_ignore_patterns_to_the_global_excludes() {
    let fake = ghfake::Fake::without_pr(CONFIG);
    let out = fake.issue(&["setup", "--here", "7", "--slug", "fix"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(fake.state().join("git/ignore")).expect("excludes written"),
        ".devkit/\n*.local\n*.local.*\n"
    );
}

#[test]
fn setup_with_no_gitignore_leaves_the_global_excludes_alone() {
    let fake = ghfake::Fake::without_pr(CONFIG);
    let out = fake.issue(&["setup", "--here", "7", "--slug", "fix", "--no-gitignore"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!fake.state().join("git/ignore").exists());
}
