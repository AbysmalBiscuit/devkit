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
