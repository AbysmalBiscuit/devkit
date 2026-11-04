//! `devkit todo prune`, run as a person or an agent runs it.

#[path = "common/todoenv.rs"]
mod todoenv;

use std::process::Output;

use devkit_todo::{Edit, Holder, NewTodo, StatusKind, TicketRef, TodoStore};
use serde_json::Value;
use todoenv::{Proj, stderr, stdout};

const HUMAN: [(&str, &str); 1] = [("DEVKIT_CALLER", "human")];
const AGENT: [(&str, &str); 1] = [("DEVKIT_CALLER", "agent")];
const SESSION_NODE: &str = "proj.main.claude-s1";

fn add(p: &Proj, node: &str, text: &str) -> String {
    let out = p.devkit(&["todo", "add", text, "--node", node], &HUMAN);
    assert!(out.status.success(), "{}", stderr(&out));
    stdout(&out).trim().to_string()
}

fn run(p: &Proj, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut all = vec!["todo"];
    all.extend_from_slice(args);
    p.devkit(&all, env)
}

fn finish(p: &Proj, verb: &str, id: &str) {
    let out = run(p, &[verb, id], &HUMAN);
    assert!(out.status.success(), "{}", stderr(&out));
}

/// Writes `at` as the todo's last change, in the built-in store's file.
fn changed_at(p: &Proj, id: &str, at: &str) {
    let path = p.state().join("todo/todos.json");
    let mut doc: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    doc["todos"][id]["modified"] = at.into();
    std::fs::write(&path, doc.to_string()).unwrap();
}

/// A done todo on the session node, last changed at `at`.
fn done_at(p: &Proj, text: &str, at: &str) -> String {
    let id = add(p, SESSION_NODE, text);
    finish(p, "done", &id);
    changed_at(p, &id, at);
    id
}

fn left(p: &Proj) -> Vec<String> {
    let mut texts: Vec<String> = p.todos().into_iter().map(|t| t.description).collect();
    texts.sort();
    texts
}

#[test]
fn a_to_date_removes_done_todos_changed_by_then_and_keeps_the_rest() {
    let p = Proj::new();
    done_at(&p, "old done", "2026-09-20T12:00:00Z");
    let cancelled = add(&p, SESSION_NODE, "old cancelled");
    finish(&p, "cancel", &cancelled);
    changed_at(&p, &cancelled, "2026-09-21T12:00:00Z");
    done_at(&p, "new done", "2026-10-20T12:00:00Z");
    let open = add(&p, SESSION_NODE, "old open");
    changed_at(&p, &open, "2026-09-20T12:00:00Z");
    let started = add(&p, SESSION_NODE, "old started");
    assert!(run(&p, &["start", &started], &HUMAN).status.success());
    changed_at(&p, &started, "2026-09-20T12:00:00Z");

    let out = run(
        &p,
        &["prune", "--all", "--to-date", "2026-10-01", "--yes"],
        &HUMAN,
    );

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(left(&p), ["new done", "old open", "old started"]);
    assert!(stdout(&out).contains("old done"), "{}", stdout(&out));
    assert!(stdout(&out).contains("removed 2"), "{}", stdout(&out));
}

#[cfg(unix)]
#[test]
fn a_date_covers_its_whole_local_day() {
    let p = Proj::new();
    done_at(&p, "before the day", "2026-09-30T14:59:59Z");
    done_at(&p, "first second", "2026-09-30T15:00:00Z");
    done_at(&p, "last second", "2026-10-01T14:59:59Z");
    done_at(&p, "after the day", "2026-10-01T15:00:00Z");
    let env = [("DEVKIT_CALLER", "human"), ("TZ", "XXX-9")];

    let args = [
        "prune",
        "--all",
        "--from-date",
        "2026-10-01",
        "--to-date",
        "2026-10-01",
        "--yes",
    ];
    let out = run(&p, &args, &env);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(left(&p), ["after the day", "before the day"]);
}

#[test]
fn timestamps_bound_by_exact_instant_both_inclusive() {
    let p = Proj::new();
    done_at(&p, "a before", "2026-10-01T11:59:59Z");
    done_at(&p, "b at from", "2026-10-01T12:00:00Z");
    done_at(&p, "c at to", "2026-10-01T13:00:00Z");
    done_at(&p, "d after", "2026-10-01T13:00:01Z");

    let args = [
        "prune",
        "--all",
        "--from-timestamp",
        "2026-10-01T14:00:00+02:00",
        "--to-timestamp",
        "2026-10-01T13:00:00Z",
        "--yes",
    ];
    let out = run(&p, &args, &HUMAN);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(left(&p), ["a before", "d after"]);
}

#[test]
fn two_flags_for_one_end_fail_naming_both() {
    let p = Proj::new();
    done_at(&p, "kept", "2026-09-20T12:00:00Z");
    for (date, timestamp) in [
        ("--from-date", "--from-timestamp"),
        ("--to-date", "--to-timestamp"),
    ] {
        let args = [
            "prune",
            "--all",
            date,
            "2026-10-01",
            timestamp,
            "2026-10-01T00:00:00Z",
            "--yes",
        ];
        let out = run(&p, &args, &HUMAN);
        assert!(!out.status.success());
        let err = stderr(&out);
        assert!(err.contains(date) && err.contains(timestamp), "{err}");
    }
    assert_eq!(left(&p), ["kept"]);
}

#[test]
fn no_date_flag_is_refused_naming_the_four() {
    let p = Proj::new();
    done_at(&p, "kept", "2026-09-20T12:00:00Z");

    let out = run(&p, &["prune", "--all", "--yes"], &HUMAN);

    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    for flag in [
        "--from-date",
        "--to-date",
        "--from-timestamp",
        "--to-timestamp",
    ] {
        assert!(err.contains(flag), "{err}");
    }
    assert_eq!(left(&p), ["kept"]);
}

#[test]
fn a_subtree_removes_only_done_todos_under_the_node() {
    let p = Proj::new();
    add(&p, SESSION_NODE, "anchor");
    for (node, text) in [
        ("proj.main", "on the node"),
        ("proj.main.claude-s1", "below the node"),
        ("proj.other.claude-s2", "beside the node"),
    ] {
        let id = add(&p, node, text);
        finish(&p, "done", &id);
    }

    let args = [
        "prune",
        "--subtree",
        "proj.main",
        "--to-date",
        "2999-01-01",
        "--yes",
    ];
    let out = run(&p, &args, &HUMAN);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(left(&p), ["anchor", "beside the node"]);
}

#[test]
fn a_done_todo_an_open_todo_needs_stays() {
    let p = Proj::new();
    let blocker = done_at(&p, "blocker", "2026-09-20T12:00:00Z");
    let grandparent = done_at(&p, "grandparent", "2026-09-20T12:00:00Z");
    let parent = add(&p, SESSION_NODE, "parent");
    assert!(
        run(&p, &["move", &parent, "--parent", &grandparent], &HUMAN)
            .status
            .success()
    );
    finish(&p, "done", &parent);
    changed_at(&p, &parent, "2026-09-20T12:00:00Z");
    done_at(&p, "free", "2026-09-20T12:00:00Z");
    // On a node outside the pruned scope, so the check reads the whole store.
    let child = add(&p, "proj.other.claude-s2", "open child");
    assert!(
        run(&p, &["move", &child, "--parent", &parent], &HUMAN)
            .status
            .success()
    );
    assert!(
        run(&p, &["block", &child, "--by", &parent], &HUMAN)
            .status
            .success()
    );
    let waiter = add(&p, "proj.other.claude-s2", "open waiter");
    assert!(
        run(&p, &["block", &waiter, "--by", &blocker], &HUMAN)
            .status
            .success()
    );

    let args = [
        "prune",
        "--node",
        SESSION_NODE,
        "--to-date",
        "2026-10-01",
        "--yes",
    ];
    let out = run(&p, &args, &HUMAN);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(left(&p), [
        "blocker",
        "grandparent",
        "open child",
        "open waiter",
        "parent"
    ]);
}

/// A done ticket todo on the repository node, its ticket last synced as
/// `synced`, last changed in September.
fn done_ticket(p: &Proj, key: &str, synced: &str) {
    done_ticket_on(p, "proj", key, synced);
}

/// [`done_ticket`] on `node`.
fn done_ticket_on(p: &Proj, node: &str, key: &str, synced: &str) {
    let id = p
        .store()
        .add(NewTodo {
            project: Some(node.into()),
            description: format!("#{key} ticket"),
            parent: None,
            order: None,
            ticket: Some(TicketRef {
                tracker: "github".into(),
                key: format!("o/r#{key}"),
                synced_status: Some(synced.into()),
            }),
        })
        .unwrap();
    p.store()
        .apply(&Edit::SetStatus {
            id: id.clone(),
            to: StatusKind::Completed,
            actor: Holder::human(),
        })
        .unwrap();
    changed_at(p, &id, "2026-09-20T12:00:00Z");
}

#[test]
fn a_done_ticket_todo_goes_only_when_its_ticket_was_last_synced_done() {
    let p = Proj::new();
    std::fs::write(
        p.path.join("devkit.toml"),
        "[ticket.sync.statuses]\nTodo = \"pending\"\nShipped = \"completed\"\n",
    )
    .unwrap();
    done_ticket(&p, "1", "Shipped");
    done_ticket(&p, "2", "closed:not_planned");
    done_ticket(&p, "3", "Todo");
    done_ticket(&p, "4", "Unmapped");

    let args = [
        "prune",
        "--node",
        "proj",
        "--to-date",
        "2026-10-01",
        "--yes",
    ];
    let out = run(&p, &args, &HUMAN);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(left(&p), ["#3 ticket", "#4 ticket"]);
}

#[test]
fn another_repositorys_ticket_todo_keeps_unless_its_ticket_closed() {
    let p = Proj::new();
    std::fs::write(
        p.path.join("devkit.toml"),
        "[ticket.sync.statuses]\nShipped = \"completed\"\n",
    )
    .unwrap();
    done_ticket(&p, "1", "Shipped");
    done_ticket_on(&p, "other", "2", "Shipped");
    done_ticket_on(&p, "other", "3", "closed:completed");

    let args = ["prune", "--all", "--to-date", "2026-10-01", "--yes"];
    let out = run(&p, &args, &HUMAN);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(left(&p), ["#2 ticket"]);
}

#[test]
fn a_dry_run_prints_what_it_would_remove_and_changes_nothing() {
    let p = Proj::new();
    done_at(&p, "old done", "2026-09-20T12:00:00Z");
    done_at(&p, "new done", "2026-10-20T12:00:00Z");
    let before = p.todos();

    let args = ["prune", "--all", "--to-date", "2026-10-01", "--dry-run"];
    let out = run(&p, &args, &HUMAN);

    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("old done"), "{text}");
    assert!(!text.contains("new done"), "{text}");
    assert!(text.contains("would remove 1"), "{text}");
    assert_eq!(p.todos(), before);
}

#[test]
fn an_agent_with_yes_is_refused() {
    let p = Proj::new();
    done_at(&p, "kept", "2026-09-20T12:00:00Z");

    let args = ["prune", "--all", "--to-date", "2026-10-01", "--yes"];
    let out = run(&p, &args, &AGENT);

    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("terminal"), "{}", stderr(&out));
    assert_eq!(left(&p), ["kept"]);
}

#[test]
fn an_agent_with_a_dry_run_prints_and_changes_nothing() {
    let p = Proj::new();
    done_at(&p, "old done", "2026-09-20T12:00:00Z");
    let before = p.todos();

    let args = ["prune", "--all", "--to-date", "2026-10-01", "--dry-run"];
    let out = run(&p, &args, &AGENT);

    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("old done"), "{}", stdout(&out));
    assert_eq!(p.todos(), before);
}

#[test]
fn a_person_with_no_terminal_needs_yes() {
    let p = Proj::new();
    done_at(&p, "kept", "2026-09-20T12:00:00Z");

    let args = ["prune", "--all", "--to-date", "2026-10-01"];
    let out = run(&p, &args, &HUMAN);

    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("--yes"), "{}", stderr(&out));
    assert_eq!(left(&p), ["kept"]);
}

#[test]
fn a_pruned_todo_leaves_the_taskchampion_replica() {
    let p = Proj::new();
    let env = [
        ("DEVKIT_CALLER", "human"),
        ("DEVKIT_TODO_BACKEND", "taskchampion"),
    ];
    let task = |text: &str| {
        let out = p.devkit(&["todo", "add", text, "--node", SESSION_NODE], &env);
        assert!(out.status.success(), "{}", stderr(&out));
        stdout(&out).trim().to_string()
    };
    let (done, cancelled) = (task("done"), task("cancelled"));
    task("open");
    assert!(run(&p, &["done", &done], &env).status.success());
    assert!(run(&p, &["cancel", &cancelled], &env).status.success());

    let args = ["prune", "--all", "--to-date", "2999-01-01", "--yes"];
    let out = run(&p, &args, &env);

    assert!(out.status.success(), "{}", stderr(&out));
    let db = rusqlite::Connection::open(p.state().join("todo/taskchampion/taskchampion.sqlite3"))
        .unwrap();
    let statuses: Vec<String> = db
        .prepare("SELECT json_extract(data, '$.status') FROM tasks")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(statuses, ["pending"]);
}
