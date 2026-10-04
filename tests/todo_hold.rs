//! `devkit hook stop` holding an agent to its open todos, driven with real
//! payloads on stdin.

#[path = "common/todoenv.rs"]
mod todoenv;

use devkit_todo::{Edit, Holder, NewTodo, StatusKind, TodoStore};
use serde_json::{Value, json};
use todoenv::{HeldLock, Proj, stderr, stdout};

const MAIN: &str = "proj.main.claude-S";

fn stop(p: &Proj, session: &str) -> serde_json::Value {
    json!({
        "hook_event_name": "Stop",
        "session_id": session,
        "stop_hook_active": false,
        "cwd": p.path,
    })
}

fn codex_stop(p: &Proj) -> Value {
    json!({
        "hook_event_name": "Stop",
        "turn_id": "t",
        "session_id": "S",
        "stop_hook_active": false,
        "cwd": p.path,
    })
}

fn set(p: &Proj, id: &str, to: StatusKind, by: &str) {
    p.store()
        .apply(&Edit::SetStatus {
            id: id.into(),
            to,
            actor: Holder::new(by),
        })
        .unwrap();
}

/// The reason a stop printed, failing unless it printed a block.
fn blocked(out: &std::process::Output) -> String {
    assert!(out.status.success(), "{}", stderr(out));
    let answer: Value = serde_json::from_str(stdout(out).trim())
        .unwrap_or_else(|e| panic!("{e}: {:?}", stdout(out)));
    assert_eq!(answer["decision"], "block", "{answer}");
    answer["reason"].as_str().unwrap().to_string()
}

fn silent(out: &std::process::Output) {
    assert!(out.status.success(), "{}", stderr(out));
    assert_eq!(stdout(out), "");
}

fn git(p: &Proj, args: &[&str]) {
    devkit_git::Git::fixture(&p.path)
        .args(args.iter().copied())
        .output()
        .unwrap();
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
    silent(&p.hook("stop", "claude-code", &stop(&p, "S")));
}

#[test]
fn a_broken_config_never_holds() {
    let p = Proj::with_home_config("[todo]\nbackend = 3\n");
    seed(&p, "proj.main.claude-S", "write the migration");
    silent(&p.hook("stop", "claude-code", &stop(&p, "S")));
}

#[test]
fn a_missing_task_binary_never_holds() {
    let p = Proj::with_home_config(
        "[todo]\nbackend = \"taskwarrior\"\n[todo.taskwarrior]\npath = \"/nonexistent/task\"\n",
    );
    seed(&p, MAIN, "write the migration");
    silent(&p.hook("stop", "claude-code", &stop(&p, "S")));
}

#[test]
fn open_todos_block_and_list_their_ids() {
    let p = Proj::new();
    let id = seed(&p, MAIN, "write the migration");
    let reason = blocked(&p.hook("stop", "claude-code", &stop(&p, "S")));
    assert_eq!(
        reason,
        format!(
            "devkit todo: you have open todos.\n\n\
             ## {MAIN}\n\
             - [ ] write the migration ({id})\n\n\
             Finish each one, or cancel one that no longer applies (`devkit todo cancel <id>`, \
             or delete it in your task tool).\n\
             Before you stop to ask the user something:\n\
             - With a clear recommendation, take it and say so in your final report.\n\
             - Without one, ask a sub-agent on a bigger model and take its answer.\n\
             - Stop for the user only on a decision that is theirs: a destructive or irreversible \
             action, anything outward-facing, a change of scope, or a preference with no default. \
             To stop for one, end your turn again: this reminder comes once per unchanged list."
        )
    );
}

#[test]
fn a_second_stop_with_the_same_list_goes_through() {
    let p = Proj::new();
    seed(&p, MAIN, "write the migration");
    blocked(&p.hook("stop", "claude-code", &stop(&p, "S")));
    silent(&p.hook("stop", "claude-code", &stop(&p, "S")));
}

#[test]
fn finishing_one_rearms_on_the_rest() {
    let p = Proj::new();
    let first = seed(&p, MAIN, "write the migration");
    seed(&p, MAIN, "run the migration");
    blocked(&p.hook("stop", "claude-code", &stop(&p, "S")));
    set(&p, &first, StatusKind::Completed, "S");
    let reason = blocked(&p.hook("stop", "claude-code", &stop(&p, "S")));
    assert!(reason.contains("run the migration"), "{reason}");
    assert!(!reason.contains("write the migration"), "{reason}");
}

#[test]
fn a_user_prompt_rearms_and_prints_nothing() {
    let p = Proj::new();
    seed(&p, MAIN, "write the migration");
    blocked(&p.hook("stop", "claude-code", &stop(&p, "S")));
    let prompt = json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "S",
        "prompt": "carry on",
        "cwd": p.path,
    });
    silent(&p.hook("user-prompt-submit", "claude-code", &prompt));
    blocked(&p.hook("stop", "claude-code", &stop(&p, "S")));
}

#[test]
fn a_compaction_rearms() {
    let p = Proj::new();
    seed(&p, MAIN, "write the migration");
    blocked(&p.hook("stop", "claude-code", &stop(&p, "S")));
    let compact = json!({
        "hook_event_name": "PostCompact",
        "session_id": "S",
        "trigger": "auto",
        "cwd": p.path,
    });
    silent(&p.hook("post-compact", "claude-code", &compact));
    blocked(&p.hook("stop", "claude-code", &stop(&p, "S")));
}

#[test]
fn nothing_open_goes_through() {
    let p = Proj::new();
    let id = seed(&p, MAIN, "write the migration");
    set(&p, &id, StatusKind::Completed, "S");
    silent(&p.hook("stop", "claude-code", &stop(&p, "S")));
}

#[test]
fn a_sub_agents_claim_does_not_hold_its_session() {
    let p = Proj::new();
    let id = seed(&p, MAIN, "write the migration");
    set(&p, &id, StatusKind::InProgress, "S/a1");
    silent(&p.hook("stop", "claude-code", &stop(&p, "S")));
}

#[test]
fn a_claim_on_another_node_still_holds() {
    let p = Proj::new();
    let id = seed(&p, "proj.other.claude-S", "write the migration");
    set(&p, &id, StatusKind::InProgress, "S");
    let reason = blocked(&p.hook("stop", "claude-code", &stop(&p, "S")));
    assert!(reason.contains("write the migration"), "{reason}");
}

#[test]
fn another_sessions_pending_todos_never_hold() {
    let p = Proj::new();
    seed(&p, "proj.main.claude-T", "write the migration");
    seed(&p, "proj.main", "a workspace todo");
    silent(&p.hook("stop", "claude-code", &stop(&p, "S")));
}

#[test]
fn outside_a_workspace_only_claims_hold() {
    let p = Proj::new();
    git(&p, &["commit", "-q", "--allow-empty", "-m", "init"]);
    git(&p, &["checkout", "-q", "--detach"]);
    seed(&p, "proj", "a project todo");
    silent(&p.hook("stop", "claude-code", &stop(&p, "S")));
    let id = seed(&p, "proj", "write the migration");
    set(&p, &id, StatusKind::InProgress, "S");
    let reason = blocked(&p.hook("stop", "claude-code", &stop(&p, "S")));
    assert!(reason.contains("write the migration"), "{reason}");
    assert!(!reason.contains("a project todo"), "{reason}");
}

#[test]
fn a_codex_interrupt_goes_through() {
    let p = Proj::new();
    seed(&p, "proj.main.codex-S", "write the migration");
    let interrupt = json!({
        "hook_event_name": "Interrupt",
        "turn_id": "t",
        "session_id": "S",
        "cwd": p.path,
    });
    silent(&p.hook("stop", "codex", &interrupt));
    blocked(&p.hook("stop", "codex", &codex_stop(&p)));
}

#[test]
fn a_codex_stop_blocks() {
    let p = Proj::new();
    seed(&p, "proj.main.codex-S", "write the migration");
    let reason = blocked(&p.hook("stop", "codex", &codex_stop(&p)));
    assert!(reason.contains("write the migration"), "{reason}");
}

#[test]
fn cursor_never_holds() {
    let p = Proj::new();
    let id = seed(&p, MAIN, "write the migration");
    set(&p, &id, StatusKind::InProgress, "S");
    let cursor = json!({
        "hook_event_name": "stop",
        "conversation_id": "S",
        "status": "completed",
        "workspace_roots": [p.path],
    });
    silent(&p.hook("stop", "cursor", &cursor));
}

#[test]
fn no_session_id_goes_through() {
    let p = Proj::new();
    seed(&p, MAIN, "write the migration");
    silent(&p.hook(
        "stop",
        "claude-code",
        &json!({
            "hook_event_name": "Stop",
            "stop_hook_active": false,
            "cwd": p.path,
        }),
    ));
}

#[test]
fn a_queued_capture_lands_before_the_hold_reads() {
    let p = Proj::new();
    let env = [("DEVKIT_TODO_BACKEND", "taskchampion")];
    let tool = |name: &str, input: Value, response: Value| {
        json!({
            "hook_event_name": "PostToolUse",
            "session_id": "S",
            "cwd": p.path,
            "tool_name": name,
            "tool_input": input,
            "tool_response": response,
        })
    };
    let create = tool(
        "TaskCreate",
        json!({"subject": "write the migration", "description": "x"}),
        json!({"task": {"id": "1", "subject": "write the migration"}}),
    );
    silent(&p.hook_with("post-tool-use", "claude-code", &create, &env));
    let held = HeldLock::at(p.state().join("todo/taskchampion/devkit.lock"));
    let done = tool(
        "TaskUpdate",
        json!({"taskId": "1", "status": "completed"}),
        json!({}),
    );
    silent(&p.hook_with("post-tool-use", "claude-code", &done, &env));
    drop(held);
    silent(&p.hook_with("stop", "claude-code", &stop(&p, "S"), &env));
}

#[test]
fn session_end_forgets_the_sessions_holds() {
    let p = Proj::new();
    seed(&p, MAIN, "write the migration");
    blocked(&p.hook("stop", "claude-code", &stop(&p, "S")));
    let holds = p
        .state()
        .join("todo/holds")
        .join(devkit_todo::render::digest("S"));
    assert!(holds.is_dir(), "{}", holds.display());
    let end = json!({
        "hook_event_name": "SessionEnd",
        "session_id": "S",
        "reason": "exit",
        "cwd": p.path,
    });
    silent(&p.hook("session-end", "claude-code", &end));
    assert!(!holds.exists(), "{}", holds.display());
}

fn sub_agent(p: &Proj, event: &str, agent_type: Option<&str>) -> Value {
    let mut payload = json!({
        "hook_event_name": event,
        "session_id": "S",
        "agent_id": "a1",
        "cwd": p.path,
    });
    if let Some(agent_type) = agent_type {
        payload["agent_type"] = json!(agent_type);
    }
    payload
}

fn sub_agent_stop(p: &Proj) -> std::process::Output {
    p.hook(
        "subagent-stop",
        "claude-code",
        &sub_agent(p, "SubagentStop", Some("general-purpose")),
    )
}

fn locked_by(p: &Proj, holder: &str) -> bool {
    let out = p.devkit(&["locks", "status", "--json"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    stdout(&out).contains(&format!("\"{holder}\""))
}

fn run_open(p: &Proj) -> bool {
    let runs = p.activity().runs;
    let run = runs
        .iter()
        .find(|r| r.agent == "a1")
        .unwrap_or_else(|| panic!("no run for a1: {runs:?}"));
    run.end.is_none()
}

/// A sub-agent `a1` of session `S` that started, claimed a todo and holds a
/// file lock.
fn claiming_sub_agent(p: &Proj) -> String {
    silent(&p.hook(
        "subagent-start",
        "claude-code",
        &sub_agent(p, "SubagentStart", Some("general-purpose")),
    ));
    let id = seed(p, MAIN, "write the migration");
    set(p, &id, StatusKind::InProgress, "S/a1");
    let lock = p.devkit(&["locks", "acquire", "--as", "S/a1", "f"], &[]);
    assert!(lock.status.success(), "{}", stderr(&lock));
    assert!(locked_by(p, "S/a1"));
    id
}

#[test]
fn a_sub_agent_with_a_claim_is_held_and_keeps_everything() {
    let p = Proj::new();
    let id = claiming_sub_agent(&p);
    let reason = blocked(&sub_agent_stop(&p));
    assert!(reason.contains("write the migration"), "{reason}");
    assert_eq!(p.todo(&id).status, devkit_todo::Status::InProgress {
        by: Holder::new("S/a1")
    });
    assert!(locked_by(&p, "S/a1"));
    assert!(run_open(&p));
}

#[test]
fn an_allowed_sub_agent_releases_everything() {
    let p = Proj::new();
    let id = claiming_sub_agent(&p);
    blocked(&sub_agent_stop(&p));
    silent(&sub_agent_stop(&p));
    assert_eq!(p.todo(&id).status, devkit_todo::Status::Pending);
    assert!(!locked_by(&p, "S/a1"));
    assert!(!run_open(&p));
    let fingerprint = p
        .state()
        .join("todo/holds")
        .join(devkit_todo::render::digest("S"))
        .join(devkit_todo::render::digest("S/a1"));
    assert!(!fingerprint.exists(), "{}", fingerprint.display());
}

#[test]
fn a_fork_never_holds() {
    let p = Proj::new();
    let id = seed(&p, MAIN, "write the migration");
    set(&p, &id, StatusKind::InProgress, "S");
    silent(&p.hook(
        "subagent-stop",
        "claude-code",
        &sub_agent(&p, "SubagentStop", None),
    ));
}

#[test]
fn a_sub_agent_that_never_claimed_goes_through() {
    let p = Proj::new();
    seed(&p, MAIN, "write the migration");
    silent(&sub_agent_stop(&p));
}
