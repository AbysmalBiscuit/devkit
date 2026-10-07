//! `devkit todo`, the session hooks and `devkit doctor` on the Supabase
//! backend. A test that needs the API [`testapi`] sets up returns early
//! without one; the header and proxy tests need none.

#[path = "../crates/devkit-todo-supabase/tests/common/testapi.rs"]
mod testapi;
#[path = "common/todoenv.rs"]
mod todoenv;

use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

use devkit_todo::{Holder, Status, Todo, TodoStore};
use devkit_todo_supabase::{Api, SupabaseStore};
use serde_json::{Value, json};
use testapi::api_url;
use todoenv::{Proj, stderr, stdout};

const URL_VAR: &str = "DEVKIT_TODO_SUPABASE_URL";
const KEY_VAR: &str = "DEVKIT_TODO_SUPABASE_KEY";

/// The time a hook gets: the lock budget every hook keeps to.
const HOOK_BUDGET: Duration = Duration::from_secs(2);

/// The longest a session's end may take against a slow API: the binary's
/// `SESSION_END_DATABASE_BUDGET` for its todo database work, plus a debug
/// build's process start on a loaded CI runner.
const SESSION_END_BOUND: Duration = Duration::from_millis(1500 + 1000);

/// A todo root no other test shares.
fn fresh_root() -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("sb-e2e-{}-{nanos}", std::process::id())
}

/// A checkout whose todos go to `root` on the Supabase backend at `url`,
/// named in the global config as a person sets it up, with the same root on
/// the postgres backend.
fn proj(url: &str, root: &str) -> Proj {
    Proj::with_home_config(&format!(
        "[todo]\nbackend = \"supabase\"\n[todo.supabase]\nurl = \"{url}\"\nroot = \"{root}\"\n\
         [todo.postgres]\nroot = \"{root}\"\n"
    ))
}

fn store(url: &str, root: &str) -> SupabaseStore {
    SupabaseStore::new(
        Api::new(url, None, Duration::from_secs(10)).unwrap().into(),
        root,
    )
}

fn todo(url: &str, root: &str, id: &str) -> Todo {
    store(url, root).get(id).unwrap().unwrap()
}

fn as_session(session: &str) -> [(&'static str, &str); 1] {
    [("CLAUDE_CODE_SESSION_ID", session)]
}

fn added(p: &Proj, text: &str, session: &str) -> String {
    let out = p.devkit(&["todo", "add", text], &as_session(session));
    assert!(out.status.success(), "{}", stderr(&out));
    stdout(&out).trim().to_string()
}

#[test]
fn todo_commands_work_and_a_held_todo_is_refused_naming_its_holder() {
    let Some(url) = api_url() else {
        return;
    };
    let root = fresh_root();
    let p = proj(&url, &root);
    let finished = added(&p, "finished", "S1");
    let dropped = added(&p, "dropped", "S1");
    for (verb, id) in [
        ("start", &finished),
        ("done", &finished),
        ("cancel", &dropped),
    ] {
        let out = p.devkit(&["todo", verb, id], &as_session("S1"));
        assert!(out.status.success(), "{verb}: {}", stderr(&out));
    }
    assert_eq!(todo(&url, &root, &finished).status, Status::Completed {
        by: Some(Holder::new("S1"))
    });
    assert_eq!(todo(&url, &root, &dropped).status, Status::Cancelled {
        by: Some(Holder::new("S1"))
    });

    let held = added(&p, "held", "S1");
    let out = p.devkit(&["todo", "start", &held], &as_session("S1"));
    assert!(out.status.success(), "{}", stderr(&out));
    let out = p.devkit(&["todo", "start", &held], &as_session("S2"));
    assert!(!out.status.success(), "S2 took S1's claim");
    assert!(
        stderr(&out).contains("in progress by S1"),
        "{}",
        stderr(&out)
    );

    let out = p.devkit(&["todo", "list", "--all"], &as_session("S1"));
    assert!(out.status.success(), "{}", stderr(&out));
    let listed = stdout(&out);
    for text in ["finished", "held"] {
        assert!(listed.contains(text), "{text}: {listed}");
    }
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

/// A subagent run, a claim interval and a session's end, recorded by
/// hooks and commands on the supabase backend, are what `devkit activity`
/// reports on the postgres backend; the session's end releases its claim
/// within a hook's time.
#[test]
fn activity_recorded_over_the_api_is_read_through_the_postgres_backend() {
    let (Some(url), Some(direct)) = (api_url(), testapi::direct_url()) else {
        return;
    };
    let root = fresh_root();
    let p = proj(&url, &root);
    let hook = |verb: &str, payload: &Value| {
        let out = p.hook(verb, "claude-code", payload);
        assert!(out.status.success(), "{verb}: {}", stderr(&out));
        assert_eq!(stderr(&out), "", "{verb}");
    };
    hook("subagent-start", &subagent(&p, "SubagentStart", "a1"));
    let finished = added(&p, "finished", "S");
    for verb in ["start", "done"] {
        let out = p.devkit(&["todo", verb, &finished], &as_session("S"));
        assert!(out.status.success(), "{verb}: {}", stderr(&out));
    }
    hook("subagent-stop", &subagent(&p, "SubagentStop", "a1"));
    hook("subagent-start", &subagent(&p, "SubagentStart", "a2"));
    let held = added(&p, "held", "S");
    let out = p.devkit(&["todo", "start", &held], &as_session("S"));
    assert!(out.status.success(), "{}", stderr(&out));

    let end = json!({"hook_event_name": "SessionEnd", "session_id": "S", "cwd": p.path});
    let started = Instant::now();
    hook("session-end", &end);
    let took = started.elapsed();
    assert!(took < HOOK_BUDGET, "session-end took {took:?}");
    assert_eq!(todo(&url, &root, &held).status, Status::Pending, "released");
    assert!(
        !p.state().join("todo/activity/events.jsonl").exists(),
        "nothing recorded locally"
    );

    let out = p.devkit(&["activity", "--json"], &[
        ("DEVKIT_TODO_BACKEND", "postgres"),
        ("DEVKIT_TODO_DATABASE_URL", &direct),
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let report: Value = serde_json::from_str(&stdout(&out)).unwrap();
    let sessions = report["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{report:#}");
    let runs: Vec<(&str, &str, &str)> = sessions[0]["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["agent"].as_str().unwrap(),
                r["agent_type"].as_str().unwrap(),
                r["outcome"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        runs,
        [
            ("a1", "Explore", "stopped"),
            ("a2", "Explore", "session_ended")
        ],
        "{report:#}"
    );
    let held_as = |id: &str| -> Vec<(&str, &str)> {
        report["todos"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["todo"].as_str().unwrap().starts_with(id))
            .flat_map(|t| t["intervals"].as_array().unwrap())
            .map(|i| {
                (
                    i["holder"].as_str().unwrap(),
                    i["outcome"].as_str().unwrap(),
                )
            })
            .collect()
    };
    assert_eq!(held_as(&finished), [("S", "completed")], "{report:#}");
    assert_eq!(held_as(&held), [("S", "released")], "{report:#}");
}

/// A server that records the head of each request it reads, then answers
/// each with `answer`.
struct Recorder {
    url: String,
    heads: Arc<Mutex<Vec<String>>>,
}

impl Recorder {
    fn start(answer: &'static str) -> Self {
        Self::delayed(answer, Duration::ZERO)
    }

    /// A recorder that answers each request `delay` after reading it.
    fn delayed(answer: &'static str, delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let heads = Arc::new(Mutex::new(Vec::new()));
        let recorded = heads.clone();
        std::thread::spawn(move || {
            for mut tcp in listener.incoming().flatten() {
                let recorded = recorded.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(&tcp);
                    let mut head = String::new();
                    let mut line = String::new();
                    while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                        head.push_str(&line);
                        line.clear();
                    }
                    recorded.lock().unwrap().push(head);
                    std::thread::sleep(delay);
                    let _ = tcp.write_all(answer.as_bytes());
                });
            }
        });
        Self { url, heads }
    }

    fn heads(&self) -> Vec<String> {
        self.heads.lock().unwrap().clone()
    }
}

/// The path each request asked for.
fn paths(heads: &[String]) -> Vec<String> {
    heads
        .iter()
        .filter_map(|head| head.split_whitespace().nth(1))
        .map(|path| path.split('?').next().unwrap().to_string())
        .collect()
}

/// A session's end on the backend at `url`, timed.
fn end_session(url: &str) -> Duration {
    let p = proj(url, &fresh_root());
    let end = json!({"hook_event_name": "SessionEnd", "session_id": "S", "cwd": p.path});
    let started = Instant::now();
    let out = p.hook("session-end", "claude-code", &end);
    let took = started.elapsed();
    assert!(out.status.success(), "{}", stderr(&out));
    took
}

/// An API that answers each request a little inside a hook's wait for one,
/// but two of them together past a session's end's budget: the release goes
/// out first, the activity record after it runs out of time, and nothing
/// follows.
#[test]
fn a_session_end_against_a_slow_api_finishes_inside_its_budget() {
    let api = Recorder::delayed(NO_ROWS, Duration::from_millis(800));
    let took = end_session(&api.url);
    assert!(took < SESSION_END_BOUND, "session-end took {took:?}");
    assert_eq!(paths(&api.heads()), [
        "/rest/v1/rpc/todo_release_all",
        "/rest/v1/rpc/activity_record"
    ]);
}

/// An API that answers no request within a hook's wait costs a session's
/// end that wait once: the request that timed out is the only one sent.
#[test]
fn a_session_end_against_a_stalled_api_waits_once() {
    let api = Recorder::delayed(NO_ROWS, Duration::from_secs(10));
    let took = end_session(&api.url);
    assert!(took < SESSION_END_BOUND, "session-end took {took:?}");
    assert_eq!(
        paths(&api.heads()),
        ["/rest/v1/rpc/todo_release_all"],
        "one request"
    );
}

/// An empty list, as PostgREST answers it.
const NO_TODOS: &str = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                        Content-Range: */0\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]";

/// A function's answer of no rows, as PostgREST gives it.
const NO_ROWS: &str = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                       Content-Length: 2\r\nConnection: close\r\n\r\n[]";

/// Each request's header names, lowercased.
fn header_names(head: &str) -> Vec<String> {
    head.lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, _)| name.trim().to_ascii_lowercase())
        .collect()
}

#[test]
fn without_a_key_requests_carry_no_credential() {
    let api = Recorder::start(NO_TODOS);
    let p = proj(&api.url, &fresh_root());
    let out = p.devkit(&["todo", "list"], &as_session("S"));
    assert!(out.status.success(), "{}", stderr(&out));
    let heads = api.heads();
    assert!(!heads.is_empty(), "no request reached the API");
    for head in &heads {
        let names = header_names(head);
        assert!(head.starts_with("GET /rest/v1/todos?"), "{head}");
        assert!(names.contains(&"accept-profile".to_string()), "{head}");
        for credential in ["apikey", "authorization"] {
            assert!(!names.contains(&credential.to_string()), "{head}");
        }
    }
}

#[test]
fn a_key_goes_in_both_credential_headers() {
    let api = Recorder::start(NO_TODOS);
    let p = proj(&api.url, &fresh_root());
    let out = p.devkit(&["todo", "list"], &[
        ("CLAUDE_CODE_SESSION_ID", "S"),
        (KEY_VAR, "sb-test-key"),
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let head = api.heads().concat().to_ascii_lowercase();
    assert!(head.contains("apikey: sb-test-key"), "{head}");
    assert!(head.contains("authorization: bearer sb-test-key"), "{head}");
}

#[test]
fn requests_go_through_https_proxy() {
    let proxy = Recorder::start("HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n");
    let p = Proj::new();
    let out = p.devkit(&["todo", "list"], &[
        ("CLAUDE_CODE_SESSION_ID", "S"),
        ("DEVKIT_TODO_BACKEND", "supabase"),
        (URL_VAR, "https://abcdefghijklmnop.supabase.co"),
        ("HTTPS_PROXY", &proxy.url),
        ("https_proxy", &proxy.url),
        ("NO_PROXY", ""),
        ("no_proxy", ""),
    ]);
    assert!(!out.status.success(), "the proxy answered nothing to list");
    let heads = proxy.heads();
    assert!(
        heads
            .iter()
            .any(|h| h.starts_with("CONNECT abcdefghijklmnop.supabase.co:443 ")),
        "{heads:?}"
    );
}

fn doctor_rows(p: &Proj, env: &[(&str, &str)]) -> Vec<Value> {
    let out = p.devkit(&["doctor", "--json"], env);
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
fn doctor_shows_the_url_and_key_sources_and_whether_the_api_answers() {
    let api = Recorder::start(NO_TODOS);
    let p = proj(&api.url, &fresh_root());
    let rows = doctor_rows(&p, &[(KEY_VAR, "sb-test-key")]);
    let backend = row(&rows, "todo_backend");
    assert!(
        backend["detail"].as_str().unwrap().contains("supabase"),
        "{backend}"
    );
    let url = row(&rows, "todo_api_url");
    assert_eq!(
        (url["source"].as_str(), url["detail"].as_str()),
        (Some("file"), Some(api.url.as_str()))
    );
    let key = row(&rows, "todo_api_key");
    assert_eq!(key["source"], "env", "{key}");
    assert!(!key.to_string().contains("sb-test-key"), "{key}");
    let answers = row(&rows, "todo_api");
    assert_eq!(answers["status"], "ok", "{answers}");
}

#[test]
fn a_project_config_cannot_name_the_url() {
    let api = Recorder::start(NO_TODOS);
    let p = Proj::new();
    std::fs::write(
        p.path.join("devkit.toml"),
        format!(
            "[todo]\nbackend = \"supabase\"\n[todo.supabase]\nurl = \"{}\"\n",
            api.url
        ),
    )
    .unwrap();
    let out = p.devkit(&["todo", "list"], &[
        ("CLAUDE_CODE_SESSION_ID", "S"),
        (KEY_VAR, "sb-test-key"),
    ]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains(URL_VAR), "{}", stderr(&out));
    assert!(api.heads().is_empty(), "the key went to the project's URL");
}
