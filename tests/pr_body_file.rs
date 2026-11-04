//! `--pr-body-file`: the summary of `pr render`, `pr create` and `pr edit`,
//! read from a file or stdin into the `pr_body` template's `input`.

#[path = "common/ghfake.rs"]
mod ghfake;

use std::{path::PathBuf, process::Output};

const CONFIG: &str = r#"
[templates]
pr_body = """
## Summary

{{ input }}

## Proof

{{ proof }}
"""

[templates.variables]
proof = { default = "none" }
"#;

const PR_7: ghfake::Pr = ghfake::Pr {
    number: 7,
    state: "OPEN",
    is_draft: false,
    author: "LevValle",
};

/// What a shell's `"$(cat f)"` yields for the file [`summary_file`] writes.
const SUMMARY: &str = "Why it matters.\n\nHow it works.";

fn summary_file(fake: &ghfake::Fake) -> PathBuf {
    let path = fake.project().join("summary.md");
    std::fs::write(&path, format!("{SUMMARY}\n\n")).unwrap();
    path
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn rendered_body(out: &Output) -> String {
    assert!(out.status.success(), "{}", stderr(out));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    json["body"].as_str().unwrap().to_string()
}

fn render(fake: &ghfake::Fake, summary: &[&str]) -> Output {
    let mut argv = vec!["pr", "render", "--pr-title", "add a thing"];
    argv.extend_from_slice(summary);
    fake.issue(&argv)
}

#[test]
fn pr_render_prints_the_body_the_string_flag_prints() {
    let fake = ghfake::Fake::without_pr(CONFIG);
    let file = summary_file(&fake);

    let from_file = render(&fake, &["--pr-body-file", file.to_str().unwrap()]);
    let from_string = render(&fake, &["--pr-body", SUMMARY]);

    let body = rendered_body(&from_file);
    assert_eq!(body, rendered_body(&from_string));
    assert!(body.contains(SUMMARY), "{body}");
}

#[test]
fn pr_render_reads_stdin_for_a_dash() {
    let fake = ghfake::Fake::without_pr(CONFIG);

    let from_stdin = fake.issue_with_stdin(
        &[
            "pr",
            "render",
            "--pr-title",
            "add a thing",
            "--pr-body-file",
            "-",
        ],
        format!("{SUMMARY}\n").as_bytes(),
    );

    assert_eq!(
        rendered_body(&from_stdin),
        rendered_body(&render(&fake, &["--pr-body", SUMMARY]))
    );
}

#[test]
fn pr_create_sends_the_body_the_string_flag_sends() {
    let fake = ghfake::Fake::without_pr(CONFIG);
    fake.create_opens(&PR_7);
    let file = summary_file(&fake);
    let expected = rendered_body(&render(&fake, &["--pr-body", SUMMARY]));

    let out = fake.issue(&[
        "pr",
        "create",
        "--no-push",
        "--pr-title",
        "add a thing",
        "--pr-body-file",
        file.to_str().unwrap(),
    ]);

    assert!(out.status.success(), "{}\n{}", stderr(&out), fake.calls());
    assert_eq!(fake.created_pr_arg("--body").as_deref(), Some(&*expected));
}

#[test]
fn pr_create_reads_stdin_for_a_dash() {
    let fake = ghfake::Fake::without_pr(CONFIG);
    fake.create_opens(&PR_7);
    let expected = rendered_body(&render(&fake, &["--pr-body", SUMMARY]));

    let out = fake.issue_with_stdin(
        &[
            "pr",
            "create",
            "--no-push",
            "--pr-title",
            "add a thing",
            "--pr-body-file",
            "-",
        ],
        format!("{SUMMARY}\n").as_bytes(),
    );

    assert!(out.status.success(), "{}\n{}", stderr(&out), fake.calls());
    assert_eq!(fake.created_pr_arg("--body").as_deref(), Some(&*expected));
}

/// The body the run sent, through whichever write it made.
fn edited_body(fake: &ghfake::Fake) -> String {
    fake.edited_pr_arg("--body")
        .or_else(|| fake.patched_pr_field("body"))
        .unwrap_or_else(|| panic!("no body was written: {}", fake.calls()))
}

const LIVE: &str = "## Summary\n\nThe old summary.\n\n## Proof\n\n1. the old proof\n";

#[test]
fn pr_edit_replaces_the_summary_from_a_file() {
    let fake = ghfake::Fake::new(CONFIG, &PR_7);
    fake.serve_pr_body(&PR_7, LIVE);
    let file = summary_file(&fake);

    let out = fake.issue(&[
        "pr",
        "edit",
        "--no-push",
        "--pr-body-file",
        file.to_str().unwrap(),
    ]);

    assert!(out.status.success(), "{}\n{}", stderr(&out), fake.calls());
    assert_eq!(
        edited_body(&fake),
        LIVE.replace("The old summary.", SUMMARY)
    );
}

#[test]
fn pr_edit_reads_stdin_for_a_dash() {
    let fake = ghfake::Fake::new(CONFIG, &PR_7);
    fake.serve_pr_body(&PR_7, LIVE);

    let out = fake.issue_with_stdin(
        &["pr", "edit", "--no-push", "--pr-body-file", "-"],
        format!("{SUMMARY}\n").as_bytes(),
    );

    assert!(out.status.success(), "{}\n{}", stderr(&out), fake.calls());
    assert_eq!(
        edited_body(&fake),
        LIVE.replace("The old summary.", SUMMARY)
    );
}

#[test]
fn both_summary_flags_together_are_refused() {
    let fake = ghfake::Fake::without_pr(CONFIG);
    let file = summary_file(&fake);

    for verb in ["render", "create", "edit"] {
        let out = fake.issue(&[
            "pr",
            verb,
            "--pr-body",
            SUMMARY,
            "--pr-body-file",
            file.to_str().unwrap(),
        ]);

        let err = stderr(&out);
        assert!(!out.status.success(), "pr {verb}: {err}");
        assert!(
            err.contains("--pr-body") && err.contains("cannot be used with"),
            "pr {verb}: {err}"
        );
    }
    assert!(!fake.calls().contains("pr create"), "{}", fake.calls());
}

#[test]
fn stdin_read_for_both_the_summary_and_an_arg_is_refused() {
    let fake = ghfake::Fake::without_pr(CONFIG);

    let out = fake.issue_with_stdin(
        &[
            "pr",
            "render",
            "--pr-title",
            "add a thing",
            "--pr-body-file",
            "-",
            "--arg-file",
            "proof=-",
        ],
        b"one read\n",
    );

    let err = stderr(&out);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("stdin"), "{err}");
}

/// A relative path names a file in the `-C` directory, not in the process
/// directory, even when the process directory has a file of the same name.
#[test]
fn relative_paths_resolve_against_the_start_directory() {
    let fake = ghfake::Fake::without_pr(CONFIG);
    fake.create_opens(&PR_7);
    summary_file(&fake);
    std::fs::write(fake.project().join("proof.md"), "1. from the project").unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::write(elsewhere.path().join("summary.md"), "Wrong summary.").unwrap();
    std::fs::write(elsewhere.path().join("proof.md"), "1. wrong proof").unwrap();
    let relative = [
        "--pr-title",
        "add a thing",
        "--pr-body-file",
        "summary.md",
        "--arg-file",
        "proof=proof.md",
    ];

    let rendered = fake.issue_from(
        elsewhere.path(),
        &[&["pr", "render"][..], &relative].concat(),
    );
    let body = rendered_body(&rendered);
    assert!(body.contains(SUMMARY), "{body}");
    assert!(body.contains("1. from the project"), "{body}");

    let created = fake.issue_from(
        elsewhere.path(),
        &[&["pr", "create", "--no-push"][..], &relative].concat(),
    );
    assert!(created.status.success(), "{}", stderr(&created));
    assert_eq!(fake.created_pr_arg("--body").as_deref(), Some(&*body));

    let edit = ghfake::Fake::new(CONFIG, &PR_7);
    edit.serve_pr_body(&PR_7, LIVE);
    summary_file(&edit);
    let edited = edit.issue_from(elsewhere.path(), &[
        "pr",
        "edit",
        "--no-push",
        "--pr-body-file",
        "summary.md",
    ]);
    assert!(edited.status.success(), "{}", stderr(&edited));
    assert_eq!(
        edited_body(&edit),
        LIVE.replace("The old summary.", SUMMARY)
    );
}

#[test]
fn a_missing_summary_file_is_named() {
    let fake = ghfake::Fake::without_pr(CONFIG);

    let out = render(&fake, &["--pr-body-file", "no-such-summary.md"]);

    let err = stderr(&out);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("no-such-summary.md"), "{err}");
}
