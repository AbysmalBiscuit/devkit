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

/// With a token of its own, devkit reads the PR over GitHub's REST API
/// directly, but writes the edit through `gh api`, so a `gh` wrapper on `PATH`
/// sees every body devkit writes.
#[test]
fn a_body_edit_goes_through_gh_api_with_a_token() {
    use devkit_common::http::stub::{self, Route};

    let fake = project("", LIVE);
    fake.refuse_graphql();
    let pr = serde_json::json!({
        "number": 7, "state": "open", "merged_at": null,
        "html_url": "https://github.com/o/r/pull/7", "title": "fix it", "body": LIVE,
        "head": { "ref": "lev/eng-1-fix", "sha": fake.head(), "repo": { "owner": { "login": "o" } } },
        "draft": false, "user": { "login": "LevValle" }
    });
    let api = stub::serve(vec![
        Route::new(
            "POST",
            "/graphql",
            403,
            r#"{"message":"GitHub GraphQL is not available from Claude Code sessions"}"#,
        ),
        Route::new("GET", "/repos/o/r/pulls?", 200, &format!("[{pr}]")),
        Route::new("GET", "/repos/o/r/pulls/7", 200, &pr.to_string()),
        Route::new("PATCH", "/repos/o/r/pulls/7", 200, &pr.to_string()),
    ]);

    let out = fake.issue_with_env(
        &["pr", "edit", "--no-push", "--arg", "proof=1. the new proof"],
        &[("GH_TOKEN", "t0k"), ("DEVKIT_TEST_GITHUB_API", &api.url())],
    );

    let reqs = api.requests();
    let calls = fake.calls();
    assert!(out.status.success(), "{out:?}\n{reqs:#?}\n{calls}");
    assert_eq!(
        fake.patched_pr_field("body").as_deref(),
        Some(
            LIVE.replace("1. the old proof", "1. the new proof")
                .as_str()
        ),
        "the edit did not go through gh: {calls}"
    );
    assert!(
        !reqs.iter().any(|r| r.method == "PATCH"),
        "the edit went around gh: {reqs:#?}"
    );
    assert!(
        reqs.iter()
            .any(|r| r.method == "GET" && r.path == "/repos/o/r/pulls/7"),
        "the body was not read directly: {reqs:#?}"
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

/// `CAVEATS_LIVE` as `pr create` rendered it with no agent and no caveats.
fn without_caveats() -> String {
    let (head, rest) = CAVEATS_LIVE.split_once("## Caveats\n").unwrap();
    let (_, tail) = rest.split_once("## Proof").unwrap();
    format!("{head}## Proof{tail}")
}

const NO_CAVEATS: [&str; 6] = [
    "--arg",
    "departures=**None**",
    "--arg",
    "loose_ends=**None**",
    "--arg",
    "assumptions=**None**",
];

/// The template hides Caveats when no agent made the PR and every caveat
/// is `**None**`, as `pr create` did.
#[test]
fn a_section_the_values_hide_stays_out_of_a_body_that_lacks_it() {
    let live = without_caveats();
    let fake = ghfake::Fake::new(CAVEATS_TEMPLATE, &PR_7);
    fake.serve_pr_body(&PR_7, &live);

    let out = fake.issue(&[&["pr", "edit", "--no-push"][..], &NO_CAVEATS].concat());

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert_eq!(sent_body(&fake), live);
}

/// An agent's edit leaves out `agent_model`, which the open PR already
/// shows, so the section the body has is still replaced.
#[test]
fn a_section_the_body_has_is_replaced_even_when_the_edit_leaves_its_condition_out() {
    let fake = ghfake::Fake::new(CAVEATS_TEMPLATE, &PR_7);
    fake.serve_pr_body(&PR_7, CAVEATS_LIVE);

    let out = fake.issue(&[&["pr", "edit", "--no-push"][..], &NO_CAVEATS].concat());

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert_eq!(
        sent_body(&fake),
        without_caveats().replace(
            "## Proof",
            "## Caveats\n\n**0 departures**\n\n**0 loose ends**\n\n**0 assumptions**\n\n## Proof"
        )
    );
}

const GATED_TEMPLATE: &str = r#"
[templates]
pr_body = '''
## Summary

{{ input | trim }}
{%- if enabled %}

## Optional

{{ detail | trim }}
{%- endif %}

## Proof

{{ proof | trim }}'''

[templates.variables]
enabled = { default = "" }
detail = { default = "" }
proof = { default = "" }
"#;

const GATED_LIVE: &str = "\
## Summary

The summary.

## Proof

1. the proof
";

/// `enabled` hides the section that `detail` renders into, and the edit
/// does not pass it, so the body keeps lacking that section.
#[test]
fn a_section_another_variable_hides_stays_out_of_a_body_that_lacks_it() {
    let fake = ghfake::Fake::new(GATED_TEMPLATE, &PR_7);
    fake.serve_pr_body(&PR_7, GATED_LIVE);

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "detail=New detail"]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert_eq!(sent_body(&fake), GATED_LIVE);
}

/// The same edit under an `enabled` that shows the section inserts it.
#[test]
fn a_section_another_variable_shows_is_inserted_into_a_body_that_lacks_it() {
    let shown = GATED_TEMPLATE.replace(
        r#"enabled = { default = "" }"#,
        r#"enabled = { default = "yes" }"#,
    );
    let fake = ghfake::Fake::new(&shown, &PR_7);
    fake.serve_pr_body(&PR_7, GATED_LIVE);

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "detail=New detail"]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert_eq!(
        sent_body(&fake),
        GATED_LIVE.replace("## Proof", "## Optional\n\nNew detail\n\n## Proof")
    );
}

const PART_GATED_TEMPLATE: &str = r#"
[templates]
pr_body = '''
## Summary

{{ input | trim }}

## Details
{%- if enabled %}

### Optional

{{ detail | trim }}
{%- endif %}

{{ tail | trim }}

## Proof

{{ proof | trim }}'''

[templates.variables]
enabled = { default = "" }
detail = { default = "" }
tail = { default = "" }
proof = { default = "" }
"#;

const PART_GATED_LIVE: &str = "\
## Summary

The summary.

## Details

Old tail

## Proof

1. the proof
";

/// `enabled` hides `detail` but not `tail`, which shares its section, so the
/// edit replaces `tail` and keeps the hidden part out.
#[test]
fn a_section_another_variable_hides_in_part_keeps_the_visible_edit() {
    let fake = ghfake::Fake::new(PART_GATED_TEMPLATE, &PR_7);
    fake.serve_pr_body(&PR_7, PART_GATED_LIVE);

    let out = fake.issue(&[
        "pr",
        "edit",
        "--no-push",
        "--arg",
        "detail=New detail",
        "--arg",
        "tail=New tail",
    ]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert_eq!(
        sent_body(&fake),
        PART_GATED_LIVE.replace("Old tail", "New tail")
    );
}
