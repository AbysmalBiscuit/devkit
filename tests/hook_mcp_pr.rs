//! The pre-tool-use hook gates a PR write through a forge's MCP tool on the
//! receipts `devkit pr render` leaves.

#[path = "common/ghfake.rs"]
mod ghfake;

use std::process::Output;

use serde_json::{Value, json};

/// The GitHub MCP server's PR tools, as a Claude Code cloud session exposes
/// them, plus a forge whose update edits the body in place.
const CONFIG: &str = r#"
[harness.pr_tools.github-create]
servers = ["*github*"]
tools   = ["create_pull_request"]
title   = "title"
body    = "body"

[harness.pr_tools.github-update]
servers = ["*github*"]
tools   = ["update_pull_request"]
absent  = ["pullNumber"]
title   = "title"
body    = "body"

[harness.pr_tools.patching]
servers    = ["*forge*"]
tools      = ["edit_pr"]
absent     = ["number"]
title      = "title"
body       = "description"
body_patch = ["patch"]

[templates]
pr_body = """
{{ input }}

## Proof
{{ proof }}
"""

[templates.variables]
proof = { default = "", required = "agents", description = "one line per Done when item" }
"#;

const CREATE: &str = "mcp__github__create_pull_request";
const UPDATE: &str = "mcp__github__update_pull_request";

fn project() -> ghfake::Fake {
    ghfake::Fake::without_pr(CONFIG)
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn rendered(out: &Output) -> (String, String) {
    assert!(out.status.success(), "{}", stderr(out));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    (
        v["title"].as_str().unwrap().to_string(),
        v["body"].as_str().unwrap().to_string(),
    )
}

/// `devkit pr render` as an agent in `session`.
fn pr_render(fake: &ghfake::Fake, session: &str, title: &str, body: &str) -> (String, String) {
    rendered(&fake.issue_in_session("agent", Some(session), &[
        "pr",
        "render",
        "--pr-title",
        title,
        "--pr-body",
        body,
        "--arg",
        "proof=1. test `a`",
    ]))
}

/// The denial's reason, or `None` for an allow (empty stdout).
fn pre_tool_use(fake: &ghfake::Fake, session: &str, tool: &str, input: Value) -> Option<String> {
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "session_id": session,
        "cwd": fake.project(),
        "tool_name": tool,
        "tool_input": input,
    });
    let out = fake.devkit_with_stdin(
        &["hook", "pre-tool-use", "--harness", "claude-code"],
        payload.to_string().as_bytes(),
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.trim().is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(stdout.trim()).expect("stdout is one JSON envelope");
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny", "{v}");
    Some(
        v["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

fn create(title: &str, body: &str) -> Value {
    json!({
        "owner": "o", "repo": "r", "head": "lev/eng-1-fix", "base": "main",
        "title": title, "body": body,
    })
}

#[test]
fn an_unrendered_pr_create_is_denied_naming_pr_render() {
    let fake = project();
    let reason = pre_tool_use(&fake, "S1", CREATE, create("T", "B")).expect("denied");
    assert!(
        reason.contains("devkit pr render --pr-title") && !reason.contains("issue pr"),
        "{reason}"
    );
    assert!(reason.contains("--arg proof=..."), "{reason}");
}

#[test]
fn a_rendered_pr_create_is_allowed() {
    let fake = project();
    let (t, b) = pr_render(&fake, "S1", "feat: a thing", "Why.");
    assert_eq!(pre_tool_use(&fake, "S1", CREATE, create(&t, &b)), None);
}

#[test]
fn an_issue_render_does_not_vouch_for_a_pr() {
    let fake = project();
    let (t, b) = rendered(&fake.issue_in_session("agent", Some("S1"), &[
        "render", "--title", "T", "--body", "B",
    ]));
    assert!(pre_tool_use(&fake, "S1", CREATE, create(&t, &b)).is_some());
}

#[test]
fn an_update_rewriting_the_body_needs_its_receipt() {
    let fake = project();
    let (_, b) = pr_render(&fake, "S1", "feat: a thing", "Why.");
    let update = |body: &str| json!({"owner": "o", "repo": "r", "pullNumber": 5, "body": body});

    let reason = pre_tool_use(&fake, "S1", UPDATE, update("hand-written")).expect("denied");
    assert!(reason.contains("differs"), "{reason}");
    assert_eq!(pre_tool_use(&fake, "S1", UPDATE, update(&b)), None);
}

#[test]
fn an_update_touching_neither_title_nor_body_is_allowed() {
    let fake = project();
    let input = json!({"owner": "o", "repo": "r", "pullNumber": 5, "state": "closed"});
    assert_eq!(pre_tool_use(&fake, "S1", UPDATE, input), None);
}

#[test]
fn a_body_patch_is_denied() {
    let fake = project();
    let input = json!({"number": 5, "patch": [{"op": "replace"}]});
    let reason = pre_tool_use(&fake, "S1", "mcp__forge__edit_pr", input).expect("denied");
    assert!(reason.contains("`patch`"), "{reason}");
    assert!(
        reason.contains("devkit pr render") && !reason.contains("issue pr"),
        "{reason}"
    );
}

#[test]
fn a_create_carrying_a_stray_pull_number_is_still_a_create() {
    let fake = project();
    let input = json!({"owner": "o", "repo": "r", "head": "h", "base": "main", "pullNumber": 1});
    let reason = pre_tool_use(&fake, "S1", CREATE, input).expect("denied");
    assert!(reason.contains("no `title`"), "{reason}");

    let mut input = create("T", "B");
    input["pullNumber"] = json!(1);
    assert!(pre_tool_use(&fake, "S1", CREATE, input).is_some());
}
