//! `devkit todo`, the todo hooks, `devkit activity` and `devkit doctor` on the
//! Postgres backend. Tests that need a database read
//! `DEVKIT_TEST_POOLER_URL`, the database behind a transaction-mode pooler, or
//! else `DEVKIT_TEST_POSTGRES_URL`, and return early when neither is set;
//! `crates/devkit-todo-postgres/testdb/up.sh` starts both.

#[path = "common/pgstall.rs"]
mod pgstall;
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
use devkit_todo_postgres::{Database, PostgresActivity, PostgresStore, Trust};
use pgstall::Stalled;
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
    PostgresStore::new(
        Database::new(url, Duration::from_secs(10), &Trust::default()).unwrap(),
        root,
    )
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

    let db = Database::new(&url, Duration::from_secs(10), &Trust::default()).unwrap();
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
    hooks_do_nothing_in_time(&silent_url(&silent));
    assert!(silent.connections() > 0, "the hooks tried the database");
}

#[test]
fn a_hook_with_the_database_stalled_after_connecting_does_nothing_in_time() {
    let stalled = Stalled::start();
    hooks_do_nothing_in_time(&format!(
        "postgres://agent@{}/todos?sslmode=disable",
        stalled.addr
    ));
}

/// Every todo hook against the database at `url`: each exits within its
/// budget with the verdict a reachable database gets, and nothing is
/// written anywhere else.
fn hooks_do_nothing_in_time(url: &str) {
    let p = proj("devkit");
    let env = [
        (DATABASE_VAR, url.to_string()),
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

/// `url` naming the database `name` on the same server, its query kept.
fn with_dbname(url: &str, name: &str) -> String {
    let (base, query) = url
        .split_once('?')
        .map_or((url, None), |(b, q)| (b, Some(q)));
    let (server, _) = base.rsplit_once('/').unwrap();
    match query {
        Some(query) => format!("{server}/{name}?{query}"),
        None => format!("{server}/{name}"),
    }
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

#[test]
fn session_end_finishes_its_database_work_inside_the_budget() {
    let Some(direct) = var("DEVKIT_TEST_POSTGRES_URL") else {
        return;
    };
    let name = format!("devkit_{}", fresh_root().replace('-', "_"));
    admin(&direct, &format!("CREATE DATABASE {name}"));
    let url = with_dbname(&test_url().unwrap(), &name);
    let p = proj("devkit");
    let env = session("S", &url);
    let id = added(&p, "held", &env);
    let out = devkit(&p, &["todo", "start", &id], &env);
    assert!(out.status.success(), "{}", stderr(&out));
    // Every write the session's end makes now answers just inside a hook's
    // wait for one call, and all of them together well past the budget.
    admin(
        &with_dbname(&direct, &name),
        "CREATE FUNCTION devkit.slow() RETURNS trigger LANGUAGE plpgsql AS
             $$ BEGIN PERFORM pg_sleep(0.7); RETURN NULL; END $$;
         CREATE TRIGGER slow AFTER UPDATE ON devkit.todos
             FOR EACH STATEMENT EXECUTE FUNCTION devkit.slow();
         CREATE TRIGGER slow AFTER INSERT ON devkit.activity
             FOR EACH STATEMENT EXECUTE FUNCTION devkit.slow();
         CREATE TRIGGER slow AFTER DELETE ON devkit.seen
             FOR EACH STATEMENT EXECUTE FUNCTION devkit.slow();",
    );
    let end = json!({"hook_event_name": "SessionEnd", "session_id": "S", "cwd": p.path});
    let started = Instant::now();
    let out = p.hook_with("session-end", "claude-code", &end, &borrowed(&env));
    let took = started.elapsed();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(took < HOOK_BUDGET, "session-end took {took:?}");

    admin(&direct, &format!("DROP DATABASE {name} WITH (FORCE)"));
}

/// A `doppler` that counts its calls in `calls` and gives `url` as the
/// database URL, and the `PATH` that finds it first.
#[cfg(unix)]
fn fake_doppler(dir: &std::path::Path, url: &str) -> String {
    slow_doppler(dir, url, 0)
}

/// [`fake_doppler`], answering after `seconds`.
#[cfg(unix)]
fn slow_doppler(dir: &std::path::Path, url: &str, seconds: u32) -> String {
    use std::os::unix::fs::PermissionsExt;
    let doppler = dir.join("doppler");
    let body = json!({DATABASE_VAR: {"computed": url}}).to_string();
    let script = format!(
        "#!/bin/sh\necho call >> '{}'\nsleep {seconds}\necho '{body}'\n",
        dir.join("calls").display()
    );
    std::fs::write(&doppler, script).unwrap();
    std::fs::set_permissions(&doppler, std::fs::Permissions::from_mode(0o755)).unwrap();
    format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

#[cfg(unix)]
fn doppler_calls(dir: &std::path::Path) -> usize {
    std::fs::read_to_string(dir.join("calls"))
        .map(|calls| calls.lines().count())
        .unwrap_or(0)
}

#[cfg(unix)]
fn doppler_proj(root: &str) -> Proj {
    Proj::with_home_config(&format!(
        "[todo]\nbackend = \"postgres\"\nproject = \"{root}\"\n\
         [todo.postgres]\ndoppler_project = \"swarm\"\n"
    ))
}

#[cfg(unix)]
#[test]
fn hooks_reuse_the_url_doppler_gave() {
    use std::os::unix::fs::PermissionsExt;
    let Some(url) = test_url() else {
        return;
    };
    let root = fresh_root();
    let p = doppler_proj(&root);
    let bin = tempfile::tempdir().unwrap();
    let path = fake_doppler(bin.path(), &url);
    let env = [("PATH", path.as_str())];
    for agent in ["a1", "a2", "a3"] {
        let out = p.hook_with(
            "subagent-start",
            "claude-code",
            &subagent(&p, "SubagentStart", agent),
            &env,
        );
        assert!(out.status.success(), "{}", stderr(&out));
    }
    assert_eq!(doppler_calls(bin.path()), 1);
    let runs = PostgresActivity::new(
        Database::new(&url, Duration::from_secs(10), &Trust::default()).unwrap(),
        &root,
    )
    .read(SystemTime::now().into())
    .unwrap()
    .runs;
    assert_eq!(runs.len(), 3, "{runs:?}");
    let cached = std::fs::read_dir(p.state().join("todo"))
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().contains("database-url"))
        .expect("the URL is cached under the state directory");
    let mode = cached.metadata().unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");
}

#[cfg(unix)]
#[test]
fn a_hook_that_cannot_connect_asks_doppler_again() {
    let refused = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = refused.local_addr().unwrap();
    drop(refused);
    let p = doppler_proj("devkit");
    let bin = tempfile::tempdir().unwrap();
    let path = fake_doppler(bin.path(), &format!("postgres://agent:pw@{addr}/todos"));
    let env = [("PATH", path.as_str())];
    for agent in ["a1", "a2"] {
        let out = p.hook_with(
            "subagent-start",
            "claude-code",
            &subagent(&p, "SubagentStart", agent),
            &env,
        );
        assert!(out.status.success(), "{}", stderr(&out));
    }
    assert_eq!(doppler_calls(bin.path()), 2);
}

#[cfg(unix)]
fn cached_url_file(p: &Proj) -> std::path::PathBuf {
    p.state().join("todo/database-url.json")
}

#[cfg(unix)]
#[test]
fn a_malformed_doppler_url_is_not_kept() {
    let p = doppler_proj("devkit");
    let bin = tempfile::tempdir().unwrap();
    let path = fake_doppler(bin.path(), "postgres://agent:pw@host:notaport/todos");
    let env = [("PATH", path.as_str())];
    let out = p.hook_with(
        "subagent-start",
        "claude-code",
        &subagent(&p, "SubagentStart", "a1"),
        &env,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(doppler_calls(bin.path()), 1);
    assert!(!cached_url_file(&p).exists());
}

#[cfg(unix)]
#[test]
fn session_end_never_waits_on_doppler() {
    let p = doppler_proj("devkit");
    let bin = tempfile::tempdir().unwrap();
    let path = slow_doppler(bin.path(), "postgres://agent:pw@127.0.0.1:1/todos", 4);
    let env = [("PATH", path.as_str())];
    let end = json!({"hook_event_name": "SessionEnd", "session_id": "S", "cwd": p.path});
    let started = Instant::now();
    let out = p.hook_with("session-end", "claude-code", &end, &env);
    let took = started.elapsed();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(took < HOOK_BUDGET, "session-end took {took:?}");
    assert_eq!(doppler_calls(bin.path()), 0);
}

#[cfg(unix)]
#[test]
fn session_end_releases_through_a_stale_cached_url() {
    let Some(url) = test_url() else {
        return;
    };
    let root = fresh_root();
    let p = doppler_proj(&root);
    let bin = tempfile::tempdir().unwrap();
    let path = fake_doppler(bin.path(), &url);
    let env = [
        ("PATH", path.clone()),
        ("CLAUDE_CODE_SESSION_ID", "S".to_string()),
    ];
    let id = added(&p, "held", &env);
    let out = devkit(&p, &["todo", "start", &id], &env);
    assert!(out.status.success(), "{}", stderr(&out));
    let cache = std::fs::File::options()
        .write(true)
        .open(cached_url_file(&p))
        .expect("the command kept the URL");
    let long_ago = SystemTime::now() - Duration::from_secs(24 * 60 * 60);
    cache.set_modified(long_ago).unwrap();
    let calls = doppler_calls(bin.path());
    slow_doppler(bin.path(), &url, 4);

    let end = json!({"hook_event_name": "SessionEnd", "session_id": "S", "cwd": p.path});
    let started = Instant::now();
    let out = p.hook_with("session-end", "claude-code", &end, &borrowed(&env));
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(started.elapsed() < HOOK_BUDGET, "{:?}", started.elapsed());
    assert_eq!(doppler_calls(bin.path()), calls);
    let todos = store(&url, &root).list(&Filter::all()).unwrap();
    assert_eq!(todos[0].status, Status::Pending, "{todos:?}");
}
