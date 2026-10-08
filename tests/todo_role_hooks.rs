#[path = "common/todoenv.rs"]
mod todoenv;

use serde_json::{Value, json};
use todoenv::{Proj, stderr, stdout};

const WORKFLOW: &str = r#"
[todo.roles.manager]
scope = "workspace"
[todo.roles.implementer]
scope = "agent"
parent = "manager"
agent_types = ["implementer"]
[todo.roles.reviewer]
scope = "agent"
parent = "manager"
"#;
const MAIN: [(&str, &str); 1] = [("CLAUDE_CODE_SESSION_ID", "S")];
const WORKER: [(&str, &str); 2] = [MAIN[0], ("DEVKIT_TODO_HOLDER", "S/a1")];

fn spawn(p: &Proj, agent_type: &str) -> Value {
    json!({"session_id": "S", "agent_id": "a1", "agent_type": agent_type,
        "cwd": p.path, "hook_event_name": "SubagentStart"})
}

#[test]
fn subagent_start_records_configured_role_without_stdout() {
    let p = Proj::with_home_config(WORKFLOW);
    let out = p.hook("subagent-start", "claude-code", &spawn(&p, "implementer"));
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    let role = p.devkit(&["todo", "role"], &WORKER);
    assert!(role.status.success(), "{}", stderr(&role));
    assert_eq!(
        stdout(&role),
        "Role `implementer`, writing to `proj.main.claude-S.a1`.\n"
    );
    let parent = p.devkit(&["todo", "role"], &MAIN);
    assert!(
        stdout(&parent).contains("Role `main`"),
        "{}",
        stdout(&parent)
    );
}

#[test]
fn native_task_tools_write_to_the_workers_role_node() {
    let p = Proj::with_home_config(WORKFLOW);
    assert!(
        p.hook("subagent-start", "claude-code", &spawn(&p, "implementer"))
            .status
            .success()
    );
    for (tool, input, response) in [
        (
            "TaskCreate",
            json!({"subject": "created task"}),
            json!({"task": {"id": "1"}}),
        ),
        (
            "TodoWrite",
            json!({"todos": [{"content": "worker list", "status": "pending"}]}),
            json!({}),
        ),
    ] {
        let mut payload = spawn(&p, "implementer");
        payload["hook_event_name"] = json!("PostToolUse");
        payload["tool_name"] = json!(tool);
        payload["tool_input"] = input;
        payload["tool_response"] = response;
        let out = p.hook("post-tool-use", "claude-code", &payload);
        assert!(out.status.success(), "{}", stderr(&out));
        assert_eq!(stdout(&out), "");
    }
    let todos = p.todos();
    assert_eq!(todos.len(), 2);
    assert!(
        todos
            .iter()
            .all(|todo| todo.node() == "proj.main.claude-S.a1"),
        "{todos:?}"
    );
}

#[test]
fn the_first_todo_write_suggests_child_roles_once() {
    let manifest: Value = serde_json::from_str(include_str!("../plugin/hooks/hooks.json")).unwrap();
    for (tool, input) in [
        ("TaskCreate", json!({"subject": "work"})),
        (
            "TodoWrite",
            json!({"todos": [{"content": "work", "status": "pending"}]}),
        ),
        ("Bash", json!({"command": "devkit todo add 'work'"})),
    ] {
        assert!(
            manifest["hooks"]["PreToolUse"]
                .as_array()
                .unwrap()
                .iter()
                .any(|group| {
                    group["matcher"]
                        .as_str()
                        .is_none_or(|pattern| pattern.split('|').any(|name| name == tool))
                }),
            "PreToolUse does not register {tool}"
        );
        let p = Proj::with_home_config(WORKFLOW);
        assert!(
            p.devkit(&["todo", "role", "manager"], &MAIN)
                .status
                .success()
        );
        let mut payload = spawn(&p, "general-purpose");
        payload["hook_event_name"] = json!("PreToolUse");
        payload["tool_name"] = json!(tool);
        payload["tool_input"] = input;
        let first = p.hook("pre-tool-use", "claude-code", &payload);
        assert!(first.status.success(), "{}", stderr(&first));
        let answer: Value = serde_json::from_slice(&first.stdout)
            .unwrap_or_else(|e| panic!("{tool}: {e}: {}", stdout(&first)));
        let context = answer["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        for expected in [
            "default `subagent`, writing to `proj.main.claude-S`",
            "implementer (writing to `proj.main.claude-S.a1`)",
            "reviewer (writing to `proj.main.claude-S.a1`)",
            "devkit todo role <name>",
        ] {
            assert!(context.contains(expected), "{tool}: {context}");
        }
        let next = p.hook("pre-tool-use", "claude-code", &payload);
        assert!(next.status.success(), "{}", stderr(&next));
        assert!(
            !stdout(&next).contains("Todo roles you can take"),
            "{}",
            stdout(&next)
        );
        if tool == "Bash" {
            let command = answer["hookSpecificOutput"]["updatedInput"]["command"]
                .as_str()
                .unwrap();
            let out = p.shell(command, &MAIN);
            assert!(out.status.success(), "{}", stderr(&out));
            assert_eq!(p.todos()[0].node(), "proj.main.claude-S");
        }
    }
}

fn injected(out: &std::process::Output) -> String {
    assert!(out.status.success(), "{}", stderr(out));
    let answer: Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", stdout(out)));
    answer["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .into()
}

#[test]
fn context_names_the_role_and_follows_changed_scope_config() {
    let p = Proj::with_home_config(WORKFLOW);
    assert!(
        p.hook("subagent-start", "claude-code", &spawn(&p, "implementer"))
            .status
            .success()
    );
    assert!(
        p.devkit(&["todo", "add", "worker plan"], &WORKER)
            .status
            .success()
    );
    let payload = spawn(&p, "implementer");
    let args = ["todo", "context", "--harness", "claude-code"];
    let first = p.devkit_in(&p.path, &args, &[], &payload.to_string());
    let text = injected(&first);
    assert!(
        text.contains("Role `implementer`, writing to `proj.main.claude-S.a1`."),
        "{text}"
    );
    assert!(text.contains("worker plan"), "{text}");
    std::fs::write(
        p.home_config(),
        WORKFLOW.replace("scope = \"agent\"", "scope = \"workspace\""),
    )
    .unwrap();
    let next = p.devkit_in(&p.path, &args, &[], &payload.to_string());
    let text = injected(&next);
    assert!(
        text.contains("Role `implementer`, writing to `proj.main`."),
        "{text}"
    );
    assert!(text.contains("worker plan"), "{text}");
}

#[test]
fn a_worker_at_agent_scope_is_held_on_pending_todos_once() {
    let p = Proj::with_home_config(WORKFLOW);
    assert!(
        p.hook("subagent-start", "claude-code", &spawn(&p, "implementer"))
            .status
            .success()
    );
    assert!(
        p.devkit(&["todo", "add", "finish the worker plan"], &WORKER)
            .status
            .success()
    );
    let mut payload = spawn(&p, "implementer");
    payload["hook_event_name"] = json!("SubagentStop");
    payload["stop_hook_active"] = json!(false);
    let first = p.hook("subagent-stop", "claude-code", &payload);
    assert!(first.status.success(), "{}", stderr(&first));
    let answer: Value =
        serde_json::from_slice(&first.stdout).unwrap_or_else(|e| panic!("{e}: {}", stdout(&first)));
    assert_eq!(answer["decision"], "block");
    assert!(
        answer["reason"]
            .as_str()
            .unwrap()
            .contains("finish the worker plan")
    );
    let next = p.hook("subagent-stop", "claude-code", &payload);
    assert!(next.status.success(), "{}", stderr(&next));
    assert_eq!(stdout(&next), "");
}

#[test]
fn manager_pending_holds_only_with_an_explicit_override() {
    for hold_pending in [false, true] {
        let p = Proj::with_home_config(&format!(
            "[todo]\nhold_stop = \"always\"\n[todo.roles.manager]\nscope = \"workspace\"\n{}",
            if hold_pending {
                "hold_pending = true\n"
            } else {
                ""
            }
        ));
        assert!(
            p.devkit(&["todo", "role", "manager"], &MAIN)
                .status
                .success()
        );
        assert!(
            p.devkit(&["todo", "add", "workspace plan"], &MAIN)
                .status
                .success()
        );
        let payload = json!({"session_id": "S", "cwd": p.path, "hook_event_name": "Stop", "stop_hook_active": false});
        let out = p.hook("stop", "claude-code", &payload);
        assert!(out.status.success(), "{}", stderr(&out));
        if hold_pending {
            let answer: Value = serde_json::from_slice(&out.stdout)
                .unwrap_or_else(|e| panic!("{e}: {}", stdout(&out)));
            assert_eq!(answer["decision"], "block");
        } else {
            assert_eq!(stdout(&out), "");
        }
    }
}

#[test]
fn explicit_false_disables_the_builtin_main_pending_hold() {
    let p = Proj::with_home_config(
        "[todo]\nhold_stop = \"always\"\n[todo.roles.main]\nscope = \"session\"\nhold_pending = false\n",
    );
    assert!(
        p.devkit(&["todo", "add", "pending"], &MAIN)
            .status
            .success()
    );
    let out = p.hook("stop", "claude-code", &json!({"session_id": "S", "cwd": p.path, "hook_event_name": "Stop", "stop_hook_active": false}));
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
}

#[test]
fn builtins_assigned_roles_and_unrelated_tools_do_not_nudge() {
    for (workflow, agent_type, tool, input) in [
        (
            "",
            "general-purpose",
            "TaskCreate",
            json!({"subject": "work"}),
        ),
        (
            WORKFLOW,
            "implementer",
            "TaskCreate",
            json!({"subject": "work"}),
        ),
        (
            WORKFLOW,
            "general-purpose",
            "TaskUpdate",
            json!({"taskId": "1", "status": "completed"}),
        ),
        (
            WORKFLOW,
            "general-purpose",
            "Bash",
            json!({"command": "echo devkit todo add work"}),
        ),
    ] {
        let p = Proj::with_home_config(workflow);
        if !workflow.is_empty() {
            assert!(
                p.devkit(&["todo", "role", "manager"], &MAIN)
                    .status
                    .success()
            );
        }
        let mut payload = spawn(&p, agent_type);
        payload["hook_event_name"] = json!("PreToolUse");
        payload["tool_name"] = json!(tool);
        payload["tool_input"] = input;
        let out = p.hook("pre-tool-use", "claude-code", &payload);
        assert!(out.status.success(), "{}", stderr(&out));
        assert!(
            !stdout(&out).contains("Todo roles you can take"),
            "{}",
            stdout(&out)
        );
    }
}

#[test]
fn main_shell_writes_receive_parentless_role_suggestions() {
    let p = Proj::with_home_config(WORKFLOW);
    let payload = json!({"session_id": "S", "cwd": p.path, "hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": "devkit todo add work"}});
    let text = injected(&p.hook("pre-tool-use", "claude-code", &payload));
    assert!(text.contains("manager"), "{text}");
    assert!(!text.contains("implementer"), "{text}");
    assert!(!text.contains("reviewer"), "{text}");
}

#[test]
fn child_shell_role_selection_preserves_the_manager() {
    let p = Proj::with_home_config(WORKFLOW);
    assert!(
        p.devkit(&["todo", "role", "manager"], &MAIN)
            .status
            .success()
    );
    let mut payload = spawn(&p, "general-purpose");
    payload["hook_event_name"] = json!("PreToolUse");
    payload["tool_name"] = json!("Bash");
    payload["tool_input"] = json!({"command": "devkit todo role implementer && devkit todo add work && devkit todo role"});
    let answer: Value =
        serde_json::from_slice(&p.hook("pre-tool-use", "claude-code", &payload).stdout).unwrap();
    let command = answer["hookSpecificOutput"]["updatedInput"]["command"]
        .as_str()
        .unwrap();
    let out = p.shell(command, &MAIN);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("Role `implementer`, writing to `proj.main.claude-S.a1`"),
        "{}",
        stdout(&out)
    );
    assert_eq!(
        stdout(&p.devkit(&["todo", "role"], &MAIN)),
        "Role `manager`, writing to `proj.main`.\n"
    );
    assert_eq!(p.todos()[0].node(), "proj.main.claude-S.a1");
}

#[test]
fn child_scripts_receive_only_their_invocations_identity() {
    let p = Proj::with_home_config(WORKFLOW);
    assert!(
        p.devkit(&["todo", "role", "manager"], &MAIN)
            .status
            .success()
    );
    std::fs::write(
        p.path.join("worker.sh"),
        "devkit todo role implementer && devkit todo add work\n",
    )
    .unwrap();
    let mut payload = spawn(&p, "general-purpose");
    payload["hook_event_name"] = json!("PreToolUse");
    payload["tool_name"] = json!("Bash");
    payload["tool_input"] =
        json!({"command": "bash worker.sh", "timeout": 12345, "description": "Worker plan"});
    let out = p.hook("pre-tool-use", "claude-code", &payload);
    assert!(out.status.success(), "{}", stderr(&out));
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    let input = &answer["hookSpecificOutput"]["updatedInput"];
    assert_eq!(input["timeout"], 12345);
    assert_eq!(input["description"], "Worker plan");
    let out = p.shell(input["command"].as_str().unwrap(), &MAIN);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(p.todos()[0].node(), "proj.main.claude-S.a1");
    assert_eq!(
        stdout(&p.devkit(&["todo", "role"], &MAIN)),
        "Role `manager`, writing to `proj.main`.\n"
    );
}

#[test]
fn codex_plan_write_suggests_roles_and_mirrors_the_selected_scope() {
    let manifest: Value =
        serde_json::from_str(include_str!("../plugin/hooks/hooks-codex.json")).unwrap();
    assert!(
        manifest["hooks"]["PreToolUse"]
            .as_array()
            .unwrap()
            .iter()
            .any(|group| {
                group["matcher"]
                    .as_str()
                    .is_none_or(|pattern| pattern.split('|').any(|name| name == "update_plan"))
            }),
        "PreToolUse does not register update_plan"
    );
    let p = Proj::with_home_config(WORKFLOW);
    let env = [("CODEX_SESSION_ID", "C")];
    let mut payload = json!({"session_id": "C", "cwd": p.path, "hook_event_name": "PreToolUse", "tool_name": "update_plan", "tool_input": {"plan": [{"step": "work", "status": "pending"}]}});
    let context = injected(&p.hook("pre-tool-use", "codex", &payload));
    assert!(
        context.contains("Todo roles you can take: manager"),
        "{context}"
    );
    assert!(
        p.devkit(&["todo", "role", "manager"], &env)
            .status
            .success()
    );
    payload["hook_event_name"] = json!("PostToolUse");
    payload["tool_response"] = json!("Plan updated");
    let out = p.hook("post-tool-use", "codex", &payload);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(p.todos()[0].node(), "proj.main");
}

/// `p` with its sub-agent `S/a1` having selected `implementer` in the
/// checkout.
fn implementer_in_checkout(p: &Proj) {
    let role = p.devkit(&["todo", "role", "implementer"], &WORKER);
    assert!(role.status.success(), "{}", stderr(&role));
}

#[test]
fn a_role_selected_in_the_checkout_routes_adds_from_outside_any_repository() {
    let p = Proj::with_home_config(WORKFLOW);
    implementer_in_checkout(&p);
    let add = p.devkit_in(p.outside(), &["todo", "add", "from tmp"], &WORKER, "");
    assert!(add.status.success(), "{}", stderr(&add));
    assert_eq!(p.todos()[0].node(), "proj.main.claude-S.a1");
    assert_eq!(stderr(&add), "");
}

#[test]
fn a_role_selected_in_the_checkout_routes_native_tasks_from_outside_any_repository() {
    let p = Proj::with_home_config(WORKFLOW);
    implementer_in_checkout(&p);
    let mut payload = spawn(&p, "general-purpose");
    payload["cwd"] = json!(p.outside());
    payload["hook_event_name"] = json!("PostToolUse");
    payload["tool_name"] = json!("TaskCreate");
    payload["tool_input"] = json!({"subject": "created task"});
    payload["tool_response"] = json!({"task": {"id": "1"}});
    let out = p.hook("post-tool-use", "claude-code", &payload);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(p.todos()[0].node(), "proj.main.claude-S.a1");
}

#[test]
fn a_role_assigned_at_spawn_in_the_checkout_routes_adds_from_outside_any_repository() {
    let p = Proj::new();
    std::fs::write(p.path.join("devkit.toml"), WORKFLOW).unwrap();
    let out = p.hook("subagent-start", "claude-code", &spawn(&p, "implementer"));
    assert!(out.status.success(), "{}", stderr(&out));
    let add = p.devkit_in(p.outside(), &["todo", "add", "from tmp"], &WORKER, "");
    assert!(add.status.success(), "{}", stderr(&add));
    assert_eq!(p.todos()[0].node(), "proj.main.claude-S.a1");
    assert_eq!(stderr(&add), "");
}

#[test]
fn a_holder_with_no_recorded_checkout_is_told_its_add_goes_on_global() {
    let p = Proj::with_home_config(WORKFLOW);
    let add = p.devkit_in(p.outside(), &["todo", "add", "far"], &WORKER, "");
    assert!(add.status.success(), "{}", stderr(&add));
    assert!(
        stderr(&add).contains("no repository found"),
        "{}",
        stderr(&add)
    );
    assert_eq!(p.todos()[0].project, None);
}

#[test]
fn a_role_the_recorded_checkouts_config_defines_is_not_reported_stale() {
    let p = Proj::new();
    std::fs::write(p.path.join("devkit.toml"), WORKFLOW).unwrap();
    implementer_in_checkout(&p);
    let add = p.devkit_in(p.outside(), &["todo", "add", "from tmp"], &WORKER, "");
    assert!(add.status.success(), "{}", stderr(&add));
    assert!(
        !stderr(&add).contains("no longer exists"),
        "{}",
        stderr(&add)
    );
    assert_eq!(p.todos()[0].node(), "proj.main.claude-S.a1");
}

#[test]
fn a_held_stop_from_outside_any_repository_sees_the_role_nodes_pending_todo() {
    let p = Proj::new();
    std::fs::write(p.path.join("devkit.toml"), WORKFLOW).unwrap();
    implementer_in_checkout(&p);
    let add = p.devkit(&["todo", "add", "finish the worker plan"], &WORKER);
    assert!(add.status.success(), "{}", stderr(&add));
    let mut payload = spawn(&p, "general-purpose");
    payload["cwd"] = json!(p.outside());
    payload["hook_event_name"] = json!("SubagentStop");
    payload["stop_hook_active"] = json!(false);
    let out = p.hook("subagent-stop", "claude-code", &payload);
    assert!(out.status.success(), "{}", stderr(&out));
    let answer: Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", stdout(&out)));
    assert_eq!(answer["decision"], "block");
    assert!(
        answer["reason"]
            .as_str()
            .unwrap()
            .contains("finish the worker plan")
    );
}
