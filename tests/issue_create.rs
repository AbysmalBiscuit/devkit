//! `devkit issue create` renders the issue templates and files the result
//! through `gh issue create`, on GitHub only.

#[path = "common/ghfake.rs"]
mod ghfake;

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

fn fake(kind: &str) -> ghfake::Fake {
    ghfake::Fake::without_pr(&format!("[tracker]\nkind = \"{kind}\"\n{TEMPLATES}"))
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_missing_required_arg_is_refused_before_gh_runs() {
    let fake = fake("github");
    let out = fake.issue(&["create", "--title", "T"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("acceptance"), "{}", stderr(&out));
    assert!(
        !fake.calls().contains("issue create"),
        "gh must not run: {}",
        fake.calls()
    );
}

#[test]
fn create_passes_the_rendered_text_to_gh() {
    let fake = fake("github");
    let out = fake.issue(&["create", "--title", "T", "--arg", "acceptance=A"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("https://github.com/o/r/issues/42"),
        "{stdout}"
    );
    let calls = fake.calls();
    assert!(calls.contains("issue create --title T --body"), "{calls}");
    assert!(calls.contains("## Acceptance criteria"), "{calls}");
}

#[test]
fn a_linear_tracker_is_pointed_at_render() {
    let fake = fake("linear");
    let out = fake.issue(&["create", "--title", "T", "--arg", "acceptance=A"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("devkit issue render"),
        "{}",
        stderr(&out)
    );
    assert!(!fake.calls().contains("issue create"), "{}", fake.calls());
}
