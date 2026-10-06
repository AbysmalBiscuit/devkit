//! `issue pr render`: the PR title and body `issue pr create` would send,
//! printed for a PR opened some other way, with a receipt per agent session.

#[path = "common/ghfake.rs"]
mod ghfake;

use std::{path::Path, process::Output};

/// Templates reading the worktree's record, the proof, the model and the
/// harness, the way a project's own `pr_title` and `pr_body` do.
const CONFIG: &str = r#"
[templates]
pr_title = "{{ issue }}: {{ input }}"
pr_body = """
Closes {{ issue }} on {{ branch }}

{{ input }}

## Proof

{{ proof }}

Generated with {{ agent_model }} via {{ agent_harness }}
"""

[templates.variables]
proof = { default = "", required = "agents", description = "one line per Done when item" }
agent_model = { default = "", required = "agents" }
agent_harness = { default = "", required = "agents" }
"#;

const ARGS: &[&str] = &[
    "--pr-title",
    "add a thing",
    "--pr-body",
    "Why it matters.\n\nHow it works.  \n",
    "--arg",
    "proof=1. test `a`\n2. test `b`",
    "--arg",
    "agent_model=Claude Opus 5.5",
    "--arg",
    "agent_harness=[Claude Code](https://claude.com/claude-code)",
];

fn project() -> ghfake::Fake {
    let fake = ghfake::Fake::without_pr(CONFIG);
    fake.record_issue("ENG-1");
    fake.create_opens(&ghfake::Pr {
        number: 9,
        state: "OPEN",
        is_draft: true,
        author: "LevValle",
    });
    fake
}

fn render(fake: &ghfake::Fake, session: Option<&str>, args: &[&str]) -> Output {
    let argv: Vec<&str> = ["pr", "render"].iter().chain(args).copied().collect();
    fake.issue_in_session("agent", session, &argv)
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn printed(out: &Output) -> (String, String) {
    assert!(out.status.success(), "{}", stderr(out));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    (
        json["title"].as_str().unwrap().to_string(),
        json["body"].as_str().unwrap().to_string(),
    )
}

fn receipts(project: &Path, session: &str) -> Vec<String> {
    let mut names: Vec<String> =
        std::fs::read_dir(project.join(".devkit").join("pr-receipts").join(session))
            .map(|d| {
                d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
    names.sort();
    names
}

/// A receipt vouches for the text with line endings and edge whitespace
/// folded, so a client that trims a trailing newline still matches.
fn receipt_name(prefix: &str, text: &str) -> String {
    let folded: Vec<&str> = text.lines().map(str::trim_end).collect();
    let digest = devkit_common::harness_log::redact::digest(folded.join("\n").trim());
    format!("{prefix}{}", digest.trim_start_matches("sha256:"))
}

#[test]
fn render_prints_what_create_sends() {
    let fake = project();
    let created = fake.issue_in_session(
        "agent",
        None,
        &[["pr", "create", "--no-push"].as_slice(), ARGS].concat(),
    );
    assert!(created.status.success(), "{}", stderr(&created));
    let sent_title = fake.created_pr_arg("--title").expect("gh pr create ran");
    let sent_body = fake.created_pr_arg("--body").expect("gh pr create ran");

    let (title, body) = printed(&render(&fake, None, ARGS));

    assert_eq!(title, sent_title);
    assert_eq!(body, sent_body);
    assert_eq!(title, "ENG-1: add a thing");
    assert!(
        body.starts_with("Closes ENG-1 on lev/eng-1-fix\n"),
        "{body}"
    );
    assert!(body.contains("1. test `a`\n2. test `b`"), "{body}");
    assert!(
        body.contains("Claude Opus 5.5 via [Claude Code](https://claude.com/claude-code)"),
        "{body}"
    );
}

#[test]
fn a_session_render_records_a_receipt_for_that_session() {
    let fake = project();
    let out = render(&fake, Some("S1"), ARGS);
    let (title, body) = printed(&out);

    assert_eq!(receipts(fake.project(), "S1"), vec![
        receipt_name("body-", &body),
        receipt_name("title-", &title),
    ]);
    assert!(
        !fake.project().join(".devkit/issue-receipts").exists(),
        "a PR render vouches for no issue write"
    );
    assert!(!fake.calls().contains("pr create"), "{}", fake.calls());
}

#[test]
fn a_render_outside_a_session_says_no_receipt_was_written() {
    let fake = project();
    let out = render(&fake, None, ARGS);
    printed(&out);

    assert!(
        stderr(&out).contains("no receipt written"),
        "{}",
        stderr(&out)
    );
    assert!(!fake.project().join(".devkit/pr-receipts").exists());
}

#[test]
fn a_missing_required_arg_is_refused_by_name_before_rendering() {
    let fake = project();
    let without_proof: Vec<&str> = ARGS
        .chunks(2)
        .filter(|pair| !pair[1].starts_with("proof="))
        .flatten()
        .copied()
        .collect();
    let out = render(&fake, Some("S1"), &without_proof);

    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "nothing rendered");
    let err = stderr(&out);
    assert!(err.contains("issue pr render"), "{err}");
    assert!(err.contains("--arg proof=..."), "{err}");
    assert!(err.contains("one line per Done when item"), "{err}");
    assert!(receipts(fake.project(), "S1").is_empty());
}

#[cfg(unix)]
#[test]
fn a_symlinked_receipt_store_is_refused_and_its_target_survives() {
    let fake = project();
    let outside = tempfile::tempdir().unwrap();
    let old = outside.path().join("old-session");
    std::fs::create_dir(&old).unwrap();
    std::fs::File::open(&old)
        .unwrap()
        .set_modified(
            std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 24 * 60 * 60),
        )
        .unwrap();
    std::fs::create_dir_all(fake.project().join(".devkit")).unwrap();
    std::os::unix::fs::symlink(outside.path(), fake.project().join(".devkit/pr-receipts")).unwrap();

    let out = render(&fake, Some("S1"), ARGS);

    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("symlink"), "{err}");
    assert!(err.contains("pr-receipts"), "{err}");
    assert!(old.exists(), "the symlink's target was swept");
    assert!(!outside.path().join("S1").exists());
}
