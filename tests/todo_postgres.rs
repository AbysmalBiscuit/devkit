//! `devkit todo`, the todo hooks, `devkit activity` and `devkit doctor` on the
//! Postgres backend. Tests that need a database read
//! `DEVKIT_TEST_POOLER_URL`, the database behind a transaction-mode pooler, or
//! else `DEVKIT_TEST_POSTGRES_URL`, and return early when neither is set;
//! `crates/devkit-todo-postgres/testdb/up.sh` starts both.

#[path = "common/syncserver.rs"]
mod syncserver;
#[path = "common/todoenv.rs"]
mod todoenv;

use std::{
    process::Output,
    time::{Duration, Instant, SystemTime},
};

use devkit_todo::{
    Filter, Status, TodoStore,
    activity::{ActivityStore, ClaimEnd, RunEnd},
};
use devkit_todo_postgres::{Database, PostgresActivity, PostgresStore};
use serde_json::{Value, json};
use syncserver::Silent;
use todoenv::{Proj, stderr, stdout};

const DATABASE_VAR: &str = "DEVKIT_TODO_DATABASE_URL";

/// The time a hook gets: the lock budget every hook keeps to.
const HOOK_BUDGET: Duration = Duration::from_secs(2);

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// The database the tests use, through the pooler when there is one.
fn test_url() -> Option<String> {
    var("DEVKIT_TEST_POOLER_URL").or_else(|| var("DEVKIT_TEST_POSTGRES_URL"))
}

/// A todo root no other test shares.
fn fresh_root() -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("e2e-{}-{nanos}", std::process::id())
}

/// A checkout whose todos go to `root` on the Postgres backend.
fn proj(root: &str) -> Proj {
    Proj::with_home_config(&format!(
        "[todo]\nbackend = \"postgres\"\nproject = \"{root}\"\n"
    ))
}

fn store(url: &str, root: &str) -> PostgresStore {
    PostgresStore::new(Database::new(url, Duration::from_secs(10)).unwrap(), root)
}

fn session(id: &str, url: &str) -> Vec<(&'static str, String)> {
    vec![
        ("CLAUDE_CODE_SESSION_ID", id.to_string()),
        (DATABASE_VAR, url.to_string()),
    ]
}

fn borrowed<'a>(env: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    env.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

fn devkit(p: &Proj, args: &[&str], env: &[(&'static str, String)]) -> Output {
    p.devkit(args, &borrowed(env))
}

fn added(p: &Proj, text: &str, env: &[(&'static str, String)]) -> String {
    let out = devkit(p, &["todo", "add", text], env);
    assert!(out.status.success(), "{}", stderr(&out));
    stdout(&out).trim().to_string()
}

#[test]
fn concurrent_claims_have_one_winner() {
    let Some(url) = test_url() else {
        return;
    };
    let root = fresh_root();
    let p = proj(&root);
    let id = added(&p, "contended", &session("s0", &url));
    let claimants: Vec<String> = (0..16).map(|i| format!("s{i}")).collect();
    let children: Vec<_> = claimants
        .iter()
        .map(|s| {
            let env = session(s, &url);
            p.devkit_child(&["todo", "start", &id], &borrowed(&env))
        })
        .collect();
    let outcomes: Vec<(String, Output)> = claimants
        .into_iter()
        .zip(children)
        .map(|(s, child)| (s, child.wait_with_output().unwrap()))
        .collect();
    let winners: Vec<&str> = outcomes
        .iter()
        .filter(|(_, out)| out.status.success())
        .map(|(s, _)| s.as_str())
        .collect();
    assert_eq!(winners.len(), 1, "{outcomes:#?}");
    let winner = winners[0];
    for (s, out) in outcomes.iter().filter(|(s, _)| s != winner) {
        let err = stderr(out);
        assert!(
            err.contains(&format!("in progress by {winner}")),
            "{s}: {err}"
        );
    }
    let todos = store(&url, &root).list(&Filter::all()).unwrap();
    assert_eq!(todos.len(), 1, "{todos:?}");
    assert_eq!(todos[0].status, Status::InProgress {
        by: devkit_todo::Holder::new(winner)
    });
}

fn subagent(p: &Proj, event: &str, agent: &str) -> Value {
    json!({
        "hook_event_name": event,
        "session_id": "S",
        "agent_id": agent,
        "agent_type": "Explore",
        "cwd": p.path,
    })
}

#[test]
fn activity_lands_in_the_database() {
    let Some(url) = test_url() else {
        return;
    };
    let root = fresh_root();
    let p = proj(&root);
    let env = session("S", &url);
    let hook = |verb: &str, event: &str| {
        let out = p.hook_with(
            verb,
            "claude-code",
            &subagent(&p, event, "a1"),
            &borrowed(&env),
        );
        assert!(out.status.success(), "{verb}: {}", stderr(&out));
    };
    hook("subagent-start", "SubagentStart");
    let id = added(&p, "a", &env);
    for verb in ["start", "done"] {
        let out = devkit(&p, &["todo", verb, &id], &env);
        assert!(out.status.success(), "{verb}: {}", stderr(&out));
    }
    hook("subagent-stop", "SubagentStop");

    let db = Database::new(&url, Duration::from_secs(10)).unwrap();
    let activity = PostgresActivity::new(db, &root)
        .read(SystemTime::now().into())
        .unwrap();
    assert_eq!(activity.runs.len(), 1, "{activity:?}");
    let run = &activity.runs[0];
    assert_eq!(
        (run.agent.as_str(), run.outcome),
        ("a1", Some(RunEnd::Stopped))
    );
    assert_eq!(activity.claims.len(), 1, "{activity:?}");
    let claim = &activity.claims[0];
    assert!(claim.todo.starts_with(&id), "{claim:?}");
    assert_eq!(claim.outcome, Some(ClaimEnd::Completed));
    assert!(
        !p.state().join("todo/activity/events.jsonl").exists(),
        "nothing recorded locally"
    );

    let out = devkit(&p, &["activity", "--json"], &env);
    assert!(out.status.success(), "{}", stderr(&out));
    let report = stdout(&out);
    assert!(report.contains("Explore"), "{report}");
}

/// The address of a server that accepts a connection and never answers.
fn silent_url(silent: &Silent) -> String {
    let addr = silent
        .url
        .trim_start_matches("http://")
        .trim_end_matches('/');
    format!("postgres://agent:hunter2@{addr}/todos")
}

#[test]
fn a_hook_with_the_database_unreachable_does_nothing_in_time() {
    let silent = Silent::start();
    let p = proj("devkit");
    let env = [
        (DATABASE_VAR, silent_url(&silent)),
        ("CLAUDE_CODE_SESSION_ID", "S".to_string()),
    ];
    let env = borrowed(&env);
    let timed = |args: &[&str], payload: Value| {
        let started = Instant::now();
        let out = p.devkit_in(&p.path, args, &env, &payload.to_string());
        let took = started.elapsed();
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        assert!(took < HOOK_BUDGET, "{args:?} took {took:?}");
        out
    };
    let hook = |verb| ["hook", verb, "--harness", "claude-code"];

    let start = json!({"hook_event_name": "SessionStart", "session_id": "S", "cwd": p.path, "source": "startup"});
    let injected = timed(&["todo", "context", "--harness", "claude-code"], start);
    assert_eq!(stdout(&injected), "", "nothing injected");

    let command = "devkit todo start abcdef12";
    let bash = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "S",
        "agent_id": "a1",
        "agent_type": "general-purpose",
        "tool_name": "Bash",
        "tool_input": {"command": command},
        "cwd": p.path,
    });
    let verdict = stdout(&timed(&hook("pre-tool-use"), bash));
    assert!(!verdict.contains("\"deny\""), "{verdict}");
    assert!(
        verdict.contains("DEVKIT_TODO_HOLDER='S/a1' devkit todo start abcdef12"),
        "the verdict is the attribution a reachable database gets: {verdict}"
    );

    let create = json!({
        "hook_event_name": "PostToolUse",
        "session_id": "S",
        "tool_name": "TaskCreate",
        "tool_input": {"subject": "alpha"},
        "tool_response": {"task": {"id": "1", "subject": "alpha"}},
        "cwd": p.path,
    });
    assert_eq!(stdout(&timed(&hook("post-tool-use"), create)), "");
    assert_eq!(
        stdout(&timed(
            &hook("subagent-stop"),
            subagent(&p, "SubagentStop", "a1")
        )),
        ""
    );

    assert!(silent.connections() > 0, "the hooks tried the database");
    assert!(p.todos().is_empty(), "nothing fell back to the local store");
    assert!(!p.state().join("todo/activity/events.jsonl").exists());
}

/// Runs `sql` on the database `url` names, outside any transaction.
fn admin(url: &str, sql: &str) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(connection);
        client.batch_execute(sql).await.unwrap();
    });
}

/// `url` naming the database `name` on the same server.
fn with_dbname(url: &str, name: &str) -> String {
    let (server, _) = url.rsplit_once('/').unwrap();
    format!("{server}/{name}")
}

#[test]
fn creates_its_schema_on_an_empty_database_and_starts_on_an_existing_one() {
    let Some(direct) = var("DEVKIT_TEST_POSTGRES_URL") else {
        return;
    };
    let name = format!("devkit_{}", fresh_root().replace('-', "_"));
    admin(&direct, &format!("CREATE DATABASE {name}"));
    let url = with_dbname(&test_url().unwrap(), &name);
    let p = proj("devkit");
    let env = session("S", &url);

    let id = added(&p, "first", &env);
    let listed = devkit(&p, &["todo", "list", "--all"], &env);
    assert!(listed.status.success(), "{}", stderr(&listed));
    assert!(stdout(&listed).contains("first"), "{}", stdout(&listed));
    let out = devkit(&p, &["todo", "start", &id], &env);
    assert!(out.status.success(), "{}", stderr(&out));

    admin(&direct, &format!("DROP DATABASE {name} WITH (FORCE)"));
}

fn doctor_rows(p: &Proj, env: &[(&'static str, String)]) -> Vec<Value> {
    let out = devkit(p, &["doctor", "--json"], env);
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}{}", stdout(&out), stderr(&out)))
}

fn row(rows: &[Value], key: &str) -> Value {
    rows.iter()
        .find(|r| r["key"] == key)
        .cloned()
        .unwrap_or_else(|| panic!("no {key} row in {rows:?}"))
}

#[test]
fn doctor_shows_the_backend_and_an_unreachable_database() {
    let silent = Silent::start();
    let p = proj("devkit");
    let rows = doctor_rows(&p, &[(DATABASE_VAR, silent_url(&silent))]);
    let backend = row(&rows, "todo_backend");
    assert!(
        backend["detail"].as_str().unwrap().contains("postgres"),
        "{backend}"
    );
    let database = row(&rows, "todo_database");
    let text = database.to_string();
    assert_eq!(database["source"], "env", "{text}");
    assert!(text.contains("unreachable"), "{text}");
    assert!(
        !text.contains("hunter2") && !text.contains("agent:"),
        "{text}"
    );
}

#[test]
fn doctor_shows_a_reachable_database() {
    let Some(url) = test_url() else {
        return;
    };
    let p = proj("devkit");
    let database = row(&doctor_rows(&p, &[(DATABASE_VAR, url)]), "todo_database");
    assert_eq!(database["status"], "ok", "{database}");
    assert!(
        database["detail"].as_str().unwrap().contains("reachable"),
        "{database}"
    );
}

#[test]
fn doctor_names_the_variable_when_no_url_resolves() {
    let p = proj("devkit");
    let database = row(&doctor_rows(&p, &[]), "todo_database");
    assert_eq!(database["status"], "invalid", "{database}");
    assert!(
        database["detail"].as_str().unwrap().contains(DATABASE_VAR),
        "{database}"
    );
}
