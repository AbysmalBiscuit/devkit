//! `devkit todo sync` bringing a GitHub repository's tickets into the todo
//! store, against a `gh` that answers from canned GitHub responses.

#[path = "common/ghfake.rs"]
mod ghfake;
#[path = "common/tickets.rs"]
mod tickets;
#[path = "common/todoenv.rs"]
mod todoenv;

use std::time::{Duration, Instant, SystemTime};

use devkit_todo::{Filter, Holder, Status, TodoStore};
use devkit_todo_taskchampion::{SyncTarget, TaskchampionStore};
use serde_json::{Value, json};
use tickets::*;
use todoenv::{Proj, stderr, stdout};

#[test]
fn sync_adds_one_todo_per_ticket_with_edges() {
    let s = two_issues();
    let out = s.sync(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("2 added"), "{}", stdout(&out));
    let todos = s.proj.todos();
    assert_eq!(todos.len(), 2, "{todos:?}");
    let (t65, t66) = (s.todo_for("o/r#65"), s.todo_for("o/r#66"));
    assert_eq!(t65.description, "#65 Issue 65");
    assert_eq!(t65.node(), "proj");
    assert_eq!(t66.node(), "proj");
    assert_eq!(t65.blocked_by, std::slice::from_ref(&t66.id));
    assert_eq!(t65.status, Status::Pending);
    let synced = t65.ticket.and_then(|t| t.synced_status);
    assert_eq!(synced.as_deref(), Some("Todo"));
}

#[test]
fn a_foreground_sync_pushes_its_tickets_to_the_sync_target() {
    let target = tempfile::tempdir().unwrap();
    let server = target.path().join("server");
    let config = format!(
        "[todo]\nbackend = \"taskchampion\"\n[todo.taskchampion]\nserver_dir = {}\n",
        toml::Value::String(server.display().to_string())
    );
    let s = two_issues_in(Proj::with_home_config(&config));
    let out = s.sync(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("2 added"), "{}", stdout(&out));
    let other =
        TaskchampionStore::at(target.path().join("other")).with_target(SyncTarget::Dir(server));
    other.sync_once().unwrap();
    let mut on_target: Vec<String> = other
        .list(&Filter::all())
        .unwrap()
        .into_iter()
        .map(|t| t.description)
        .collect();
    on_target.sort();
    assert_eq!(on_target, ["#65 Issue 65", "#66 Issue 66"]);
}

#[test]
fn a_second_sync_adds_nothing() {
    let s = two_issues();
    assert!(s.sync(&[]).status.success());
    let out = s.sync(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("0 added, 0 changed"),
        "{}",
        stdout(&out)
    );
    assert_eq!(s.proj.todos().len(), 2);
}

#[test]
fn dry_run_prints_and_writes_nothing() {
    let s = two_issues();
    let out = s.sync(&["--dry-run"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let shown = stdout(&out);
    assert!(
        shown.contains("add #65 Issue 65") && shown.contains("add #66 Issue 66"),
        "{shown}"
    );
    assert!(shown.contains("block o/r#65 by o/r#66"), "{shown}");
    assert!(s.proj.todos().is_empty(), "{:?}", s.proj.todos());
}

#[test]
fn an_unmapped_status_fails_and_writes_nothing() {
    let s = Synced::new(&format!("{RULES}{STATUSES}"), true);
    s.serve(
        "graphql.json",
        &graphql(
            &[
                issue(65, Some("Todo"), &[]),
                issue(66, Some("Blocked"), &[]),
            ],
            &["Todo", "In progress", "Done", "Blocked"],
        ),
    );
    let out = s.sync(&[]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(stderr(&out).contains("\"Blocked\""), "{}", stderr(&out));
    assert!(s.proj.todos().is_empty(), "{:?}", s.proj.todos());
}

#[test]
fn sync_without_rules_queries_no_tickets() {
    let s = Synced::new(STATUSES, true);
    let out = s.sync(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!s.calls().contains("api graphql"), "{}", s.calls());
    assert!(s.proj.todos().is_empty());
}

#[test]
fn an_agents_start_survives_a_sync_while_the_ticket_stands_still() {
    let s = two_issues();
    assert!(s.sync(&[]).status.success());
    let id = s.todo_for("o/r#66").id;
    let agent = [("DEVKIT_CALLER", "agent"), ("CLAUDE_CODE_SESSION_ID", "S")];
    let start = s.devkit(&["todo", "start", &id], &agent);
    assert!(start.status.success(), "{}", stderr(&start));
    assert!(s.sync(&[]).status.success());
    assert_eq!(s.todo_for("o/r#66").status, Status::InProgress {
        by: Holder::new("S")
    });
}

#[test]
fn a_closed_ticket_finishes_its_todo_as_the_tracker() {
    let s = two_issues();
    assert!(s.sync(&[]).status.success());
    let closed = issue(65, Some("In progress"), &[66]).replace(
        r#""state":"OPEN","stateReason":null"#,
        r#""state":"CLOSED","stateReason":"COMPLETED""#,
    );
    s.serve(
        "graphql.json",
        &graphql(&[closed, issue(66, None, &[])], &OPTIONS)
            .replace(r#""nodes":[{"number":65"#, r#""nodes":[{"number":66"#),
    );
    let out = s.sync(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(s.todo_for("o/r#65").status, Status::Completed {
        by: Some(Holder::tracker())
    });
}

#[test]
fn a_todo_finished_while_the_tracker_answers_stays_finished() {
    let s = two_issues();
    assert!(s.sync(&[]).status.success());
    let id = s.todo_for("o/r#65").id;
    s.serve(
        "graphql.json",
        &graphql(
            &[issue(65, Some("In progress"), &[66]), issue(66, None, &[])],
            &OPTIONS,
        ),
    );
    let hang = s.bin.path().join("hang");
    std::fs::write(&hang, "").unwrap();
    let before = s.calls().lines().count();
    std::thread::scope(|scope| {
        let sync = scope.spawn(|| s.sync(&[]));
        poll("the sync to reach the tracker", || {
            s.calls().lines().count() > before
        });
        let done = s.devkit(&["todo", "done", &id], &[]);
        std::fs::remove_file(&hang).unwrap();
        assert!(done.status.success(), "{}", stderr(&done));
        let out = sync.join().unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
    });
    let status = s.todo_for("o/r#65").status;
    assert!(matches!(status, Status::Completed { .. }), "{status:?}");
}

#[test]
fn where_github_refuses_graphql_rest_reads_the_tickets() {
    let s = Synced::new(
        &format!("{RULES}\n[ticket.sync.statuses]\n\"\" = \"pending\"\n"),
        false,
    );
    std::fs::write(s.bin.path().join("refuse_graphql"), "").unwrap();
    let rest = |n: u64, blocked: u64| {
        format!(
            r#"{{"number":{n},"title":"Issue {n}","state":"open","state_reason":null,
                "repository_url":"https://api.github.com/repos/o/r",
                "issue_dependencies_summary":{{"total_blocked_by":{blocked}}}}}"#
        )
    };
    s.serve(
        "rest_issues.json",
        &format!("[{},{}]", rest(65, 1), rest(66, 0)),
    );
    s.serve(
        "rest_issue_65_blocked_by.json",
        &format!("[{}]", rest(66, 0)),
    );
    s.serve("rest_issue_66_parent.json", &rest(65, 1));
    let out = s.sync(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let (t65, t66) = (s.todo_for("o/r#65"), s.todo_for("o/r#66"));
    assert_eq!(t65.blocked_by, std::slice::from_ref(&t66.id));
    assert_eq!(t66.parent.as_deref(), Some(t65.id.as_str()));
    assert!(s.calls().contains("assignee=LevValle"), "{}", s.calls());
}

#[test]
fn a_slug_in_another_case_names_the_same_tickets() {
    let s = two_issues();
    assert!(s.sync(&[]).status.success());
    let config = s.proj.path.join("devkit.toml");
    let upper = std::fs::read_to_string(&config)
        .unwrap()
        .replace("\"o/r\"", "\"O/R\"");
    std::fs::write(&config, upper).unwrap();
    let out = s.sync(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("0 added, 0 changed"),
        "{}",
        stdout(&out)
    );
    assert_eq!(s.proj.todos().len(), 2, "{:?}", s.proj.todos());
}

#[test]
fn a_stale_store_starts_one_background_sync() {
    let s = two_issues();
    for _ in 0..2 {
        let out = s.devkit(&["todo", "list"], &[]);
        assert!(out.status.success(), "{}", stderr(&out));
    }
    poll("the background sync", || s.sync_state("last").exists());
    assert_eq!(s.ticket_queries(), 1, "{}", s.calls());
    assert_eq!(s.proj.todos().len(), 2, "{:?}", s.proj.todos());
}

#[test]
fn a_fresh_store_starts_none() {
    let s = two_issues();
    assert!(s.sync(&[]).status.success());
    assert!(s.sync_state("last").exists());
    let before = s.ticket_queries();
    let out = s.devkit(&["todo", "list"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    settle();
    assert_eq!(s.ticket_queries(), before, "{}", s.calls());
}

#[test]
fn a_failed_sync_holds_off_the_next() {
    let s = Synced::new(&format!("{RULES}{STATUSES}"), true);
    let out = s.devkit(&["todo", "list"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    poll("the failed sync", || s.sync_state("failed").exists());
    assert_eq!(s.ticket_queries(), 1, "{}", s.calls());
    let out = s.devkit(&["todo", "ready"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    settle();
    assert_eq!(s.ticket_queries(), 1, "{}", s.calls());
    assert!(!s.sync_state("last").exists());
}

#[test]
fn a_hook_injection_answers_while_the_tracker_hangs() {
    let s = two_issues();
    assert!(s.sync(&[]).status.success());
    let day_old = SystemTime::now() - Duration::from_secs(24 * 3600);
    std::fs::File::options()
        .write(true)
        .open(s.sync_state("last"))
        .unwrap()
        .set_modified(day_old)
        .unwrap();
    let hang = s.bin.path().join("hang");
    std::fs::write(&hang, "").unwrap();
    let before = s.calls().lines().count();
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "s1",
        "cwd": s.proj.path,
        "source": "startup",
    });
    let started = Instant::now();
    let out = s.devkit_fed(
        &["todo", "context", "--harness", "claude-code"],
        &[],
        &payload.to_string(),
    );
    let took = started.elapsed();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(took < Duration::from_secs(3), "the injection took {took:?}");
    assert!(stdout(&out).contains("#65 Issue 65"), "{}", stdout(&out));
    poll("the background sync to reach the tracker", || {
        s.calls().lines().count() > before
    });
    std::fs::remove_file(&hang).unwrap();
    poll_or(
        "the background sync to finish",
        || {
            std::fs::metadata(s.sync_state("last"))
                .and_then(|m| m.modified())
                .is_ok_and(|at| at > day_old + Duration::from_secs(3600))
        },
        || {
            std::fs::read_to_string(s.sync_state("failed"))
                .map(|reason| format!("; the sync failed: {reason}"))
                .unwrap_or_default()
        },
    );
}

/// A taskchampion replica whose sync server refuses every connection.
const UNREACHABLE_SERVER: [(&str, &str); 4] = [
    ("DEVKIT_TODO_BACKEND", "taskchampion"),
    ("DEVKIT_TODO_SYNC_URL", "http://127.0.0.1:1"),
    (
        "DEVKIT_TODO_SYNC_CLIENT_ID",
        "6a1b3c5e-0000-4000-8000-000000000001",
    ),
    ("DEVKIT_TODO_SYNC_SECRET", "secret"),
];

/// Asserts no ticket sync started: the tracker saw no query and the sync
/// left no state behind.
fn assert_no_ticket_sync(s: &Synced) {
    settle();
    assert_eq!(s.ticket_queries(), 0, "{}", s.calls());
    assert!(!s.sync_state("last").exists());
    assert!(!s.sync_state("failed").exists());
}

#[test]
fn list_sync_skips_tickets_when_the_replica_sync_fails() {
    let s = two_issues();
    let out = s.devkit(&["todo", "list", "--sync"], &UNREACHABLE_SERVER);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_no_ticket_sync(&s);
}

#[test]
fn session_start_skips_tickets_when_the_replica_sync_fails() {
    let s = two_issues();
    let payload = json!({
        "hook_event_name": "SessionStart",
        "session_id": "s1",
        "cwd": s.proj.path,
        "source": "startup",
    });
    let out = s.devkit_fed(
        &["todo", "context", "--harness", "claude-code"],
        &UNREACHABLE_SERVER,
        &payload.to_string(),
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_no_ticket_sync(&s);
}

#[test]
fn a_completed_replica_sync_refreshes_tickets() {
    let server = tempfile::tempdir().unwrap();
    let config = format!(
        "[todo]\nbackend = \"taskchampion\"\n[todo.taskchampion]\nserver_dir = {}\n",
        toml::Value::String(server.path().display().to_string())
    );
    let s = two_issues_in(Proj::with_home_config(&config));
    let out = s.devkit(&["todo", "list", "--sync"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    poll("the background sync", || s.sync_state("last").exists());
    assert_eq!(s.ticket_queries(), 1, "{}", s.calls());
}

/// [`two_issues`] on a taskchampion replica at `replica` that syncs to a
/// directory server the caller keeps alive as `server`.
fn two_issues_on_a_synced_replica(server: &std::path::Path, replica: &std::path::Path) -> Synced {
    let config = format!(
        "[todo]\nbackend = \"taskchampion\"\n[todo.taskchampion]\ndata_dir = {}\nserver_dir = {}\n",
        toml::Value::String(replica.display().to_string()),
        toml::Value::String(server.display().to_string())
    );
    two_issues_in(Proj::with_home_config(&config))
}

#[test]
fn reads_on_a_replica_that_never_pulled_sync_no_tickets() {
    let dir = tempfile::tempdir().unwrap();
    let s = two_issues_on_a_synced_replica(&dir.path().join("server"), &dir.path().join("replica"));
    for read in [&["todo", "list"][..], &["todo", "ready"]] {
        let out = s.devkit(read, &[]);
        assert!(out.status.success(), "{}", stderr(&out));
    }
    let prompt = json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "s1",
        "cwd": s.proj.path,
        "prompt": "go",
    });
    let out = s.devkit_fed(
        &[
            "todo",
            "context",
            "--harness",
            "claude-code",
            "--guide",
            "none",
            "--if-changed",
        ],
        &[],
        &prompt.to_string(),
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_no_ticket_sync(&s);
}

#[test]
fn a_read_after_the_replica_pulls_syncs_tickets() {
    let dir = tempfile::tempdir().unwrap();
    let replica = dir.path().join("replica");
    let s = two_issues_on_a_synced_replica(&dir.path().join("server"), &replica);
    let out = s.devkit(&["todo", "add", "plain"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    poll("the replica's sync", || {
        replica.join("sync.pulled").exists()
    });
    let out = s.devkit(&["todo", "list"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    poll("the background sync", || s.sync_state("last").exists());
    assert_eq!(s.ticket_queries(), 1, "{}", s.calls());
}

#[test]
fn a_replica_found_through_taskdata_with_a_sync_target_syncs_no_tickets() {
    let dir = tempfile::tempdir().unwrap();
    let config = format!(
        "[todo]\nbackend = \"taskchampion\"\n[todo.taskchampion]\nserver_dir = {}\n",
        toml::Value::String(dir.path().join("server").display().to_string())
    );
    let s = two_issues_in(Proj::with_home_config(&config));
    let replica = dir.path().join("replica");
    let taskdata = [("TASKDATA", replica.to_str().unwrap())];
    let out = s.devkit(&["todo", "list"], &taskdata);
    assert!(out.status.success(), "{}", stderr(&out));
    let out = s.devkit(&["todo", "sync", "--tickets-only"], &taskdata);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("tickets not synced"),
        "{}",
        stderr(&out)
    );
    assert_no_ticket_sync(&s);
}

const TODO_EVENTS: &str = "
[ticket.events.todo_start]
to = \"In progress\"

[ticket.events.todo_done]
to = \"Done\"
";

/// [`two_issues`] synced, with `todo_start` and `todo_done` moving tickets
/// on project 3, whose writes `gh` accepts.
fn moving() -> Synced {
    moving_in(Proj::new())
}

/// [`moving`] in `proj`.
fn moving_in(proj: Proj) -> Synced {
    let s = Synced::in_proj(proj, &format!("{RULES}{STATUSES}{TODO_EVENTS}"), true);
    s.serve(
        "graphql.json",
        &graphql(
            &[issue(65, Some("Todo"), &[66]), issue(66, None, &[])],
            &OPTIONS,
        ),
    );
    s.serve("graphql_mutation.json", r#"{"data":{}}"#);
    let out = s.sync(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    s
}

/// Whether `gh` was asked to set project 3's status field to option `option`.
fn moved_to(s: &Synced, option: &str) -> bool {
    s.mutations().iter().any(|m| {
        m.contains("updateProjectV2ItemFieldValue") && m.contains(&format!("\"{option}\""))
    })
}

#[test]
fn done_on_a_ticket_todo_moves_the_ticket() {
    let s = moving();
    let id = s.todo_for("o/r#65").id;
    let out = s.devkit(&["todo", "done", &id], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    poll("the move to Done", || moved_to(&s, "o2"));
}

#[test]
fn start_on_a_ticket_todo_moves_the_ticket() {
    let s = moving();
    let id = s.todo_for("o/r#65").id;
    let agent = [("DEVKIT_CALLER", "agent"), ("CLAUDE_CODE_SESSION_ID", "S")];
    let out = s.devkit(&["todo", "start", "--force", &id], &agent);
    assert!(out.status.success(), "{}", stderr(&out));
    poll("the move to In progress", || moved_to(&s, "o1"));
}

#[test]
fn cancel_on_a_ticket_todo_without_push_moves_nothing() {
    let s = Synced::new(
        &format!("{RULES}{STATUSES}{TODO_EVENTS}\n[ticket.events.todo_cancel]\nto = \"Done\"\n"),
        true,
    );
    s.serve(
        "graphql.json",
        &graphql(
            &[issue(65, Some("Todo"), &[66]), issue(66, None, &[])],
            &OPTIONS,
        ),
    );
    s.serve("graphql_mutation.json", r#"{"data":{}}"#);
    assert!(s.sync(&[]).status.success());
    let id = s.todo_for("o/r#65").id;
    let out = s.devkit(&["todo", "cancel", &id], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    settle();
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
}

#[test]
fn a_sync_completing_a_ticket_fires_nothing() {
    let s = moving();
    let closed = issue(65, Some("In progress"), &[66]).replace(
        r#""state":"OPEN","stateReason":null"#,
        r#""state":"CLOSED","stateReason":"COMPLETED""#,
    );
    s.serve(
        "graphql.json",
        &graphql(&[closed, issue(66, None, &[])], &OPTIONS)
            .replace(r#""nodes":[{"number":65"#, r#""nodes":[{"number":66"#),
    );
    let out = s.sync(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(s.todo_for("o/r#65").status, Status::Completed {
        by: Some(Holder::tracker())
    });
    settle();
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
}

#[test]
fn a_ticket_event_whose_status_the_todo_left_moves_nothing() {
    let s = moving();
    let id = s.todo_for("o/r#65").id;
    let out = s.devkit(&["todo", "done", &id], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    poll("the move to Done", || moved_to(&s, "o2"));
    let out = s.devkit(
        &["ticket", "event", "todo_start", "o/r#65", "--todo", &id],
        &[],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("skipped"), "{}", stderr(&out));
    assert!(!moved_to(&s, "o1"), "{:?}", s.mutations());
}

#[test]
fn done_from_outside_the_repository_warns() {
    let s = moving();
    let id = s.todo_for("o/r#65").id;
    let out = s.devkit_in(s.proj.outside(), &["todo", "done", &id], &[], "");
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("ticket o/r#65 not moved: run from its repository"),
        "{}",
        stderr(&out)
    );
    assert!(s.proj.todo(&id).status.kind() == devkit_todo::StatusKind::Completed);
    settle();
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
}

#[test]
fn a_plain_todo_fires_nothing() {
    let s = moving();
    let out = s.devkit(&["todo", "add", "plain"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let id = stdout(&out).trim().to_string();
    let out = s.devkit(&["todo", "done", &id], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    settle();
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
}

/// Claude Code's `tool` run in `s`'s checkout with `input` and answered with
/// `response`, as the post-tool-use hook receives it.
fn native(s: &Synced, tool: &str, input: Value, response: Value) {
    let payload = json!({
        "session_id": "S",
        "cwd": s.proj.path,
        "hook_event_name": "PostToolUse",
        "tool_name": tool,
        "tool_input": input,
        "tool_response": response,
    });
    let out = s.devkit_fed(
        &["hook", "post-tool-use", "--harness", "claude-code"],
        &[],
        &payload.to_string(),
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));
}

fn task_create(s: &Synced, id: &str, subject: &str) {
    native(
        s,
        "TaskCreate",
        json!({"subject": subject, "description": subject}),
        json!({"task": {"id": id, "subject": subject}}),
    );
}

fn task_update(s: &Synced, id: &str, status: &str) {
    native(
        s,
        "TaskUpdate",
        json!({"taskId": id, "status": status}),
        json!({"success": true}),
    );
}

#[test]
fn a_harness_task_naming_a_ticket_moves_it() {
    let s = moving();
    task_create(&s, "1", "#65");
    assert_eq!(s.proj.todos().len(), 2, "{:?}", s.proj.todos());
    task_update(&s, "1", "in_progress");
    poll("the move to In progress", || moved_to(&s, "o1"));
    task_update(&s, "1", "completed");
    poll("the move to Done", || moved_to(&s, "o2"));
    assert_eq!(
        s.todo_for("o/r#65").status.kind(),
        devkit_todo::StatusKind::Completed
    );
}

#[test]
fn a_queued_harness_task_naming_a_ticket_moves_it() {
    let s = moving_in(Proj::with_home_config(
        "[todo]\nbackend = \"taskchampion\"\n",
    ));
    task_create(&s, "1", "o/r#65");
    task_update(&s, "1", "completed");
    poll("the move to Done", || moved_to(&s, "o2"));
}

/// Puts a `git` ahead of the real one on `s`'s `PATH` that logs each spawn's
/// arguments, one line per spawn, to the returned file.
#[cfg(unix)]
fn trace_git(s: &Synced) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let real = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("git on PATH");
    let trace = s.bin.path().join("git-spawns");
    let wrapper = s.bin.path().join("git");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            trace.display(),
            real.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    trace
}

#[cfg(unix)]
#[test]
fn a_ticket_task_update_asks_git_no_more_than_a_plain_one() {
    let s = two_issues();
    assert!(s.sync(&[]).status.success());
    task_create(&s, "1", "#65");
    task_create(&s, "2", "write the fix");
    let trace = trace_git(&s);
    let spawns = |task: &str| {
        let _ = std::fs::remove_file(&trace);
        task_update(&s, task, "in_progress");
        std::fs::read_to_string(&trace).unwrap_or_default()
    };
    let plain = spawns("2");
    let ticket = spawns("1");
    assert_eq!(ticket.lines().count(), plain.lines().count(), "{ticket}");
}

#[test]
fn a_harness_task_naming_no_ticket_adds_a_plain_todo() {
    let s = moving();
    task_create(&s, "1", "write the fix");
    let todos = s.proj.todos();
    let plain = todos
        .iter()
        .find(|t| t.description == "write the fix")
        .unwrap_or_else(|| panic!("{todos:?}"));
    assert!(plain.ticket.is_none());
    task_update(&s, "1", "completed");
    assert_eq!(s.todo_for("o/r#65").status, Status::Pending);
    settle();
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
}

#[test]
fn a_plan_step_naming_a_ticket_moves_it_and_dropping_it_keeps_it() {
    let s = moving();
    let write = |todos: Value| {
        native(
            &s,
            "TodoWrite",
            json!({"todos": todos}),
            json!({"oldTodos": [], "newTodos": todos}),
        )
    };
    write(json!([{"content": "#65 Issue 65", "status": "in_progress", "activeForm": "Fixing"}]));
    assert_eq!(s.proj.todos().len(), 2, "{:?}", s.proj.todos());
    poll("the move to In progress", || moved_to(&s, "o1"));
    write(json!([]));
    let ticket = s.todo_for("o/r#65");
    assert_eq!(ticket.status.kind(), devkit_todo::StatusKind::InProgress);
    assert_eq!(ticket.node(), "proj");
}

#[test]
fn a_plan_resent_after_a_sync_closes_its_ticket_moves_nothing() {
    let s = moving();
    let plan =
        json!([{"content": "#65 Issue 65", "status": "in_progress", "activeForm": "Fixing"}]);
    let write = || {
        native(
            &s,
            "TodoWrite",
            json!({"todos": plan}),
            json!({"oldTodos": [], "newTodos": plan}),
        )
    };
    write();
    poll("the move to In progress", || moved_to(&s, "o1"));
    let closed = issue(65, Some("In progress"), &[66]).replace(
        r#""state":"OPEN","stateReason":null"#,
        r#""state":"CLOSED","stateReason":"COMPLETED""#,
    );
    s.serve(
        "graphql.json",
        &graphql(&[closed, issue(66, None, &[])], &OPTIONS)
            .replace(r#""nodes":[{"number":65"#, r#""nodes":[{"number":66"#),
    );
    assert!(s.sync(&[]).status.success());
    let before = s.mutations();
    write();
    assert_eq!(
        s.todo_for("o/r#65").status.kind(),
        devkit_todo::StatusKind::Completed
    );
    settle();
    assert_eq!(s.mutations(), before);
}

/// Makes `s`'s checkout a `workspace setup` worktree for issue `issue`.
fn record_issue(s: &Synced, issue: &str) {
    record_issue_from(s, issue, devkit_common::record::RecordOrigin::Setup);
}

/// Records `s`'s checkout as made by `origin` for issue `issue`.
fn record_issue_from(s: &Synced, issue: &str, origin: devkit_common::record::RecordOrigin) {
    devkit_common::record::write(&s.proj.path, &devkit_common::record::IssueRecord {
        issue: issue.into(),
        slug: "fix".into(),
        origin: Some(origin),
        ..Default::default()
    })
    .unwrap();
}

const AGENT: [(&str, &str); 2] = [("DEVKIT_CALLER", "agent"), ("CLAUDE_CODE_SESSION_ID", "S")];

#[test]
fn ready_in_a_ticket_workspace_offers_the_ticket() {
    let s = two_issues();
    assert!(s.sync(&[]).status.success());
    record_issue(&s, "66");
    let out = s.devkit(&["todo", "ready"], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    let ready = stdout(&out);
    assert!(ready.contains("#66 Issue 66"), "{ready}");
    assert!(!ready.contains("#65"), "{ready}");
}

#[test]
fn a_review_checkout_offers_no_ticket() {
    let s = two_issues();
    assert!(s.sync(&[]).status.success());
    record_issue_from(&s, "66", devkit_common::record::RecordOrigin::Checkout);
    let out = s.devkit(&["todo", "ready"], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!stdout(&out).contains("#66"), "{}", stdout(&out));
}

#[test]
fn ready_without_a_record_is_unchanged() {
    let s = two_issues();
    assert!(s.sync(&[]).status.success());
    let out = s.devkit(&["todo", "ready"], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!stdout(&out).contains("#66"), "{}", stdout(&out));
}

#[test]
fn expand_lists_the_workspace_todos_under_the_ticket() {
    let s = two_issues();
    assert!(s.sync(&[]).status.success());
    record_issue(&s, "66");
    for text in ["write the fix", "already done"] {
        let out = s.devkit(&["todo", "add", text], &AGENT);
        assert!(out.status.success(), "{}", stderr(&out));
    }
    let done = s
        .proj
        .todos()
        .into_iter()
        .find(|t| t.description == "already done")
        .unwrap();
    assert!(
        s.devkit(&["todo", "done", &done.id], &AGENT)
            .status
            .success()
    );
    let ticket = s.todo_for("o/r#66");
    let summary = format!(
        "- [ ] #66 Issue 66 ({}, 1 open)",
        devkit_todo::short_id(&ticket.id)
    );

    let out = s.devkit(&["todo", "list"], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    let listed = stdout(&out);
    assert!(listed.contains(&format!("{summary}\n")), "{listed}");

    let out = s.devkit(&["todo", "list", "--expand"], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    let expanded = stdout(&out);
    let lines: Vec<&str> = expanded.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.starts_with(&summary))
        .unwrap_or_else(|| panic!("{expanded}"));
    assert!(
        lines[at + 1].starts_with("  - [ ] write the fix (")
            && lines[at + 1].contains(" on proj.main"),
        "{expanded}"
    );
    assert!(
        lines
            .get(at + 2)
            .is_none_or(|l| !l.contains("already done")),
        "{expanded}"
    );

    let out = s.devkit(&["todo", "plan", "--expand"], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    let plan = stdout(&out);
    let lines: Vec<&str> = plan.lines().collect();
    let row = format!("o {}  #66 Issue 66", devkit_todo::short_id(&ticket.id));
    let at = lines
        .iter()
        .position(|l| l.trim_start().starts_with(&row))
        .unwrap_or_else(|| panic!("{plan}"));
    assert!(lines[at].ends_with(", 1 open"), "{plan}");
    assert!(
        lines[at + 1].starts_with("    - [ ] write the fix (")
            && lines[at + 1].contains(" on proj.main"),
        "{plan}"
    );
    assert!(
        lines
            .get(at + 2)
            .is_none_or(|l| !l.contains("already done")),
        "{plan}"
    );
}
