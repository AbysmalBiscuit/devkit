//! `devkit todo` and the todo hooks on the taskchampion backend, chosen by
//! `DEVKIT_TODO_BACKEND` as a container chooses it.

#[path = "common/syncserver.rs"]
mod syncserver;
#[path = "common/todoenv.rs"]
mod todoenv;

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use devkit_todo::{Edit, Filter, Holder, NewTodo, Status, StatusKind, Todo, TodoStore};
use devkit_todo_taskchampion::{SyncTarget, TaskchampionStore, Uuid};
use serde_json::{Value, json};
use syncserver::{Refusing, Silent, SyncServer};
use todoenv::{HeldLock, Proj, stderr, stdout};

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

/// The variables that select the backend and point it at the server `url`.
fn server_env(url: &str) -> [(&str, &str); 4] {
    [
        ("DEVKIT_TODO_BACKEND", "taskchampion"),
        ("DEVKIT_TODO_SYNC_URL", url),
        ("DEVKIT_TODO_SYNC_CLIENT_ID", CLIENT_ID),
        ("DEVKIT_TODO_SYNC_SECRET", SECRET),
    ]
}

/// A replica outside any `Proj`, syncing with the server at `url` as the
/// same client.
fn other_replica(dir: &std::path::Path, url: &str) -> TaskchampionStore {
    TaskchampionStore::at(dir.join("other")).with_target(SyncTarget::Server {
        url: url.to_string(),
        client_id: Uuid::parse_str(CLIENT_ID).unwrap(),
        secret: SECRET.as_bytes().to_vec(),
    })
}

fn descriptions(store: &TaskchampionStore) -> Vec<String> {
    let mut out: Vec<String> = store
        .list(&Filter::all())
        .unwrap()
        .into_iter()
        .map(|t| t.description)
        .collect();
    out.sort();
    out
}

/// Polls `done` until it holds, panicking with `what` after `limit`.
fn poll_until(limit: Duration, what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn add_returns_before_an_unanswering_server() {
    let p = Proj::new();
    let server = Silent::start();
    let started = Instant::now();
    let out = p.devkit(&["todo", "add", "one"], &server_env(&server.url));
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    poll_until(
        Duration::from_secs(30),
        "the background sync to connect",
        || server.connections() > 0,
    );
}

#[test]
fn a_failed_sync_holds_off_background_attempts() {
    let p = Proj::new();
    let server = Refusing::start();
    let env = server_env(&server.url);
    let out = p.devkit(&["todo", "add", "one"], &env);
    assert!(out.status.success(), "{}", stderr(&out));
    let failed = replica_dir(&p).join("sync.failed");
    poll_until(Duration::from_secs(30), "sync.failed", || failed.exists());
    let after_failure = server.accepts();
    assert!(after_failure > 0);

    let out = p.devkit(&["todo", "add", "two"], &env);
    assert!(out.status.success(), "{}", stderr(&out));
    let started = Instant::now();
    let out = p.devkit(&["todo", "sync", "--background"], &env);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(server.accepts(), after_failure);
    assert!(replica_dir(&p).join("sync.pending").exists());
}

#[test]
fn todo_sync_reports_failure_and_exits_zero() {
    let p = Proj::new();
    let server = Refusing::start();
    let out = p.devkit(&["todo", "sync"], &server_env(&server.url));
    assert!(out.status.success(), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("devkit todo: sync failed:"), "{err}");
    assert!(err.contains("changes are saved"), "{err}");
    assert!(!err.contains(SECRET) && !err.contains(CLIENT_ID), "{err}");
}

#[test]
fn a_sync_carries_writes_made_while_it_ran() {
    let p = Proj::new();
    let server = SyncServer::start();
    let env = server_env(&server.url);
    let seeded = p.devkit(&["todo", "add", "first"], &env);
    assert!(seeded.status.success(), "{}", stderr(&seeded));
    let pending = replica_dir(&p).join("sync.pending");
    poll_until(Duration::from_secs(30), "the first sync", || {
        server.versions().len() == 1 && !pending.exists()
    });

    server.hang_on_child_of(None);
    let mut running = p.devkit_child(&["todo", "sync"], &env);
    server.wait_hung(Duration::from_secs(30));
    let mut writer = p.devkit_child(&["todo", "add", "second"], &env);
    server.release();
    assert!(running.wait().unwrap().success());
    assert!(writer.wait().unwrap().success());

    let dir = tempfile::tempdir().unwrap();
    let other = other_replica(dir.path(), &server.url);
    poll_until(
        Duration::from_secs(30),
        "the second todo on the server",
        || {
            other.sync_once().unwrap();
            descriptions(&other) == ["first", "second"]
        },
    );
}

#[test]
fn an_interrupted_sync_rolls_back() {
    let server = SyncServer::start();
    let dir = tempfile::tempdir().unwrap();
    let other = other_replica(dir.path(), &server.url);
    for text in ["one", "two"] {
        other
            .add(NewTodo {
                project: Some("proj.main".into()),
                description: text.into(),
                parent: None,
                order: None,
            })
            .unwrap();
        other.sync_once().unwrap();
    }
    let versions = server.versions();
    assert_eq!(versions.len(), 2);

    let p = Proj::new();
    let local = p.devkit(&["todo", "add", "mine"], &backend("taskchampion"));
    assert!(local.status.success(), "{}", stderr(&local));
    server.hang_on_child_of(Some(&versions[0]));
    let mut sync = p.devkit_child(&["todo", "sync"], &server_env(&server.url));
    server.wait_hung(Duration::from_secs(30));
    sync.kill().unwrap();
    sync.wait().unwrap();
    server.release();

    let mine: Vec<String> = replica(&p).into_iter().map(|t| t.description).collect();
    assert_eq!(mine, ["mine"]);
    let out = p.devkit(&["todo", "sync"], &server_env(&server.url));
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
    let mut all: Vec<String> = replica(&p).into_iter().map(|t| t.description).collect();
    all.sort();
    assert_eq!(all, ["mine", "one", "two"]);
}

#[test]
fn a_hook_gives_up_on_a_held_replica_within_its_budget() {
    let p = Proj::new();
    let held = HeldLock::at(replica_dir(&p).join("devkit.lock"));
    let create = json!({
        "session_id": SESSION,
        "cwd": p.path,
        "hook_event_name": "PostToolUse",
        "tool_name": "TaskCreate",
        "tool_input": {"subject": "alpha", "description": "alpha"},
        "tool_response": {"task": {"id": "1", "subject": "alpha"}},
    });
    let started = Instant::now();
    let out = p.hook_with(
        "post-tool-use",
        "claude-code",
        &create,
        &backend("taskchampion"),
    );
    let took = started.elapsed();
    drop(held);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "");
    assert_eq!(stderr(&out), "");
    assert!(
        took >= Duration::from_millis(1500) && took < Duration::from_secs(4),
        "{took:?}"
    );
    assert!(replica(&p).is_empty());
}

/// A project whose home config syncs the taskchampion replica to
/// `<sync>/server`, and a second replica on the same directory.
struct Shared {
    p: Proj,
    sync: tempfile::TempDir,
}

impl Shared {
    fn new() -> Self {
        let sync = tempfile::tempdir().unwrap();
        let config = format!(
            "[todo]\nbackend = \"taskchampion\"\n[todo.taskchampion]\nserver_dir = \"{}\"\n",
            sync.path().join("server").display()
        );
        Self {
            p: Proj::with_home_config(&config),
            sync,
        }
    }

    fn other(&self) -> TaskchampionStore {
        TaskchampionStore::at(self.sync.path().join("other"))
            .with_target(SyncTarget::Dir(self.sync.path().join("server")))
    }
}

fn add_on(store: &TaskchampionStore, node: &str, text: &str) -> String {
    store
        .add(NewTodo {
            project: Some(node.into()),
            description: text.into(),
            parent: None,
            order: None,
        })
        .unwrap()
}

fn context(p: &Proj, event: &str, env: &[(&str, &str)]) -> std::process::Output {
    let payload = json!({
        "hook_event_name": event,
        "session_id": "s1",
        "cwd": p.path,
        "source": "startup",
    });
    p.devkit_in(
        &p.path,
        &["todo", "context", "--harness", "claude-code"],
        env,
        &payload.to_string(),
    )
}

#[test]
fn session_start_pulls_before_injecting() {
    let shared = Shared::new();
    let other = shared.other();
    add_on(&other, "proj.main", "left by the last container");
    other.sync_once().unwrap();
    let out = context(&shared.p, "SessionStart", &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("left by the last container"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn session_start_is_bounded_when_the_server_hangs() {
    let p = Proj::new();
    let server = Silent::start();
    let started = Instant::now();
    let out = context(&p, "SessionStart", &server_env(&server.url));
    let took = started.elapsed();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(took < Duration::from_secs(22), "{took:?}");
    assert!(
        stdout(&out).contains("Todo list, kept by devkit."),
        "{}",
        stdout(&out)
    );
    assert!(server.connections() > 0);
}

#[test]
fn user_prompt_context_never_syncs() {
    let shared = Shared::new();
    let out = context(&shared.p, "UserPromptSubmit", &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!replica_dir(&shared.p).join("sync.lock").exists());
}

#[test]
fn list_sync_pulls_and_plain_list_does_not() {
    let shared = Shared::new();
    let other = shared.other();
    add_on(&other, "proj.main", "from elsewhere");
    other.sync_once().unwrap();
    let plain = shared.p.devkit(&["todo", "list", "--all"], &[]);
    assert!(plain.status.success(), "{}", stderr(&plain));
    assert!(
        !stdout(&plain).contains("from elsewhere"),
        "{}",
        stdout(&plain)
    );
    let synced = shared.p.devkit(&["todo", "list", "--all", "--sync"], &[]);
    assert!(synced.status.success(), "{}", stderr(&synced));
    assert!(
        stdout(&synced).contains("from elsewhere"),
        "{}",
        stdout(&synced)
    );
}

#[test]
fn list_sync_reports_a_failed_sync_and_lists_the_replica() {
    let p = Proj::new();
    let server = Refusing::start();
    let env = server_env(&server.url);
    let local = p.devkit(&["todo", "add", "kept here"], &backend("taskchampion"));
    assert!(local.status.success(), "{}", stderr(&local));
    let out = p.devkit(&["todo", "list", "--all", "--sync"], &env);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("devkit todo: sync failed:"),
        "{}",
        stderr(&out)
    );
    assert!(stdout(&out).contains("kept here"), "{}", stdout(&out));
}

#[test]
fn session_end_pushes_the_release() {
    let shared = Shared::new();
    let here = TaskchampionStore::at(replica_dir(&shared.p))
        .with_target(SyncTarget::Dir(shared.sync.path().join("server")));
    let id = add_on(&here, "proj.main.claude-s1", "claimed");
    here.apply(&Edit::SetStatus {
        id: id.clone(),
        to: StatusKind::InProgress,
        actor: Holder::new("s1"),
    })
    .unwrap();
    here.sync_once().unwrap();
    let other = shared.other();
    other.sync_once().unwrap();
    assert!(matches!(
        other.get(&id).unwrap().unwrap().status,
        Status::InProgress { .. }
    ));

    let out = shared.p.hook(
        "session-end",
        "claude-code",
        &json!({"session_id": "s1", "cwd": shared.p.path}),
    );
    assert!(out.status.success(), "{}", stderr(&out));
    other.sync_once().unwrap();
    assert_eq!(other.get(&id).unwrap().unwrap().status, Status::Pending);
}

#[test]
fn the_session_end_timeout_allows_the_push() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin/hooks");
    for manifest in ["hooks.json", "hooks-codex.json"] {
        let text = std::fs::read_to_string(root.join(manifest)).unwrap();
        let hooks: Value = serde_json::from_str(&text).unwrap();
        let groups = hooks["hooks"]["SessionEnd"].as_array().unwrap();
        let timeouts: Vec<&Value> = groups
            .iter()
            .flat_map(|g| g["hooks"].as_array().unwrap())
            .map(|h| &h["timeout"])
            .collect();
        assert!(!timeouts.is_empty(), "{manifest}");
        assert!(
            timeouts.iter().all(|t| **t == 25),
            "{manifest}: {timeouts:?}"
        );
    }
}
