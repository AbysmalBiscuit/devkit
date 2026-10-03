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
    if s.trim().is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    let output = &v["hookSpecificOutput"];
    (output["permissionDecision"] == "deny").then(|| {
        output["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .to_string()
    })
}

/// What the pre-tool-use hook hands back for a sub-agent's Bash command: the
/// command the harness runs in its place, or the original when the hook left
/// it alone.
fn guarded(p: &Proj, harness: &str, agent: &str, command: &str) -> String {
    let out = p.hook("pre-tool-use", harness, &bash(p, Some(agent), command));
    assert_eq!(out.status.code(), Some(0), "{}", todoenv::stderr(&out));
    let s = stdout(&out);
    if s.trim().is_empty() {
        return command.to_string();
    }
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert!(denial(&out).is_none(), "denied: {s}");
    v["hookSpecificOutput"]["updatedInput"]["command"]
        .as_str()
        .unwrap_or_else(|| panic!("no updated command: {s}"))
        .to_string()
}

/// `command` from sub-agent `agent` of session `S`, through the hook and then
/// run the way the harness would run what the hook returned.
fn run_as_sub_agent(p: &Proj, agent: &str, command: &str) -> std::process::Output {
    let rewritten = guarded(p, "claude-code", agent, command);
    p.shell(&rewritten, &[("CLAUDE_CODE_SESSION_ID", "S")])
}

#[test]
fn attribution_a_sub_agents_start_runs_as_the_sub_agent() {
    let p = Proj::new();
    let id = seed(&p, "a");
    let rewritten = guarded(&p, "claude-code", "a1", &format!("devkit todo start {id}"));
    assert_eq!(
        p.todo(&id).status,
        Status::Pending,
        "the hook writes nothing"
    );
    let run = p.shell(&rewritten, &[("CLAUDE_CODE_SESSION_ID", "S")]);
    assert!(run.status.success(), "{}", todoenv::stderr(&run));
    assert_eq!(p.todo(&id).status, in_progress("S/a1"));
}

#[test]
fn attribution_a_command_that_never_reaches_the_todo_changes_nothing() {
    let p = Proj::new();
    let id = seed(&p, "a");
    run_as_sub_agent(&p, "a1", &format!("false && devkit todo done {id}"));
    assert_eq!(p.todo(&id).status, Status::Pending);
}

#[test]
fn attribution_start_then_done_credits_the_sub_agent() {
    let p = Proj::new();
    let id = seed(&p, "a");
    let run = run_as_sub_agent(
        &p,
        "a1",
        &format!("devkit todo start {id} && devkit todo done {id}"),
    );
    assert!(run.status.success(), "{}", todoenv::stderr(&run));
    assert_eq!(p.todo(&id).status, Status::Completed {
        by: Some(Holder::new("S/a1"))
    });
}

#[test]
fn attribution_a_siblings_start_is_denied_naming_the_holder() {
    let p = Proj::new();
    let id = seed(&p, "a");
    let start = format!("devkit todo start {id}");
    run_as_sub_agent(&p, "a1", &start);
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
fn attribution_the_session_starting_it_again_keeps_the_sub_agent() {
    let p = Proj::new();
    let id = seed(&p, "a");
    run_as_sub_agent(&p, "a1", &format!("devkit todo start {id}"));
    let run = p.devkit(&["todo", "start", &id], &[("CLAUDE_CODE_SESSION_ID", "S")]);
    assert!(run.status.success(), "{}", todoenv::stderr(&run));
    assert_eq!(p.todo(&id).status, in_progress("S/a1"));
}

#[test]
fn attribution_leaves_the_sessions_own_command_alone() {
    let p = Proj::new();
    let id = seed(&p, "a");
    let out = p.hook(
        "pre-tool-use",
        "claude-code",
        &bash(&p, None, &format!("devkit todo start {id}")),
    );
    assert_eq!(stdout(&out), "");
}

/// Codex's handling of a rewritten command is unverified, so its sub-agents'
/// changes land as the session.
#[test]
fn attribution_leaves_codex_commands_alone() {
    let p = Proj::new();
    let id = seed(&p, "a");
    let command = format!("devkit todo start {id}");
    assert_eq!(guarded(&p, "codex", "a1", &command), command);
}

/// The recorded payloads in `tests/fixtures/todo/<name>`, pointed at `p`.
fn fixture(p: &Proj, name: &str) -> Vec<serde_json::Value> {
    let body = std::fs::read_to_string(format!("tests/fixtures/todo/{name}")).unwrap();
    body.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
            v["cwd"] = json!(p.path);
            if v.get("transcript_path").is_some() {
                v["transcript_path"] = json!(p.outside().join("t.jsonl"));
            }
            v
        })
        .collect()
}

fn post_tool_use(p: &Proj, harness: &str, payloads: &[serde_json::Value]) {
    for payload in payloads {
        let out = p.hook("post-tool-use", harness, payload);
        assert_eq!(out.status.code(), Some(0), "{}", todoenv::stderr(&out));
        assert_eq!(stdout(&out), "", "capture writes nothing to stdout");
    }
}

fn by_text(p: &Proj, text: &str) -> devkit_todo::Todo {
    p.todos()
        .into_iter()
        .find(|t| t.description == text)
        .unwrap_or_else(|| panic!("no todo {text:?} in {:#?}", p.todos()))
}

const TASKS_SESSION: &str = "732b6b74-6009-478a-abe2-4129415b6007";

#[test]
fn capture_task_create_and_update_mirror_into_the_store() {
    let p = Proj::new();
    post_tool_use(&p, "claude-code", &fixture(&p, "claude-task-create.jsonl"));
    let alpha = by_text(&p, "alpha");
    assert_eq!(
        alpha.project.as_deref(),
        Some(format!("proj.main.claude-{TASKS_SESSION}").as_str())
    );
    assert_eq!(alpha.status, Status::Pending);
    post_tool_use(&p, "claude-code", &fixture(&p, "claude-task-update.jsonl"));
    assert_eq!(by_text(&p, "alpha").status, Status::Completed {
        by: Some(Holder::new(TASKS_SESSION))
    });
    assert_eq!(by_text(&p, "beta").status, Status::Pending);
}

#[test]
fn capture_deleted_becomes_cancelled() {
    let p = Proj::new();
    post_tool_use(&p, "claude-code", &fixture(&p, "claude-task-create.jsonl"));
    post_tool_use(&p, "claude-code", &fixture(&p, "claude-task-deleted.jsonl"));
    assert_eq!(by_text(&p, "beta").status, Status::Cancelled {
        by: Some(Holder::new(TASKS_SESSION))
    });
}

#[test]
fn capture_a_sub_agents_native_tasks_are_attributed_to_it() {
    let p = Proj::new();
    post_tool_use(
        &p,
        "claude-code",
        &fixture(&p, "claude-subagent-tasks.jsonl"),
    );
    let session = "c967902f-a6ca-4488-b58d-5c8c0a6cacc6";
    assert_eq!(by_text(&p, "gamma").status, Status::Completed {
        by: Some(Holder::new(format!("{session}/a06ce5e051ede4454")))
    });
    assert_eq!(by_text(&p, "parent-step").status, Status::Completed {
        by: Some(Holder::new(session))
    });
    assert_eq!(
        by_text(&p, "gamma").project,
        by_text(&p, "parent-step").project,
        "a sub-agent writes to its session's node"
    );
}

#[test]
fn capture_a_parent_updates_a_task_its_sub_agent_created() {
    let p = Proj::new();
    let mut payloads = fixture(&p, "claude-subagent-tasks.jsonl");
    payloads.truncate(2);
    let mut parent_done = payloads[1].clone();
    for key in ["agent_id", "agent_type"] {
        parent_done.as_object_mut().unwrap().remove(key);
    }
    parent_done["tool_name"] = json!("TaskUpdate");
    parent_done["tool_input"] = json!({"taskId": "2", "status": "completed"});
    payloads.push(parent_done);
    post_tool_use(&p, "claude-code", &payloads);
    assert_eq!(by_text(&p, "gamma").status.kind(), StatusKind::Completed);
}

const PLAN_SESSION: &str = "01a10214-8027-70e1-b13f-0bb35d607bed";

fn on_node(p: &Proj, node: &str) -> Vec<devkit_todo::Todo> {
    let mut todos: Vec<_> = p
        .todos()
        .into_iter()
        .filter(|t| t.project.as_deref() == Some(node))
        .collect();
    todos.sort_by_key(|t| t.order);
    todos
}

#[test]
fn plan_update_plan_sequence_mirrors_into_the_store() {
    let p = Proj::new();
    post_tool_use(&p, "codex", &fixture(&p, "codex-update-plan.jsonl"));
    let todos = on_node(&p, &format!("proj.main.codex-{PLAN_SESSION}"));
    let open: Vec<(&str, StatusKind)> = todos
        .iter()
        .filter(|t| t.status.kind() != StatusKind::Cancelled)
        .map(|t| (t.description.as_str(), t.status.kind()))
        .collect();
    assert_eq!(
        open,
        [
            ("run tests", StatusKind::Completed),
            ("run tests", StatusKind::InProgress)
        ],
        "both repeats are kept, in the last plan's order"
    );
    let build = by_text(&p, "build");
    assert_eq!(build.status, Status::Cancelled {
        by: Some(Holder::new(PLAN_SESSION))
    });
    assert_eq!(todos.len(), 3, "no step was added twice");
}

#[test]
fn plan_a_sub_agents_first_list_leaves_the_parent_alone() {
    let p = Proj::new();
    let parent = fixture(&p, "codex-update-plan.jsonl").remove(0);
    let mut sub = fixture(&p, "codex-subagent-plan.jsonl").remove(0);
    sub["session_id"] = json!(PLAN_SESSION);
    let agent = sub["agent_id"].as_str().unwrap().to_string();
    post_tool_use(&p, "codex", &[parent, sub]);
    let todos = on_node(&p, &format!("proj.main.codex-{PLAN_SESSION}"));
    let parents: Vec<_> = todos
        .iter()
        .filter(|t| t.description != "sub step")
        .map(|t| t.status.clone())
        .collect();
    assert_eq!(parents, [Status::Pending, Status::Pending, Status::Pending]);
    assert_eq!(
        by_text(&p, "sub step").status,
        in_progress(&format!("{PLAN_SESSION}/{agent}"))
    );
}

#[test]
fn plan_todowrite_mirrors_into_the_store() {
    let p = Proj::new();
    post_tool_use(&p, "claude-code", &fixture(&p, "claude-todowrite.jsonl"));
    assert_eq!(by_text(&p, "one").status.kind(), StatusKind::Completed);
    assert_eq!(by_text(&p, "two").status.kind(), StatusKind::Cancelled);
    assert_eq!(by_text(&p, "three").status, Status::Pending);
}

#[test]
fn plan_a_cancel_refused_by_a_claim_is_retried_on_the_next_list() {
    let p = Proj::new();
    let mut lists = fixture(&p, "claude-todowrite.jsonl");
    post_tool_use(&p, "claude-code", &lists[..1]);
    let two = by_text(&p, "two").id;
    claim(&p, &two, "human");
    let mut empty = lists.remove(1);
    empty["tool_input"]["todos"] = json!([]);
    post_tool_use(&p, "claude-code", std::slice::from_ref(&empty));
    assert_eq!(p.todo(&two).status, in_progress("human"));
    p.store()
        .apply(&Edit::SetStatus {
            id: two.clone(),
            to: StatusKind::Pending,
            actor: Holder::human(),
        })
        .unwrap();
    post_tool_use(&p, "claude-code", &[empty]);
    assert_eq!(p.todo(&two).status.kind(), StatusKind::Cancelled);
}

#[test]
fn plan_purge_forgets_the_mirrored_text() {
    let p = Proj::new();
    let mut first = fixture(&p, "claude-todowrite.jsonl").remove(0);
    first["tool_input"]["todos"][0]["content"] = json!("SECRET-abc123");
    post_tool_use(&p, "claude-code", &[first]);
    let id = by_text(&p, "SECRET-abc123").id;
    let purge = p.devkit(&["todo", "purge", &id], &[("DEVKIT_CALLER", "human")]);
    assert!(purge.status.success(), "{}", todoenv::stderr(&purge));
    let native = std::fs::read_to_string(p.state().join("todo/native.json")).unwrap();
    assert!(!native.contains("SECRET-abc123"), "{native}");
}
