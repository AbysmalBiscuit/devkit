//! The activity log through the real binary: subagent runs from hook payloads,
//! claim intervals from `devkit todo`, and `devkit activity` reporting both.

#[path = "common/todoenv.rs"]
mod todoenv;

use devkit_todo::activity::{ActivityStore, BACKSTOP, ClaimEnd, Run, RunEnd};
use serde_json::{Value, json};
use todoenv::{Proj, stderr, stdout};

fn subagent(p: &Proj, event: &str, agent: &str, agent_type: Option<&str>) -> Value {
    let mut payload = json!({
        "hook_event_name": event,
        "session_id": "S",
        "agent_id": agent,
        "cwd": p.path,
    });
    if let Some(agent_type) = agent_type {
        payload["agent_type"] = json!(agent_type);
    }
    payload
}

fn hook(p: &Proj, verb: &str, payload: &Value) {
    let out = p.hook(verb, "claude-code", payload);
    assert!(out.status.success(), "{verb}: {}", stderr(&out));
    assert_eq!(stdout(&out), "", "{verb}");
}

fn start(p: &Proj, agent: &str, agent_type: Option<&str>) {
    hook(
        p,
        "subagent-start",
        &subagent(p, "SubagentStart", agent, agent_type),
    );
}

fn stop(p: &Proj, agent: &str, agent_type: Option<&str>) {
    hook(
        p,
        "subagent-stop",
        &subagent(p, "SubagentStop", agent, agent_type),
    );
}

fn run<'a>(runs: &'a [Run], agent: &str) -> &'a Run {
    runs.iter()
        .find(|r| r.agent == agent)
        .unwrap_or_else(|| panic!("no run for {agent}: {runs:?}"))
}

#[test]
fn a_subagent_start_and_stop_make_one_run() {
    let p = Proj::new();
    start(&p, "a1", Some("Explore"));
    stop(&p, "a1", Some("Explore"));
    let runs = p.activity().runs;
    assert_eq!(runs.len(), 1, "{runs:?}");
    let r = &runs[0];
    assert_eq!(
        (
            r.session.as_str(),
            r.agent.as_str(),
            r.agent_type.as_deref()
        ),
        ("S", "a1", Some("Explore"))
    );
    assert!(r.end.is_some_and(|end| end >= r.start), "{r:?}");
    assert_eq!(r.outcome, Some(RunEnd::Stopped));
}

#[test]
fn parallel_runs_of_one_type_each_get_their_own_record() {
    let p = Proj::new();
    start(&p, "a1", Some("reviewer"));
    start(&p, "a2", Some("reviewer"));
    stop(&p, "a2", Some("reviewer"));
    stop(&p, "a1", Some("reviewer"));
    let runs = p.activity().runs;
    assert_eq!(runs.len(), 2, "{runs:?}");
    let (a1, a2) = (run(&runs, "a1"), run(&runs, "a2"));
    assert!(a1.start <= a2.start, "{runs:?}");
    assert!(a2.end.unwrap() <= a1.end.unwrap(), "{runs:?}");
    assert_eq!(a1.outcome, Some(RunEnd::Stopped));
    assert_eq!(a2.outcome, Some(RunEnd::Stopped));
}

#[test]
fn a_run_without_an_agent_type_stores_none() {
    let p = Proj::new();
    start(&p, "afork", None);
    stop(&p, "afork", None);
    let runs = p.activity().runs;
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].agent_type, None);
    assert_eq!(runs[0].label(), "subagent");
}

/// The log's raw `subagent_stop` rows, as a reader of the database sees them.
fn stop_rows(p: &Proj) -> Vec<Value> {
    std::fs::read_to_string(p.state().join("todo/activity/events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| row["event"] == "subagent_stop")
        .collect()
}

#[test]
fn a_stop_row_carries_the_payloads_agent_type() {
    let p = Proj::new();
    stop(&p, "a1", Some("Explore"));
    stop(&p, "afork", None);
    let rows = stop_rows(&p);
    let types: Vec<(&str, &Value)> = rows
        .iter()
        .map(|row| (row["agent"].as_str().unwrap(), &row["agent_type"]))
        .collect();
    assert_eq!(
        types,
        [("a1", &json!("Explore")), ("afork", &Value::Null)],
        "{rows:?}"
    );
}

#[test]
fn a_stop_with_no_recorded_start_is_no_run() {
    let p = Proj::new();
    stop(&p, "turn-end", Some("general-purpose"));
    assert_eq!(p.activity().runs, [], "no run from the log");

    let out = p.devkit(&["activity", "--json"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let report: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(report["sessions"], json!([]), "{report:#}");
    assert_eq!(report["agent_types"], json!([]), "{report:#}");
}

#[test]
fn a_run_whose_stop_never_arrives_closes_at_its_sessions_end() {
    let p = Proj::new();
    start(&p, "a1", Some("Explore"));
    hook(
        &p,
        "session-end",
        &json!({"hook_event_name": "SessionEnd", "session_id": "S", "cwd": p.path}),
    );
    let runs = p.activity().runs;
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert!(runs[0].end.is_some(), "{runs:?}");
    assert_eq!(runs[0].outcome, Some(RunEnd::SessionEnded));
}

#[test]
fn records_are_written_with_the_harness_log_off() {
    let p = Proj::with_home_config("[harness.log]\nenabled = false\n");
    start(&p, "a1", Some("Explore"));
    stop(&p, "a1", Some("Explore"));
    let runs = p.activity().runs;
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].outcome, Some(RunEnd::Stopped));
}

const SESSION: (&str, &str) = ("CLAUDE_CODE_SESSION_ID", "S");

fn todo(p: &Proj, args: &[&str], env: &[(&str, &str)]) -> String {
    let mut all = vec![SESSION];
    all.extend_from_slice(env);
    let mut argv = vec!["todo"];
    argv.extend_from_slice(args);
    let out = p.devkit(&argv, &all);
    assert!(out.status.success(), "{args:?}: {}", stderr(&out));
    stdout(&out).trim().to_string()
}

const AS_SUB_AGENT: (&str, &str) = ("DEVKIT_TODO_HOLDER", "S/a1");

#[test]
fn claiming_and_completing_a_todo_gives_one_completed_interval() {
    let p = Proj::new();
    let id = todo(&p, &["add", "a"], &[]);
    todo(&p, &["start", &id], &[]);
    todo(&p, &["done", &id], &[]);
    let claims = p.activity().claims;
    assert_eq!(claims.len(), 1, "{claims:?}");
    assert_eq!(claims[0].todo, id);
    assert_eq!(&*claims[0].holder, "S");
    assert!(claims[0].end.is_some(), "{claims:?}");
    assert_eq!(claims[0].outcome, Some(ClaimEnd::Completed));
}

#[test]
fn session_end_closes_the_sessions_intervals_as_released() {
    let p = Proj::new();
    let (own, sub) = (
        todo(&p, &["add", "own"], &[]),
        todo(&p, &["add", "sub"], &[]),
    );
    todo(&p, &["start", &own], &[]);
    todo(&p, &["start", &sub], &[AS_SUB_AGENT]);
    hook(
        &p,
        "session-end",
        &json!({"hook_event_name": "SessionEnd", "session_id": "S", "cwd": p.path}),
    );
    let claims = p.activity().claims;
    assert_eq!(claims.len(), 2, "{claims:?}");
    for claim in &claims {
        assert_eq!(claim.outcome, Some(ClaimEnd::Released), "{claim:?}");
    }
}

#[test]
fn handing_a_claim_closes_one_interval_and_opens_the_next() {
    let p = Proj::new();
    let id = todo(&p, &["add", "a"], &[]);
    todo(&p, &["start", &id], &[]);
    todo(&p, &["start", &id], &[AS_SUB_AGENT]);
    let claims = p.activity().claims;
    assert_eq!(claims.len(), 2, "{claims:?}");
    assert_eq!(
        (&*claims[0].holder, claims[0].outcome),
        ("S", Some(ClaimEnd::Handed))
    );
    assert_eq!((&*claims[1].holder, claims[1].outcome), ("S/a1", None));
    assert_eq!(claims[0].end, Some(claims[1].start));
}

/// A `PreToolUse` from sub-agent `a1` running `command`.
fn sub_agent_bash(p: &Proj, command: &str) -> Value {
    json!({
        "hook_event_name": "PreToolUse",
        "session_id": "S",
        "agent_id": "a1",
        "agent_type": "Explore",
        "tool_name": "Bash",
        "tool_input": {"command": command},
        "cwd": p.path,
    })
}

/// An activity log nothing can be written to or read from: `events.jsonl` a
/// directory and `seen` a file, which fail alike on Unix and Windows.
fn break_log(p: &Proj) {
    let dir = p.state().join("todo/activity");
    std::fs::create_dir_all(dir.join("events.jsonl")).unwrap();
    std::fs::write(dir.join("seen"), "not a directory").unwrap();
}

#[test]
fn a_record_that_cannot_be_written_changes_no_verdict() {
    let healthy = Proj::new();
    let broken = Proj::new();
    break_log(&broken);

    let verdict = |p: &Proj| {
        let id = todo(p, &["add", "a"], &[]);
        let out = p.hook(
            "pre-tool-use",
            "claude-code",
            &sub_agent_bash(p, &format!("devkit todo start {id}")),
        );
        assert!(out.status.success(), "{}", stderr(&out));
        stdout(&out)
    };
    let expected = verdict(&healthy);
    assert!(expected.contains("DEVKIT_TODO_HOLDER"), "{expected}");
    assert_eq!(verdict(&broken), expected);

    start(&broken, "a1", Some("Explore"));
    stop(&broken, "a1", Some("Explore"));
    let id = todo(&broken, &["add", "b"], &[]);
    todo(&broken, &["start", &id], &[]);
    assert_eq!(broken.todo(&id).status, devkit_todo::Status::InProgress {
        by: devkit_todo::Holder::new("S")
    });
}

#[test]
fn the_report_groups_parallel_runs_by_session_type_and_todo() {
    let p = Proj::new();
    start(&p, "a1", Some("reviewer"));
    start(&p, "a2", Some("reviewer"));
    start(&p, "afork", None);
    stop(&p, "a2", Some("reviewer"));
    stop(&p, "a1", Some("reviewer"));
    let id = todo(&p, &["add", "a"], &[]);
    todo(&p, &["start", &id], &[AS_SUB_AGENT]);
    todo(&p, &["done", &id], &[AS_SUB_AGENT]);

    let out = p.devkit(&["activity", "--json"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let report: Value = serde_json::from_str(&stdout(&out)).unwrap();
    let sessions = report["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{report:#}");
    assert_eq!(sessions[0]["session"], "S");
    let runs = sessions[0]["runs"].as_array().unwrap();
    let agents: Vec<&str> = runs.iter().map(|r| r["agent"].as_str().unwrap()).collect();
    assert_eq!(agents, ["a1", "a2", "afork"], "{report:#}");
    assert_eq!(runs[2]["agent_type"], Value::Null);
    assert_eq!(runs[2]["end"], Value::Null);
    let types: Vec<(&str, u64)> = report["agent_types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["agent_type"].as_str().unwrap(),
                t["runs"].as_u64().unwrap(),
            )
        })
        .collect();
    assert!(types.contains(&("reviewer", 2)), "{report:#}");
    assert!(types.contains(&("subagent", 1)), "{report:#}");
    let todos = report["todos"].as_array().unwrap();
    assert_eq!(todos.len(), 1, "{report:#}");
    assert_eq!(todos[0]["todo"], id);
    assert_eq!(todos[0]["intervals"][0]["holder"], "S/a1");
    assert_eq!(todos[0]["intervals"][0]["outcome"], "completed");

    let out = p.devkit(&["activity"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    for want in [
        "S", "a1", "a2", "afork", "reviewer", "subagent", "running", "S/a1",
    ] {
        assert!(text.contains(want), "{want} missing from:\n{text}");
    }
}

#[test]
fn the_report_keeps_to_its_range() {
    let p = Proj::new();
    start(&p, "a1", Some("Explore"));
    stop(&p, "a1", Some("Explore"));
    let out = p.devkit(&["activity", "--json", "--until", "2000-01-01"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let report: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(report["sessions"], json!([]), "{report:#}");
}

#[test]
fn a_subagents_hooks_move_where_a_lost_run_ends() {
    let p = Proj::new();
    start(&p, "a1", Some("Explore"));
    let before_hook: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
    let out = p.hook("pre-tool-use", "claude-code", &sub_agent_bash(&p, "ls"));
    assert!(out.status.success(), "{}", stderr(&out));
    let after_hook: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();

    let past = after_hook + BACKSTOP + chrono::TimeDelta::seconds(1);
    let runs = p.activity_log().read(past).unwrap().runs;
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].outcome, Some(RunEnd::Lost), "{runs:?}");
    let end = runs[0].end.unwrap();
    assert!(
        before_hook <= end && end <= after_hook,
        "{end} outside the pre-tool-use hook's {before_hook}..{after_hook}"
    );
}

#[test]
fn an_unreadable_log_is_an_error_naming_its_path() {
    let p = Proj::new();
    break_log(&p);
    let out = p.devkit(&["activity"], &[]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("events.jsonl"), "{err}");
}

#[test]
fn the_report_escapes_terminal_control_sequences() {
    let p = Proj::new();
    let hostile = "Explore\u{1b}]52;c;aGk=\u{7}\u{1b}[2J\u{202e}";
    start(&p, "a1\u{9b}31m", Some(hostile));
    let out = p.devkit(&["activity"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let report = stdout(&out);
    for raw in ['\u{1b}', '\u{7}', '\u{9b}', '\u{202e}'] {
        assert!(!report.contains(raw), "{raw:?} printed raw: {report:?}");
    }
    assert!(report.contains("Explore\\u{1b}]52"), "{report}");
}
