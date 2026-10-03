//! The todo store's side of `devkit hook`: release, claim attribution and
//! native tool capture, each driven with real payloads on stdin.

#[path = "common/todoenv.rs"]
mod todoenv;

use devkit_todo::{Edit, Holder, NewTodo, Status, StatusKind, TodoStore};
use serde_json::json;
use todoenv::{Proj, stdout};

fn seed(p: &Proj, text: &str) -> String {
    p.store()
        .add(NewTodo {
            project: Some("proj.main.claude-S".into()),
            description: text.into(),
            parent: None,
            order: None,
        })
        .unwrap()
}

fn claim(p: &Proj, id: &str, by: &str) {
    p.store()
        .apply(&Edit::SetStatus {
            id: id.into(),
            to: StatusKind::InProgress,
            actor: Holder::new(by),
        })
        .unwrap();
}

fn in_progress(by: &str) -> Status {
    Status::InProgress {
        by: Holder::new(by),
    }
}

#[test]
fn release_subagent_stop_returns_its_claims_to_pending() {
    let p = Proj::new();
    let (a, b) = (seed(&p, "a"), seed(&p, "b"));
    claim(&p, &a, "S/a1");
    claim(&p, &b, "S");
    let out = p.hook(
        "subagent-stop",
        "claude-code",
        &json!({"session_id": "S", "agent_id": "a1", "agent_type": "general-purpose"}),
    );
    assert!(out.status.success());
    assert_eq!(stdout(&out), "");
    assert_eq!(p.todo(&a).status, Status::Pending);
    assert_eq!(p.todo(&b).status, in_progress("S"));
}

#[test]
fn release_session_end_releases_the_session_and_its_sub_agents() {
    let p = Proj::new();
    let (a, b) = (seed(&p, "a"), seed(&p, "b"));
    claim(&p, &a, "S/a1");
    claim(&p, &b, "S");
    let out = p.hook("session-end", "claude-code", &json!({"session_id": "S"}));
    assert_eq!(stdout(&out), "");
    assert_eq!(p.todo(&a).status, Status::Pending);
    assert_eq!(p.todo(&b).status, Status::Pending);
}

#[test]
fn release_a_fork_releases_nothing() {
    let p = Proj::new();
    let a = seed(&p, "a");
    claim(&p, &a, "S");
    p.hook(
        "subagent-stop",
        "claude-code",
        &json!({"session_id": "S", "agent_id": "afork"}),
    );
    assert_eq!(p.todo(&a).status, in_progress("S"));
}

fn bash(p: &Proj, agent: Option<&str>, command: &str) -> serde_json::Value {
    let mut payload = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "S",
        "tool_name": "Bash",
        "tool_input": {"command": command},
        "cwd": p.path,
    });
    if let Some(agent) = agent {
        payload["agent_id"] = json!(agent);
        payload["agent_type"] = json!("general-purpose");
    }
    payload
}

fn denial(out: &std::process::Output) -> Option<String> {
    let s = stdout(out);
    (!s.trim().is_empty()).then(|| {
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        v["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .to_string()
    })
}

#[test]
fn attribution_a_sub_agents_start_is_recorded_as_the_sub_agent() {
    let p = Proj::new();
    let id = seed(&p, "a");
    let out = p.hook(
        "pre-tool-use",
        "claude-code",
        &bash(&p, Some("a1"), &format!("devkit todo start {id}")),
    );
    assert_eq!(stdout(&out), "");
    assert_eq!(p.todo(&id).status, in_progress("S/a1"));
}

#[test]
fn attribution_a_siblings_start_is_denied_naming_the_holder() {
    let p = Proj::new();
    let id = seed(&p, "a");
    let start = format!("devkit todo start {id}");
    p.hook("pre-tool-use", "claude-code", &bash(&p, Some("a1"), &start));
    let out = p.hook("pre-tool-use", "claude-code", &bash(&p, Some("a2"), &start));
    let reason = denial(&out).expect("denied");
    assert!(reason.contains("in progress by S/a1"), "{reason}");
    assert_eq!(p.todo(&id).status, in_progress("S/a1"));
}

#[test]
fn attribution_a_command_the_write_gate_denies_leaves_no_claim() {
    let p = Proj::new();
    std::fs::write(
        p.path.join("devkit.toml"),
        "[harness]\nenforce_writes = true\n",
    )
    .unwrap();
    let id = seed(&p, "a");
    let held = p.devkit(&["locks", "acquire", "--as", "other", "f"], &[]);
    assert!(held.status.success(), "{}", todoenv::stderr(&held));
    let out = p.hook(
        "pre-tool-use",
        "claude-code",
        &bash(
            &p,
            Some("a1"),
            &format!("echo x > f && devkit todo start {id}"),
        ),
    );
    assert!(denial(&out).is_some_and(|r| r.contains("other")));
    assert_eq!(p.todo(&id).status, Status::Pending);
}

#[test]
fn attribution_the_parent_command_run_after_it_keeps_the_sub_agent() {
    let p = Proj::new();
    let id = seed(&p, "a");
    let start = format!("devkit todo start {id}");
    p.hook("pre-tool-use", "claude-code", &bash(&p, Some("a1"), &start));
    let run = p.devkit(&["todo", "start", &id], &[("CLAUDE_CODE_SESSION_ID", "S")]);
    assert!(run.status.success(), "{}", todoenv::stderr(&run));
    assert_eq!(p.todo(&id).status, in_progress("S/a1"));
}

#[test]
fn attribution_skips_the_session_itself() {
    let p = Proj::new();
    let id = seed(&p, "a");
    let out = p.hook(
        "pre-tool-use",
        "claude-code",
        &bash(&p, None, &format!("devkit todo start {id}")),
    );
    assert_eq!(stdout(&out), "");
    assert_eq!(p.todo(&id).status, Status::Pending);
}
