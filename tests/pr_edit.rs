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
proof = { default = "", required = "agents" }
agent_model = { default = "" }
"#;

/// A body `pr create` rendered from [`TEMPLATE`], then edited by bots above
/// and below it. The bot's own `---` repeats the template's separator.
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

---

More from the bot.
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

/// Two equally good places for the Proof heading leave its section
/// ambiguous, and picking one would delete the text between them.
#[test]
fn a_template_line_repeated_in_the_body_is_refused_rather_than_guessed() {
    let live = LIVE.replace(
        "The old summary.",
        "The old summary, quoting a template:\n\n## Proof\n\nand more summary.",
    );
    let fake = project("", &live);

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "proof=1. the new proof"]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains("`proof`") && stderr.contains("## Proof"),
        "names the variable and the repeated line: {stderr}"
    );
    assert!(!wrote(&fake), "{}", fake.calls());
}

/// A blank value does not satisfy a variable the template requires of the
/// caller, on an edit as on `pr create`.
#[test]
fn an_agent_cannot_blank_a_variable_required_of_agents() {
    let fake = project("", LIVE);

    let out = fake.issue_as("agent", &["pr", "edit", "--no-push", "--arg", "proof="]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains("proof"), "names the variable: {stderr}");
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
    assert!(
        !fake.calls().contains("GET repos/o/r/pulls/7"),
        "the body is never read: {}",
        fake.calls()
    );
}

/// The Claude Code cloud proxy refuses GraphQL; a body edit with nothing to
/// upload reads and writes the PR over REST.
#[test]
fn a_body_edit_works_where_github_refuses_graphql() {
    let fake = project("", LIVE);
    fake.refuse_graphql();

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "proof=1. the new proof"]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert_eq!(
        fake.patched_pr_field("body").as_deref(),
        Some(
            LIVE.replace("1. the old proof", "1. the new proof")
                .as_str()
        )
    );
}

#[test]
fn a_merged_pr_is_refused() {
    let fake = ghfake::Fake::new(TEMPLATE, &ghfake::Pr {
        state: "MERGED",
        ..PR_7
    });

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "proof=1. the new proof"]);

    assert!(!out.status.success());
    assert!(!wrote(&fake), "{}", fake.calls());
}

/// A forge `pr edit` cannot write to is refused before the push, so the run
/// changes nothing it would then have to report.
#[test]
fn a_forge_without_pr_edits_refuses_before_the_push() {
    let origin = tempfile::tempdir().expect("origin dir");
    devkit_git::Git::fixture(origin.path())
        .args(["init", "-q", "--bare"])
        .output()
        .expect("git init --bare");
    let fake = ghfake::Fake::with_origin_and(
        &format!("{TEMPLATE}\n[forge]\nkind = \"gitlab\""),
        &origin.path().to_string_lossy(),
    );

    let out = fake.issue(&["pr", "edit", "--arg", "proof=1. the new proof"]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains("gitlab"), "names the forge: {stderr}");
    let refs = devkit_git::Git::fixture(origin.path())
        .args(["for-each-ref"])
        .output()
        .expect("git for-each-ref");
    assert_eq!(refs, "", "nothing is pushed");
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

/// The Caveats section of workcell's `pr_body` template: one loop renders
/// three variables, and the template rewrites `**None**`.
const CAVEATS_TEMPLATE: &str = r#"
[templates]
pr_body = '''
## Summary

{{ input | trim }}
{%- set caveats = [["departures", departures], ["loose ends", loose_ends], ["assumptions", assumptions]] %}
{%- set filled = [departures, loose_ends, assumptions] | map("trim") | reject("eq", "") | reject("eq", "**None**") | list %}
{%- if agent_model or filled %}

## Caveats
{%- for kind, value in caveats %}

{{ value | trim if value | trim not in ["", "**None**"] else "**0 " ~ kind ~ "**" }}
{%- endfor %}
{%- endif %}

## Proof

{{ proof | trim }}

---

Footer by {{ agent_model }}'''

[templates.variables]
departures = { default = "" }
loose_ends = { default = "" }
assumptions = { default = "" }
proof = { default = "" }
agent_model = { default = "" }
"#;

const CAVEATS_LIVE: &str = "\
## Summary

The summary.

## Caveats

**0 departures**

<details><summary><b>1 loose end</b></summary>

- the old loose end

</details>

**0 assumptions**

## Proof

1. the proof

---

Footer by Claude

## Review bot
Details the bot keeps current.
";

#[test]
fn an_edit_giving_every_variable_of_a_shared_section_rerenders_it() {
    let fake = ghfake::Fake::new(CAVEATS_TEMPLATE, &PR_7);
    fake.serve_pr_body(&PR_7, CAVEATS_LIVE);
    let assumption = "<details><summary><b>1 assumption</b></summary>\n\n- a choice\n\n</details>";

    let out = fake.issue(&[
        "pr",
        "edit",
        "--no-push",
        "--arg",
        "departures=**None**",
        "--arg",
        "loose_ends=**None**",
        "--arg",
        &format!("assumptions={assumption}"),
    ]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    let old = CAVEATS_LIVE
        .split_once("## Caveats\n")
        .and_then(|(_, rest)| rest.split_once("## Proof"))
        .map(|(section, _)| section)
        .unwrap();
    let new = format!("\n**0 departures**\n\n**0 loose ends**\n\n{assumption}\n\n");
    assert_eq!(sent_body(&fake), CAVEATS_LIVE.replace(old, &new));
}

#[test]
fn an_edit_giving_part_of_a_shared_section_names_what_it_needs() {
    let fake = ghfake::Fake::new(CAVEATS_TEMPLATE, &PR_7);
    fake.serve_pr_body(&PR_7, CAVEATS_LIVE);

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "assumptions=**None**"]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains("`departures`") && stderr.contains("`loose_ends`"),
        "names the variables the edit left out: {stderr}"
    );
    assert!(!wrote(&fake), "{}", fake.calls());
}
