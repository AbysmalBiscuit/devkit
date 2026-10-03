//! `devkit todo` and the todo hooks on the taskchampion backend, chosen by
//! `DEVKIT_TODO_BACKEND` as a container chooses it.

#[path = "common/todoenv.rs"]
mod todoenv;

use std::path::PathBuf;

use devkit_todo::{Filter, Todo, TodoStore};
use devkit_todo_taskchampion::TaskchampionStore;
use serde_json::{Value, json};
use todoenv::{Proj, stderr, stdout};

const SESSION: &str = "732b6b74-6009-478a-abe2-4129415b6007";
const CLIENT_ID: &str = "0b8d4c2e-5f6a-4b7c-8d9e-0f1a2b3c4d5e";
const SECRET: &str = "s3cret-value-never-shown";

fn replica_dir(p: &Proj) -> PathBuf {
    p.state().join("todo/taskchampion")
}

fn replica(p: &Proj) -> Vec<Todo> {
    TaskchampionStore::at(replica_dir(p))
        .list(&Filter::all())
        .unwrap()
}

fn backend(name: &str) -> [(&str, &str); 1] {
    [("DEVKIT_TODO_BACKEND", name)]
}

#[test]
fn the_env_selects_taskchampion_with_no_config() {
    let p = Proj::new();
    let out = p.devkit(&["todo", "add", "one"], &backend("taskchampion"));
    assert!(out.status.success(), "{}", stderr(&out));
    let id = stdout(&out).trim().to_string();
    assert_eq!(id.len(), 8, "{id}");
    assert!(id.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
    let todos = replica(&p);
    assert_eq!(todos.len(), 1, "{todos:?}");
    assert!(todos[0].id.starts_with(&id));
    assert!(p.todos().is_empty());
}

#[test]
fn the_env_beats_the_home_config() {
    let p = Proj::with_home_config(
        "[todo]\nbackend = \"taskwarrior\"\n[todo.taskwarrior]\npath = \"/nonexistent/task\"\n",
    );
    let out = p.devkit(&["todo", "add", "one"], &backend("taskchampion"));
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(replica(&p).len(), 1);
}

#[test]
fn the_replica_files_todos_under_the_taskwarrior_root() {
    let p = Proj::with_home_config("[todo.taskwarrior]\nproject = \"agents\"\n");
    let out = p.devkit(&["todo", "add", "one"], &backend("taskchampion"));
    assert!(out.status.success(), "{}", stderr(&out));
    let under_agents = TaskchampionStore::at(replica_dir(&p)).with_root("agents");
    assert_eq!(under_agents.list(&Filter::all()).unwrap().len(), 1);
    assert!(replica(&p).is_empty());
}

fn doctor_rows(p: &Proj, env: &[(&str, &str)]) -> Vec<Value> {
    let out = p.devkit(&["doctor", "--json"], env);
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}{}", stdout(&out), stderr(&out)))
}

fn row<'a>(rows: &'a [Value], key: &str) -> &'a Value {
    rows.iter()
        .find(|r| r["key"] == key)
        .unwrap_or_else(|| panic!("no {key} row in {rows:?}"))
}

#[test]
fn doctor_reports_the_backend_and_its_source() {
    let p = Proj::new();
    let env = [
        ("DEVKIT_TODO_BACKEND", "taskchampion"),
        ("DEVKIT_TODO_SYNC_URL", "https://sync.invalid"),
        ("DEVKIT_TODO_SYNC_CLIENT_ID", CLIENT_ID),
        ("DEVKIT_TODO_SYNC_SECRET", SECRET),
    ];
    let rows = doctor_rows(&p, &env);
    let backend = row(&rows, "todo_backend");
    assert_eq!(backend["source"], "env", "{backend}");
    let detail = backend["detail"].as_str().unwrap();
    assert!(detail.contains("taskchampion"), "{detail}");
    assert!(detail.contains("DEVKIT_TODO_BACKEND"), "{detail}");
    for key in [
        "devkit_todo_sync_url",
        "devkit_todo_sync_client_id",
        "devkit_todo_sync_secret",
    ] {
        assert_eq!(row(&rows, key)["source"], "env", "{key}");
    }
    let text = serde_json::to_string(&rows).unwrap();
    for value in [SECRET, CLIENT_ID, "sync.invalid"] {
        assert!(!text.contains(value), "{value} leaked: {text}");
    }
}

#[test]
fn doctor_reports_a_default_backend_and_no_credentials() {
    let p = Proj::new();
    let rows = doctor_rows(&p, &[]);
    let backend = row(&rows, "todo_backend");
    assert!(backend["detail"].as_str().unwrap().contains("builtin"));
    assert!(!rows.iter().any(|r| r["key"] == "devkit_todo_sync_url"));
}

#[test]
fn a_misspelled_backend_fails_the_cli_and_not_hooks() {
    let p = Proj::new();
    let env = backend("taskchampio");
    let out = p.devkit(&["todo", "list"], &env);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("DEVKIT_TODO_BACKEND"),
        "{}",
        stderr(&out)
    );

    let create = json!({
        "session_id": SESSION,
        "cwd": p.path,
        "hook_event_name": "PostToolUse",
        "tool_name": "TaskCreate",
        "tool_input": {"subject": "alpha", "description": "alpha"},
        "tool_response": {"task": {"id": "1", "subject": "alpha"}},
    });
    let out = p.hook_with("post-tool-use", "claude-code", &create, &env);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert_eq!(p.todos().len(), 1);
    assert!(!replica_dir(&p).exists());
}

#[test]
fn a_half_set_server_config_names_what_is_missing() {
    let p = Proj::new();
    let env = [
        ("DEVKIT_TODO_BACKEND", "taskchampion"),
        ("DEVKIT_TODO_SYNC_URL", "https://sync.invalid"),
        ("DEVKIT_TODO_SYNC_SECRET", SECRET),
    ];
    let out = p.devkit(&["todo", "list"], &env);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("DEVKIT_TODO_SYNC_CLIENT_ID"), "{err}");
    assert!(!err.contains(SECRET), "{err}");
}
