//! `pr edit` rewrites the parts of an open PR's body that the run passes,
//! placing each where the `pr_body` template puts it and leaving every other
//! byte of the live body, a bot's additions included, as it was.

#[path = "common/ghfake.rs"]
mod ghfake;

const PR_7: ghfake::Pr = ghfake::Pr {
    number: 7,
    state: "OPEN",
    is_draft: false,
    author: "LevValle",
};

const TEMPLATE: &str = r#"
[templates]
pr_body = """
## Summary

{{ input | trim }}
{%- if proof | trim %}

## Proof

{{ proof | trim }}
{%- endif %}

---

Footer by {{ agent_model }}"""

[templates.variables]
proof = { default = "" }
agent_model = { default = "" }
"#;

/// A body `pr create` rendered from [`TEMPLATE`], then edited by bots above
/// and below it.
const LIVE: &str = "\
> [!NOTE]
> A bot's banner.

## Summary

The old summary.

## Proof

1. the old proof

---

Footer by Claude

## Review bot
Details the bot keeps current.
";

/// A project whose branch has PR 7 open, carrying `body`.
fn project(extra: &str, body: &str) -> ghfake::Fake {
    let fake = ghfake::Fake::new(&format!("{extra}\n{TEMPLATE}"), &PR_7);
    fake.serve_pr_body(&PR_7, body);
    fake
}

/// The body the run sent, through whichever write it made.
fn sent_body(fake: &ghfake::Fake) -> String {
    fake.edited_pr_arg("--body")
        .or_else(|| fake.patched_pr_field("body"))
        .unwrap_or_else(|| panic!("no body was written: {}", fake.calls()))
}

fn wrote(fake: &ghfake::Fake) -> bool {
    fake.edited_pr_arg("--body").is_some() || fake.patched_pr_field("body").is_some()
}

#[test]
fn a_proof_edit_replaces_only_the_proof_section() {
    let fake = project("", LIVE);

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "proof=1. the new proof"]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert_eq!(
        sent_body(&fake),
        LIVE.replace("1. the old proof", "1. the new proof")
    );
}

#[test]
fn a_summary_edit_keeps_the_proof() {
    let fake = project("", LIVE);

    let out = fake.issue(&["pr", "edit", "--no-push", "--pr-body", "A new summary."]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert_eq!(
        sent_body(&fake),
        LIVE.replace("The old summary.", "A new summary.")
    );
}

#[test]
fn an_attachment_lands_under_the_proof_line_it_proves() {
    let fake = project("", LIVE);
    std::fs::write(fake.project().join("after.png"), "png").unwrap();
    let proof = "1. the button shows a spinner\n   - **After:**\n\n![after](./after.png)";

    let out = fake.issue(&[
        "pr",
        "edit",
        "--no-push",
        "--arg",
        &format!("proof={proof}"),
        "--attach",
        "./after.png#The spinner",
    ]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert_eq!(
        fake.edited_pr_arg("--body").as_deref(),
        Some(LIVE.replace("1. the old proof", proof).as_str()),
        "{}",
        fake.calls()
    );
    assert_eq!(
        fake.edited_pr_arg("--attach").as_deref(),
        Some("./after.png#The spinner")
    );
}

/// gh appends a file the body does not reference to the end of the body,
/// below every section and any bot's additions.
#[test]
fn an_attachment_the_edit_does_not_reference_is_refused() {
    let fake = project("", LIVE);
    std::fs::write(fake.project().join("after.png"), "png").unwrap();

    let out = fake.issue(&[
        "pr",
        "edit",
        "--no-push",
        "--arg",
        "proof=1. see the screenshot",
        "--attach",
        "after.png",
    ]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains("after.png") && stderr.contains("](after.png)"),
        "names the file and how to reference it: {stderr}"
    );
    assert!(!wrote(&fake), "{}", fake.calls());
}

#[test]
fn a_missing_proof_section_is_inserted_where_the_template_puts_it() {
    let without = LIVE.replace("## Proof\n\n1. the old proof\n\n", "");
    let fake = project("", &without);

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "proof=1. the new proof"]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert_eq!(
        sent_body(&fake),
        LIVE.replace("1. the old proof", "1. the new proof")
    );
}

#[test]
fn a_body_the_template_no_longer_describes_is_refused() {
    let fake = project("", "Someone rewrote the whole description.\n");

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "proof=1. the new proof"]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains("`proof`") && stderr.contains("#7"),
        "names the variable and the PR: {stderr}"
    );
    assert!(!wrote(&fake), "{}", fake.calls());
}

#[test]
fn a_new_title_alone_leaves_the_body_unread() {
    let fake = project("", LIVE);

    let out = fake.issue(&["pr", "edit", "--no-push", "--pr-title", "feat: a new title"]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert_eq!(
        fake.patched_pr_field("title").as_deref(),
        Some("feat: a new title")
    );
    assert_eq!(fake.patched_pr_field("body"), None);
}

#[test]
fn an_agents_proof_that_skips_an_item_is_refused() {
    let fake = project(
        "pr_proof_variable = \"proof\"\n\n[tracker]\nkind = \"github\"\n",
        LIVE,
    );
    fake.record_issue("7");
    fake.serve_issue("## Done when\n\n- [ ] One\n- [ ] Two\n");

    let out = fake.issue_as("agent", &[
        "pr",
        "edit",
        "--no-push",
        "--arg",
        "proof=1. the first",
    ]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains("2. Two"), "names the gap: {stderr}");
    assert!(!wrote(&fake), "{}", fake.calls());
}

#[test]
fn nothing_to_change_is_refused() {
    let fake = project("", LIVE);

    let out = fake.issue(&["pr", "edit", "--no-push"]);

    assert!(!out.status.success());
    assert!(!wrote(&fake), "{}", fake.calls());
}
