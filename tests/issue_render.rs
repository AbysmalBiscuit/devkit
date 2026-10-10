//! `devkit ticket render`: the issue templates rendered for a tracker MCP call,
//! with a receipt per field the pre-tool-use hook checks the call against.

#[path = "common/testenv.rs"]
mod testenv;

use std::{path::Path, process::Output};

const TEMPLATES: &str = r#"
[templates]
issue_body = """
{{ input }}

## Acceptance criteria
{{ acceptance }}
"""

[templates.variables]
acceptance = { required = "agents", description = "observable outcomes that mean the issue is done" }
"#;

fn project() -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    devkit_git::Git::fixture(p.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    std::fs::write(p.path().join("devkit.toml"), TEMPLATES).unwrap();
    p
}

fn render(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let (_home, mut cmd) = testenv::isolated(env!("CARGO_BIN_EXE_devkit"));
    testenv::scrub_identity(&mut cmd);
    cmd.args(["issue", "render"])
        .args(args)
        .current_dir(dir)
        .env_remove("DEVKIT_CONFIG")
        .env("DEVKIT_CALLER", "agent");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn devkit issue render")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn receipts(project: &Path, session: &str) -> Vec<String> {
    let mut names: Vec<String> =
        std::fs::read_dir(project.join(".devkit").join("issue-receipts").join(session))
            .map(|d| {
                d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
    names.sort();
    names
}

fn is_receipt(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix).is_some_and(|hex| {
        hex.len() == 64 && hex.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
    })
}

#[test]
fn a_missing_required_arg_is_refused_by_name() {
    let p = project();
    let out = render(p.path(), &[("CLAUDE_CODE_SESSION_ID", "S1")], &[
        "--title", "T", "--body", "B",
    ]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("--arg acceptance=..."), "{err}");
    assert!(err.contains("observable outcomes"), "{err}");
    assert!(receipts(p.path(), "S1").is_empty());
}

#[test]
fn render_prints_json_and_writes_both_receipts() {
    let p = project();
    let out = render(p.path(), &[("CLAUDE_CODE_SESSION_ID", "S1")], &[
        "--title",
        "T",
        "--body",
        "B",
        "--arg",
        "acceptance=A",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["title"], "T");
    let body = json["body"].as_str().unwrap();
    assert!(body.starts_with('B'), "{body}");
    assert!(body.contains("## Acceptance criteria\nA"), "{body}");
    let names = receipts(p.path(), "S1");
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(is_receipt(&names[0], "body-"), "{names:?}");
    assert!(is_receipt(&names[1], "title-"), "{names:?}");
}

#[test]
fn two_harness_ids_each_get_receipts() {
    let p = project();
    let out = render(
        p.path(),
        &[("CLAUDE_CODE_SESSION_ID", "S1"), ("CODEX_SESSION_ID", "S2")],
        &["--title", "T", "--arg", "acceptance=A"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(receipts(p.path(), "S1").len(), 2);
    assert_eq!(receipts(p.path(), "S2").len(), 2);
}

#[test]
fn an_omitted_body_still_gets_a_receipt() {
    let p = project();
    let out = render(p.path(), &[("CLAUDE_CODE_SESSION_ID", "S1")], &[
        "--title",
        "T",
        "--arg",
        "acceptance=A",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        receipts(p.path(), "S1")
            .iter()
            .any(|n| is_receipt(n, "body-"))
    );
}

#[test]
fn no_session_renders_without_receipts_outside_a_checkout() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("devkit.toml"), TEMPLATES).unwrap();
    let out = render(dir.path(), &[("DEVKIT_CALLER", "human")], &[
        "--title",
        "T",
        "--body",
        "B",
        "--arg",
        "acceptance=A",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["title"], "T");
    assert!(stderr(&out).contains("no receipt"), "{}", stderr(&out));
    assert!(!dir.path().join(".devkit").exists());
}

#[test]
fn an_invalid_session_id_is_refused() {
    let p = project();
    let out = render(p.path(), &[("CLAUDE_CODE_SESSION_ID", "../x")], &[
        "--title",
        "T",
        "--arg",
        "acceptance=A",
    ]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("session id"), "{}", stderr(&out));
    assert!(!p.path().join(".devkit").join("x").exists());
}

#[test]
fn an_invalid_second_session_id_writes_no_receipt_for_the_first() {
    let p = project();
    let out = render(
        p.path(),
        &[
            ("CLAUDE_CODE_SESSION_ID", "S1"),
            ("CODEX_SESSION_ID", "../x"),
        ],
        &["--title", "T", "--arg", "acceptance=A"],
    );
    assert!(!out.status.success());
    assert!(stderr(&out).contains("session id"), "{}", stderr(&out));
    assert!(receipts(p.path(), "S1").is_empty());
}

#[test]
fn an_empty_title_is_refused() {
    let p = project();
    let out = render(p.path(), &[("CLAUDE_CODE_SESSION_ID", "S1")], &[
        "--title",
        "  ",
        "--arg",
        "acceptance=A",
    ]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("--title is required"),
        "{}",
        stderr(&out)
    );
    assert!(receipts(p.path(), "S1").is_empty());
}

#[cfg(unix)]
#[test]
fn a_symlinked_devkit_dir_is_refused_and_its_target_survives() {
    let p = project();
    let outside = tempfile::tempdir().unwrap();
    let old = outside.path().join("issue-receipts").join("old-session");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::File::open(&old)
        .unwrap()
        .set_modified(
            std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 24 * 60 * 60),
        )
        .unwrap();
    std::os::unix::fs::symlink(outside.path(), p.path().join(".devkit")).unwrap();

    let out = render(p.path(), &[("CLAUDE_CODE_SESSION_ID", "S1")], &[
        "--title",
        "T",
        "--arg",
        "acceptance=A",
    ]);

    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("symlink"), "{err}");
    assert!(err.contains(".devkit"), "{err}");
    assert!(old.exists(), "the symlink's target was swept");
    assert!(!outside.path().join("issue-receipts").join("S1").exists());
}

/// A project at `config`, under a `devkit.local.toml` of `local` when given.
fn project_with(config: &str, local: Option<&str>) -> tempfile::TempDir {
    let p = project();
    std::fs::write(p.path().join("devkit.toml"), config).unwrap();
    if let Some(local) = local {
        std::fs::write(p.path().join("devkit.local.toml"), local).unwrap();
    }
    p
}

fn rendered(p: &Path) -> (String, String) {
    let out = render(p, &[("CLAUDE_CODE_SESSION_ID", "S1")], &[
        "--title", "x", "--body", "y",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    (
        json["title"].as_str().unwrap().to_string(),
        json["body"].as_str().unwrap().to_string(),
    )
}

#[test]
fn tickets_render_from_the_ticket_templates() {
    let p = project_with(
        "[templates]\n\
         ticket_title = \"T: {{ input }}\"\n\
         ticket_body = \"{{ ticket_title }} / {{ issue_title }} / {{ input }}\"\n",
        None,
    );
    assert_eq!(rendered(p.path()), ("T: x".into(), "T: x / T: x / y".into()));
}

#[test]
fn a_project_overriding_the_issue_templates_keeps_its_override() {
    let p = project_with(
        "[templates]\n\
         issue_title = \"Old: {{ input }}\"\n\
         issue_body = \"{{ issue_title }} | {{ input }}\"\n",
        None,
    );
    assert_eq!(rendered(p.path()), ("Old: x".into(), "Old: x | y".into()));

    let p = project_with(
        "[templates]\nticket_body = \"new {{ input }}\"\n",
        Some("[templates]\nissue_body = \"local {{ input }}\"\n"),
    );
    assert_eq!(rendered(p.path()), ("x".into(), "local y".into()));
}
