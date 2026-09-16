//! `issue pr create` renders a template only for the run that sends it.
//!
//! Templating is strict about undefined variables, and `{{ issue }}` is bound
//! only in a worktree `issue setup` recorded. So a template that reads it is a
//! live failure for every run that renders one it has no use for.

#[path = "common/ghfake.rs"]
mod ghfake;

/// A `pr_body` that cannot render outside an `issue setup` worktree.
const BODY_NEEDS_THE_RECORD: &str = r#"
[templates]
pr_body = "Closes {{ issue }}"
"#;

/// A `pr_title` with the same problem, so a run that renders one before
/// deciding it needs one fails on the title instead.
const TITLE_NEEDS_THE_RECORD: &str = r#"
[templates]
pr_title = "{{ issue }}: {{ input }}"
"#;

#[test]
fn reusing_a_pr_renders_neither_template() {
    for templates in [BODY_NEEDS_THE_RECORD, TITLE_NEEDS_THE_RECORD] {
        let fake = ghfake::Fake::new(templates, &ghfake::Pr {
            number: 7,
            state: "OPEN",
            is_draft: true,
            author: "LevValle",
        });
        let out = fake.issue(&["pr", "create", "--no-push"]);

        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            out.status.success(),
            "reuse must not render a template it never sends: {stderr}"
        );
        assert!(
            !fake.calls().contains("pr create"),
            "the PR already exists: {}",
            fake.calls()
        );
    }
}

/// A run the reviewer gate refuses must say so. Rendering first would answer a
/// policy refusal with a template error, naming a problem the user does not
/// have and hiding the one they do.
#[test]
fn the_reviewer_gate_refuses_before_a_template_can_fail() {
    let fake = ghfake::Fake::without_pr(&format!(
        "require_pr_reviewer = true\n{BODY_NEEDS_THE_RECORD}"
    ));
    let out = fake.issue(&["pr", "create", "--ready", "--no-push"]);

    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "the gate must refuse: {stderr}");
    assert!(
        stderr.contains("no human reviewer"),
        "the refusal must name the gate, not the template: {stderr}"
    );
    assert!(
        !fake.calls().contains("pr create"),
        "nothing is opened: {}",
        fake.calls()
    );
}

/// A project whose `pr_title` and `pr_body` are `templates`, with `ticket`
/// defaulted but required of agents, reusing an open PR.
fn fake_with(templates: &str) -> ghfake::Fake {
    ghfake::Fake::new(
        &format!(
            "{templates}\n[templates.variables]\nticket = {{ default = \"NONE\", required = \"agents\" }}\n"
        ),
        &ghfake::Pr {
            number: 7,
            state: "OPEN",
            is_draft: true,
            author: "LevValle",
        },
    )
}

#[test]
fn issue_pr_refuses_a_required_arg_the_agent_did_not_pass() {
    let fake = fake_with("[templates]\npr_title = \"{{ ticket }}: {{ input }}\"");
    let out = fake.issue(&["pr", "create", "--no-push", "--pr-title", "add a thing"]);
    assert!(!out.status.success(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--arg ticket=..."), "{stderr}");
    assert!(stderr.contains("required for agents"), "{stderr}");
}

#[test]
fn a_name_only_another_surfaces_template_reads_does_not_bind() {
    // `pr_title`/`pr_body` default to `{{ input }}` and read no `ticket`,
    // so the marking is irrelevant to this command.
    let fake = fake_with("[templates]\nreview_finish = \"{{ ticket }} {{ pr_url }}\"");
    let out = fake.issue(&["pr", "create", "--no-push", "--pr-title", "add a thing"]);
    assert!(out.status.success(), "{out:?}");
}
