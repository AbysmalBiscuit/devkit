#[path = "common/todoenv.rs"]
mod todoenv;

use devkit_todo::{NewTodo, TodoStore};
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

const MAIN: [(&str, &str); 2] = [("DEVKIT_CALLER", "agent"), ("CLAUDE_CODE_SESSION_ID", "S")];

#[test]
fn selecting_manager_changes_the_callers_write_node() {
    let p = Proj::with_home_config(WORKFLOW);
    let role = p.devkit(&["todo", "role", "manager"], &MAIN);
    assert!(role.status.success(), "{}", stderr(&role));
    assert!(stdout(&role).contains("manager"), "{}", stdout(&role));
    assert!(stdout(&role).contains("proj.main"), "{}", stdout(&role));
    let scope = p.devkit(&["todo", "scope"], &MAIN);
    assert_eq!(stdout(&scope), "proj.main\n");
    let add = p.devkit(&["todo", "add", "manage the workspace"], &MAIN);
    assert!(add.status.success(), "{}", stderr(&add));
    assert_eq!(p.todos()[0].node(), "proj.main");
}

#[test]
fn a_worker_can_write_and_list_a_named_scope() {
    let p = Proj::with_home_config(WORKFLOW);
    let worker = [MAIN[0], MAIN[1], ("DEVKIT_TODO_HOLDER", "S/a1")];
    let role = p.devkit(&["todo", "role", "implementer"], &worker);
    assert!(role.status.success(), "{}", stderr(&role));
    let own = p.devkit(&["todo", "add", "worker plan"], &worker);
    assert!(own.status.success(), "{}", stderr(&own));
    let finding = p.devkit(
        &["todo", "add", "manager finding", "--scope", "workspace"],
        &worker,
    );
    assert!(finding.status.success(), "{}", stderr(&finding));
    let listed = p.devkit(&["todo", "list", "--scope", "workspace"], &worker);
    assert!(listed.status.success(), "{}", stderr(&listed));
    assert!(
        stdout(&listed).contains("manager finding"),
        "{}",
        stdout(&listed)
    );
    assert!(
        !stdout(&listed).contains("worker plan"),
        "{}",
        stdout(&listed)
    );
    assert_eq!(p.todos()[0].node(), "proj.main.claude-S.a1");
    assert_eq!(p.todos()[1].node(), "proj.main");
    let conflict = p.devkit(
        &[
            "todo",
            "add",
            "invalid",
            "--scope",
            "workspace",
            "--node",
            "raw",
        ],
        &worker,
    );
    assert!(!conflict.status.success());
    let unknown = p.devkit(&["todo", "add", "invalid", "--scope", "nope"], &worker);
    assert!(!unknown.status.success());
    assert!(
        stderr(&unknown).contains("valid scopes"),
        "{}",
        stderr(&unknown)
    );
}

#[test]
fn manager_listing_includes_only_its_session_descendants() {
    let p = Proj::with_home_config(WORKFLOW);
    assert!(
        p.devkit(&["todo", "role", "manager"], &MAIN)
            .status
            .success()
    );
    for (node, text) in [
        ("global", "global ancestor"),
        ("proj", "repo ancestor"),
        ("proj.main", "workspace plan"),
        ("proj.main.claude-S.a1", "own worker"),
        ("proj.main.claude-T.a1", "sibling worker"),
    ] {
        p.store()
            .add(NewTodo {
                project: Some(node.into()),
                description: text.into(),
                parent: None,
                order: None,
            })
            .unwrap();
    }
    let out = p.devkit(&["todo", "list"], &MAIN);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    for expected in [
        "global ancestor",
        "repo ancestor",
        "workspace plan",
        "own worker",
    ] {
        assert!(text.contains(expected), "{text}");
    }
    assert!(!text.contains("sibling worker"), "{text}");
}

#[test]
fn scans_use_anchors_outside_the_requested_subtree() {
    let p = Proj::new();
    for (node, text) in [
        ("global", "global todo"),
        ("proj", "anchored repo"),
        ("proj.main", "anchored workspace"),
        ("proj.other.claude-S", "anchor elsewhere"),
        ("ideas", "personal"),
        ("ideas.weekend", "personal child"),
        ("proj.malformed.extra", "malformed"),
        ("devkit.proj.main.claude-S", "legacy"),
    ] {
        p.store()
            .add(NewTodo {
                project: Some(node.into()),
                description: text.into(),
                parent: None,
                order: None,
            })
            .unwrap();
    }
    let all = p.devkit(&["todo", "list", "--all"], &MAIN);
    assert!(all.status.success(), "{}", stderr(&all));
    let text = stdout(&all);
    for expected in [
        "global todo",
        "anchored repo",
        "anchored workspace",
        "anchor elsewhere",
    ] {
        assert!(text.contains(expected), "{text}");
    }
    for excluded in ["personal", "malformed", "legacy"] {
        assert!(!text.contains(excluded), "{text}");
    }
    let subtree = p.devkit(&["todo", "list", "--subtree", "proj.main"], &MAIN);
    assert!(
        stdout(&subtree).contains("anchored workspace"),
        "{}",
        stdout(&subtree)
    );
    let raw = p.devkit(&["todo", "list", "--node", "ideas"], &MAIN);
    assert!(stdout(&raw).contains("personal"), "{}", stdout(&raw));
}

#[test]
fn role_selection_lists_names_and_refuses_terminal_callers() {
    let p = Proj::with_home_config(WORKFLOW);
    let unknown = p.devkit(&["todo", "role", "nope"], &MAIN);
    assert!(!unknown.status.success());
    for name in ["main", "subagent", "manager", "implementer", "reviewer"] {
        assert!(stderr(&unknown).contains(name), "{}", stderr(&unknown));
    }
    for env in [&[][..], &[("DEVKIT_CALLER", "human"), MAIN[1]][..]] {
        let terminal = p.devkit(&["todo", "role", "manager"], env);
        assert!(!terminal.status.success());
        assert!(
            stderr(&terminal).contains("agent session"),
            "{}",
            stderr(&terminal)
        );
    }
}

#[test]
fn a_stale_role_warns_once_per_command_and_keeps_writing() {
    let p = Proj::with_home_config(WORKFLOW);
    assert!(
        p.devkit(&["todo", "role", "manager"], &MAIN)
            .status
            .success()
    );
    std::fs::write(p.home_config(), "[todo]\n").unwrap();
    let add = p.devkit(&["todo", "add", "resume with defaults"], &MAIN);
    assert!(add.status.success(), "{}", stderr(&add));
    assert_eq!(stderr(&add).matches("no longer exists").count(), 1);
    assert_eq!(p.todos()[0].node(), "proj.main.claude-S");
}

#[test]
fn invalid_layout_reaches_cli_and_doctor_with_the_same_error() {
    let p = Proj::with_home_config(
        "[todo.scopes]\nbroken = {node = \"{missing}\", parent = \"global\"}\n",
    );
    let cli = p.devkit(&["todo", "scope"], &MAIN);
    assert!(!cli.status.success());
    assert!(
        stderr(&cli).contains("unknown placeholder"),
        "{}",
        stderr(&cli)
    );
    let doctor = p.devkit(&["doctor", "--json"], &[]);
    let text = format!("{}{}", stdout(&doctor), stderr(&doctor));
    assert!(text.contains("unknown placeholder"), "{text}");
}

#[test]
fn default_sections_follow_scope_ancestry_even_for_a_deep_literal_root() {
    let p = Proj::with_home_config("[todo.scopes.global]\nnode = \"all.todos.archive\"\n");
    for (node, text) in [
        ("all.todos.archive", "root"),
        ("proj", "repository"),
        ("proj.main", "workspace"),
    ] {
        p.store()
            .add(NewTodo {
                project: Some(node.into()),
                description: text.into(),
                parent: None,
                order: None,
            })
            .unwrap();
    }
    let out = p.devkit(&["todo", "list"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.find("## proj.main\n").unwrap() < text.find("## proj\n").unwrap(),
        "{text}"
    );
    assert!(
        text.find("## proj\n").unwrap() < text.find("## all.todos.archive\n").unwrap(),
        "{text}"
    );
    let payload =
        serde_json::json!({"session_id": "S", "cwd": p.path, "hook_event_name": "SessionStart"});
    let out = p.devkit_in(
        &p.path,
        &["todo", "context", "--harness", "claude-code"],
        &[],
        &payload.to_string(),
    );
    let answer: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let text = answer["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        text.find("## proj.main\n").unwrap() < text.find("## proj\n").unwrap(),
        "{text}"
    );
    assert!(
        text.find("## proj\n").unwrap() < text.find("## all.todos.archive\n").unwrap(),
        "{text}"
    );
}
