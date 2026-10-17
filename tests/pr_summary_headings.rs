//! `pr render`, `pr create` and `pr edit` refuse a summary that writes a
//! heading the `pr_body` template emits itself, on any of its branches: the
//! body would carry that heading twice, and an agent writing the human TL;DR
//! fills a section that is not its own.

#[path = "common/ghfake.rs"]
mod ghfake;

use std::process::Output;

/// The shape of a project's own `pr_body`: a TL;DR only humans get, a heading
/// glued to an expression, and a proof section behind its own branch.
const TEMPLATE: &str = r#"
[templates]
pr_body = '''
{%- set agent = agent_model -%}
{%- set tldr = human_tldr | trim or not agent -%}
{%- if tldr -%}
## TL;DR (human written)
{{ human_tldr }}
{%- endif -%}
{%- if agent -%}
{{ "\n\n---\n\n" if tldr else "" }}## Agent summary
{%- endif %}

{{ input | trim }}
{%- if proof | trim %}

## Proof

{{ proof | trim }}
{%- endif %}
'''

[templates.variables]
human_tldr = { default = "" }
proof = { default = "" }
agent_model = { default = "" }
"#;

const PR_7: ghfake::Pr = ghfake::Pr {
    number: 7,
    state: "OPEN",
    is_draft: false,
    author: "LevValle",
};

fn agent_args(summary: &str) -> Vec<String> {
    [
        "--pr-title",
        "add a thing",
        "--pr-body",
        summary,
        "--arg",
        "proof=1. test `a`",
        "--arg",
        "agent_model=Claude Opus 5.5",
    ]
    .map(String::from)
    .to_vec()
}

fn run(fake: &ghfake::Fake, verb: &[&str], summary: &str) -> Output {
    let args = agent_args(summary);
    let argv: Vec<&str> = verb
        .iter()
        .copied()
        .chain(args.iter().map(String::as_str))
        .collect();
    fake.issue_as("agent", &argv)
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn assert_refused(out: &Output, heading: &str) {
    let err = stderr(out);
    assert!(!out.status.success(), "{err}");
    assert!(out.stdout.is_empty(), "nothing rendered or opened");
    assert!(err.contains(&format!("`{heading}`")), "{err}");
    assert!(err.contains("pr_body"), "{err}");
}

const WITH_PROOF: &str = "Why it matters.\n\n## Proof\n\n1. test `a`";

#[test]
fn pr_render_refuses_a_summary_with_the_proof_heading() {
    let fake = ghfake::Fake::without_pr(TEMPLATE);
    assert_refused(&run(&fake, &["pr", "render"], WITH_PROOF), "## Proof");
}

#[test]
fn pr_create_refuses_a_summary_with_the_proof_heading() {
    let fake = ghfake::Fake::without_pr(TEMPLATE);
    fake.create_opens(&PR_7);
    let out = run(&fake, &["pr", "create", "--no-push"], WITH_PROOF);
    assert_refused(&out, "## Proof");
    assert!(!fake.calls().contains("pr create"), "{}", fake.calls());
}

#[test]
fn pr_edit_refuses_a_summary_with_the_proof_heading() {
    let fake = ghfake::Fake::new(TEMPLATE, &PR_7);
    fake.serve_pr_body(&PR_7, "## Agent summary\n\nOld.\n\n## Proof\n\n1. old");
    let out = fake.issue_as("agent", &[
        "pr",
        "edit",
        "--no-push",
        "--pr-body",
        WITH_PROOF,
    ]);
    assert_refused(&out, "## Proof");
    assert!(fake.edited_pr_arg("--body").is_none(), "{}", fake.calls());
    assert!(fake.patched_pr_field("body").is_none(), "{}", fake.calls());
}

#[test]
fn an_agent_summary_with_the_human_tldr_heading_is_refused() {
    let fake = ghfake::Fake::without_pr(TEMPLATE);
    let out = run(
        &fake,
        &["pr", "render"],
        "## TL;DR (human written)\nShips the thing.\n\nWhy it matters.",
    );
    assert_refused(&out, "## TL;DR (human written)");
}

#[test]
fn a_heading_the_template_does_not_emit_renders_unchanged() {
    let fake = ghfake::Fake::without_pr(TEMPLATE);
    let summary = "Why it matters.\n\n## Conceptual Changes\n\nThe store owns the claim.";
    let out = run(&fake, &["pr", "render"], summary);
    assert!(out.status.success(), "{}", stderr(&out));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let body = json["body"].as_str().unwrap();
    assert!(
        body.contains("## Agent summary\n\nWhy it matters.\n\n## Conceptual Changes\n\nThe store owns the claim.\n\n## Proof"),
        "{body}"
    );
}

#[test]
fn the_refused_headings_come_from_the_configured_template() {
    let fake = ghfake::Fake::without_pr(
        r#"
[templates]
pr_body = "{{ input }}\n\n## Rollout\n\n{{ rollout }}"

[templates.variables]
rollout = { default = "" }
proof = { default = "" }
agent_model = { default = "" }
"#,
    );

    let out = run(&fake, &["pr", "render"], "Why.\n\n## Rollout\n\nFlip it.");
    assert_refused(&out, "## Rollout");

    let out = run(&fake, &["pr", "render"], WITH_PROOF);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn a_heading_differing_only_in_case_and_level_is_refused() {
    let fake = ghfake::Fake::without_pr(TEMPLATE);
    let out = run(&fake, &["pr", "render"], "Why.\n\n# proof #\n\n1. test `a`");
    assert_refused(&out, "# proof #");
}
