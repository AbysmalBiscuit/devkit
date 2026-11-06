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

/// With a token and no `gh` to run, the edit goes straight to GitHub's REST
/// API. The `PATH` holds `git` alone, so the fake `gh` is out of reach.
#[cfg(unix)]
#[test]
fn a_body_edit_goes_direct_with_a_token_and_no_gh() {
    use devkit_common::http::stub::{self, Route};

    let fake = project("", LIVE);
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
    let only_git = tempfile::tempdir().unwrap();
    let git = String::from_utf8(
        std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    std::os::unix::fs::symlink(git.trim(), only_git.path().join("git")).unwrap();

    let out = fake.issue_with_env(
        &["pr", "edit", "--no-push", "--arg", "proof=1. the new proof"],
        &[
            ("GH_TOKEN", "t0k"),
            ("DEVKIT_TEST_GITHUB_API", &api.url()),
            ("PATH", &only_git.path().to_string_lossy()),
        ],
    );

    let reqs = api.requests();
    assert!(out.status.success(), "{out:?}\n{reqs:#?}");
    assert_eq!(fake.calls(), "", "gh ran though it is not on PATH");
    let patch = reqs
        .iter()
        .find(|r| r.method == "PATCH" && r.path == "/repos/o/r/pulls/7")
        .unwrap_or_else(|| panic!("no direct edit: {reqs:#?}"));
    assert_eq!(patch.header("Authorization"), Some("Bearer t0k"));
    let body: serde_json::Value = serde_json::from_str(&patch.body).unwrap();
    assert_eq!(
        body["body"].as_str(),
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

const LABELLED_TEMPLATE: &str = r#"
[templates]
pr_body = """
## Merge danger

**Door:** {{ door | trim }}

**Blast Radius:** {{ blast_radius | trim }}

---

Footer"""

[templates.variables]
door = { default = "" }
blast_radius = { default = "" }
"#;

const LABELLED_LIVE: &str = "\
## Merge danger

**Door:** one-way

**Blast Radius:** only the old line
- and an old bullet

---

Footer

## Review bot
Details the bot keeps current.
";

#[test]
fn a_variable_after_a_label_is_replaced_and_keeps_the_label() {
    let fake = ghfake::Fake::new(LABELLED_TEMPLATE, &PR_7);
    fake.serve_pr_body(&PR_7, LABELLED_LIVE);

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "door=two-way"]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert_eq!(
        sent_body(&fake),
        LABELLED_LIVE.replace("**Door:** one-way", "**Door:** two-way")
    );
}

#[test]
fn a_multi_line_value_after_a_label_replaces_the_old_lines() {
    let fake = ghfake::Fake::new(LABELLED_TEMPLATE, &PR_7);
    fake.serve_pr_body(&PR_7, LABELLED_LIVE);

    let out = fake.issue(&[
        "pr",
        "edit",
        "--no-push",
        "--arg",
        "blast_radius=the new line\n- first bullet\n- second bullet",
    ]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert_eq!(
        sent_body(&fake),
        LABELLED_LIVE.replace(
            "only the old line\n- and an old bullet",
            "the new line\n- first bullet\n- second bullet"
        )
    );
}

#[test]
fn a_variable_sharing_its_line_with_another_is_refused_by_name() {
    let fake = ghfake::Fake::new(
        r#"
[templates]
pr_body = """
## Merge danger

**Door:** {{ door }} **Owner:** {{ owner }}

---
"""

[templates.variables]
door = { default = "" }
owner = { default = "" }
"#,
        &PR_7,
    );
    fake.serve_pr_body(
        &PR_7,
        "## Merge danger\n\n**Door:** a **Owner:** b\n\n---\n",
    );

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "door=two-way"]);

    assert!(!out.status.success(), "{}", fake.calls());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("shares its line"), "{stderr}");
    assert!(stderr.contains("`owner`"), "{stderr}");
    assert!(!wrote(&fake), "{}", fake.calls());
}

/// The body lacks both label lines, and the edit passes only the second, so
/// the first stays out rather than landing as an unresolved slot.
#[test]
fn inserting_a_label_section_stops_at_a_variable_the_edit_did_not_pass() {
    let live = "## Merge danger\n\n---\n\nFooter\n";
    let fake = ghfake::Fake::new(LABELLED_TEMPLATE, &PR_7);
    fake.serve_pr_body(&PR_7, live);

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "blast_radius=wide"]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert_eq!(
        sent_body(&fake),
        "## Merge danger\n\n**Blast Radius:** wide\n\n---\n\nFooter\n"
    );
}

const SUFFIX_TEMPLATE: &str = r#"
[templates]
pr_body = """
## Merge danger

Door {{ door | trim }} (checked)

---

Footer"""

[templates.variables]
door = { default = "" }
"#;

#[test]
fn a_multi_line_value_before_literal_text_is_replaced_whole() {
    let live = "## Merge danger\n\nDoor old\nold too (checked)\n\n---\n\nFooter\n";
    let fake = ghfake::Fake::new(SUFFIX_TEMPLATE, &PR_7);
    fake.serve_pr_body(&PR_7, live);

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "door=new\nnew too"]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert_eq!(
        sent_body(&fake),
        "## Merge danger\n\nDoor new\nnew too (checked)\n\n---\n\nFooter\n"
    );
}

#[test]
fn a_multi_line_value_before_literal_text_is_inserted_when_the_body_lacks_it() {
    let live = "## Merge danger\n\n---\n\nFooter\n";
    let fake = ghfake::Fake::new(SUFFIX_TEMPLATE, &PR_7);
    fake.serve_pr_body(&PR_7, live);

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "door=new\nnew too"]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert_eq!(
        sent_body(&fake),
        "## Merge danger\n\nDoor new\nnew too (checked)\n\n---\n\nFooter\n"
    );
}

/// With no text before the value, the old value's first line cannot be
/// recognised, so the span opens at the template line above it.
#[test]
fn a_multi_line_value_with_only_literal_text_after_it_is_replaced_whole() {
    let live = "## Merge danger\n\nold\nold too (checked)\n\n---\n\nFooter\n";
    let fake = ghfake::Fake::new(
        &SUFFIX_TEMPLATE.replace("Door {{ door | trim }}", "{{ door | trim }}"),
        &PR_7,
    );
    fake.serve_pr_body(&PR_7, live);

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "door=new\nnew too"]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert_eq!(
        sent_body(&fake),
        "## Merge danger\n\nnew\nnew too (checked)\n\n---\n\nFooter\n"
    );
}

const LABEL_THEN_OWN_LINE_TEMPLATE: &str = r#"
[templates]
pr_body = """
## Merge danger

**Door:** {{ door | trim }}
{{ extra | trim }}

---

Footer"""

[templates.variables]
door = { default = "" }
extra = { default = "" }
"#;

const LABEL_THEN_OWN_LINE_LIVE: &str =
    "## Merge danger\n\n**Door:** old\n- more\nold extra\n\n---\n\nFooter\n";

/// The door's continuation lines sit in the span `extra` renders into, so
/// replacing `extra` alone would delete them.
#[test]
fn a_variable_below_a_label_line_needs_the_labels_variable() {
    let fake = ghfake::Fake::new(LABEL_THEN_OWN_LINE_TEMPLATE, &PR_7);
    fake.serve_pr_body(&PR_7, LABEL_THEN_OWN_LINE_LIVE);

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "extra=new extra"]);

    assert!(!out.status.success(), "{}", fake.calls());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("also pass `door`"), "{stderr}");
    assert!(!wrote(&fake), "{}", fake.calls());
}

#[test]
fn a_label_line_and_the_variable_below_it_are_replaced_together() {
    let fake = ghfake::Fake::new(LABEL_THEN_OWN_LINE_TEMPLATE, &PR_7);
    fake.serve_pr_body(&PR_7, LABEL_THEN_OWN_LINE_LIVE);

    let out = fake.issue(&[
        "pr",
        "edit",
        "--no-push",
        "--arg",
        "door=new\n- bullet",
        "--arg",
        "extra=new extra",
    ]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert_eq!(
        sent_body(&fake),
        "## Merge danger\n\n**Door:** new\n- bullet\nnew extra\n\n---\n\nFooter\n"
    );
}

/// The title cannot be passed to an edit and never spans lines, so a line
/// holding it stays an ordinary bound.
#[test]
fn a_variable_below_the_title_line_is_replaced_alone() {
    let fake = ghfake::Fake::new(
        r#"
[templates]
pr_body = """
Title: {{ pr_title }}

{{ extra | trim }}

---
"""

[templates.variables]
extra = { default = "" }
"#,
        &PR_7,
    );
    fake.serve_pr_body(&PR_7, "Title: A title\n\nold\n\n---\n");

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "extra=new"]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert_eq!(sent_body(&fake), "Title: A title\n\nnew\n\n---\n");
}

/// A section of `upper` and, below it, `lower` on a line with text only after
/// its slot. That text sits on the value's last row, so it cannot bound
/// `upper`.
fn text_after_slot_project(upper: &str, lower: &str) -> ghfake::Fake {
    let fake = ghfake::Fake::new(
        &format!(
            r#"
[templates]
pr_body = """
## Merge danger

{{{{ {upper} | trim }}}}

{{{{ {lower} | trim }}}} (checked)

---

Footer"""

[templates.variables]
{upper} = {{ default = "" }}
{lower} = {{ default = "" }}
"#
        ),
        &PR_7,
    );
    fake.serve_pr_body(
        &PR_7,
        "## Merge danger\n\nold upper\n\nold lower\nmore lower (checked)\n\n---\n\nFooter\n",
    );
    fake
}

#[test]
fn a_variable_above_a_line_with_text_only_after_its_slot_needs_that_variable() {
    let fake = text_after_slot_project("upper", "lower");

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "upper=new upper"]);

    assert!(!out.status.success(), "{}", fake.calls());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("also pass `lower`"), "{stderr}");
    assert!(!wrote(&fake), "{}", fake.calls());
}

/// Sections are placed in the order their variables sort, which must not
/// change what the edit replaces.
#[test]
fn a_line_with_text_only_after_its_slot_is_replaced_with_the_variable_above_it() {
    for (upper, lower) in [("a", "z"), ("z", "a")] {
        let fake = text_after_slot_project(upper, lower);

        let out = fake.issue(&[
            "pr",
            "edit",
            "--no-push",
            "--arg",
            &format!("{upper}=new upper"),
            "--arg",
            &format!("{lower}=new lower\nnew more"),
        ]);

        assert!(out.status.success(), "{out:?}\n{}", fake.calls());
        assert_eq!(
            sent_body(&fake),
            "## Merge danger\n\nnew upper\n\nnew lower\nnew more (checked)\n\n---\n\nFooter\n"
        );
    }
}

/// `enabled` hides `detail`, and the body lacks its heading, so the edit would
/// replace from the door's line. That would delete the door's continuation.
#[test]
fn a_part_shown_section_refuses_to_replace_from_a_line_holding_a_value() {
    let fake = ghfake::Fake::new(
        r#"
[templates]
pr_body = '''
## Summary

**Door:** {{ door | trim }}
{%- if enabled %}

### Optional

{{ detail | trim }}
{%- endif %}

{{ tail | trim }}

## Proof
'''

[templates.variables]
enabled = { default = "" }
door = { default = "" }
detail = { default = "" }
tail = { default = "" }
"#,
        &PR_7,
    );
    fake.serve_pr_body(
        &PR_7,
        "## Summary\n\n**Door:** old\n- door continuation\n\nold tail\n\n## Proof\n",
    );

    let out = fake.issue(&[
        "pr",
        "edit",
        "--no-push",
        "--arg",
        "detail=new detail",
        "--arg",
        "tail=new tail",
    ]);

    assert!(!out.status.success(), "{}", fake.calls());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("`door`"), "{stderr}");
    assert!(!wrote(&fake), "{}", fake.calls());
}

/// The title cannot be passed, so the line cannot be rewritten, and `who` can
/// run on into the section below it.
#[test]
fn a_variable_below_a_line_with_the_title_and_a_value_is_refused_by_name() {
    let fake = ghfake::Fake::new(
        r#"
[templates]
pr_body = """
Title: {{ pr_title }} by {{ who | trim }}
{{ extra | trim }}

---
"""

[templates.variables]
who = { default = "" }
extra = { default = "" }
"#,
        &PR_7,
    );
    fake.serve_pr_body(&PR_7, "Title: T by me\n- who continuation\nold\n\n---\n");

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "extra=new"]);

    assert!(!out.status.success(), "{}", fake.calls());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("`who`") && stderr.contains("title"),
        "{stderr}"
    );
    assert!(!wrote(&fake), "{}", fake.calls());
}

/// An edit has no title to render, so the insert starts below the title line.
#[test]
fn inserting_below_a_missing_title_line_leaves_the_title_out() {
    let fake = ghfake::Fake::new(
        r#"
[templates]
pr_body = """
Title: {{ pr_title }}

{{ extra | trim }}

---
"""

[templates.variables]
extra = { default = "" }
"#,
        &PR_7,
    );
    fake.serve_pr_body(&PR_7, "A bot's banner.\n\n---\n");

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "extra=new"]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert_eq!(sent_body(&fake), "A bot's banner.\n\nnew\n\n---\n");
}

/// `door` and `owner` share a line, so the section below it, which that line
/// opens, cannot be replaced, whichever of the edit's variables sorts first.
#[test]
fn a_section_opened_by_a_line_of_two_variables_is_refused_in_any_order() {
    for extra in ["extra", "aextra"] {
        let fake = ghfake::Fake::new(
            &format!(
                r#"
[templates]
pr_body = """
## H

**Door:** {{{{ door }}}} **Owner:** {{{{ owner }}}}
{{{{ {extra} | trim }}}}

---
"""

[templates.variables]
door = {{ default = "" }}
owner = {{ default = "" }}
{extra} = {{ default = "" }}
"#
            ),
            &PR_7,
        );
        fake.serve_pr_body(&PR_7, "## H\n\n**Door:** a **Owner:** b\nold\n\n---\n");

        let out = fake.issue(&[
            "pr",
            "edit",
            "--no-push",
            "--arg",
            "door=x",
            "--arg",
            "owner=y",
            "--arg",
            &format!("{extra}=new"),
        ]);

        assert!(!out.status.success(), "{extra}: {}", fake.calls());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            ["`door`", "`owner`", "share"]
                .iter()
                .all(|w| stderr.contains(w)),
            "{extra}: {stderr}"
        );
        assert!(!wrote(&fake), "{extra}: {}", fake.calls());
    }
}

/// An edit cannot pass the title, so a section that renders it is refused by
/// name instead of asking for an argument that cannot be given.
#[test]
fn a_section_that_renders_the_title_is_refused_naming_it() {
    let fake = ghfake::Fake::new(
        r#"
[templates]
pr_body = """
**Door:** {{ door | trim }}
{{ pr_title }}

---
"""

[templates.variables]
door = { default = "" }
"#,
        &PR_7,
    );
    fake.serve_pr_body(&PR_7, "**Door:** old\nA title\n\n---\n");

    let out = fake.issue(&["pr", "edit", "--no-push", "--arg", "door=new"]);

    assert!(!out.status.success(), "{}", fake.calls());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("title"), "{stderr}");
    assert!(!stderr.contains("also pass"), "{stderr}");
    assert!(!wrote(&fake), "{}", fake.calls());
}
