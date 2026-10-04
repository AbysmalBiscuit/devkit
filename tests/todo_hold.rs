//! `devkit hook stop` holding an agent to its open todos, driven with real
//! payloads on stdin.

#[path = "common/todoenv.rs"]
mod todoenv;

use devkit_todo::{NewTodo, TodoStore};
use serde_json::json;
use todoenv::{Proj, stdout};

fn stop(p: &Proj, session: &str) -> serde_json::Value {
    json!({
        "hook_event_name": "Stop",
        "session_id": session,
        "stop_hook_active": false,
        "cwd": p.path,
    })
}

fn seed(p: &Proj, node: &str, text: &str) -> String {
    p.store()
        .add(NewTodo {
            project: Some(node.into()),
            description: text.into(),
            parent: None,
            order: None,
        })
        .unwrap()
}

#[test]
fn config_off_never_holds() {
    let p = Proj::with_home_config("[todo]\nhold_stop = false\n");
    seed(&p, "proj.main.claude-S", "write the migration");
    let out = p.hook("stop", "claude-code", &stop(&p, "S"));
    assert!(out.status.success());
    assert_eq!(stdout(&out), "");
}

#[test]
fn a_broken_config_never_holds() {
    let p = Proj::with_home_config("[todo]\nbackend = 3\n");
    seed(&p, "proj.main.claude-S", "write the migration");
    let out = p.hook("stop", "claude-code", &stop(&p, "S"));
    assert!(out.status.success());
    assert_eq!(stdout(&out), "");
}

#[test]
fn a_missing_task_binary_never_holds() {
    let p = Proj::with_home_config(
        "[todo]\nbackend = \"taskwarrior\"\n[todo.taskwarrior]\npath = \"/nonexistent/task\"\n",
    );
    let out = p.hook("stop", "claude-code", &stop(&p, "S"));
    assert!(out.status.success());
    assert_eq!(stdout(&out), "");
}
