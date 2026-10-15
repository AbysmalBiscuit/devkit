//! `devkit todo` commands naming a todo by a ticket key, a word name or a
//! short uuid prefix instead of its stored id.

#[path = "common/ghfake.rs"]
mod ghfake;
#[path = "common/tickets.rs"]
mod tickets;
#[path = "common/todoenv.rs"]
mod todoenv;

use devkit_todo::{Filter, Status, StatusKind, Todo, TodoStore, names::word_name};
use devkit_todo_taskchampion::TaskchampionStore;
use tickets::*;
use todoenv::{Proj, stderr};

const TASKCHAMPION: [(&str, &str); 2] = [
    ("DEVKIT_TODO_BACKEND", "taskchampion"),
    ("CLAUDE_CODE_SESSION_ID", "s1"),
];
const S1: [(&str, &str); 1] = [("CLAUDE_CODE_SESSION_ID", "s1")];

fn replica(p: &Proj) -> Vec<Todo> {
    TaskchampionStore::at(p.state().join("todo/taskchampion"))
        .list(&Filter::all())
        .unwrap()
}

/// The full id of the one replica todo described `text`.
fn added(p: &Proj, text: &str) -> String {
    let out = p.devkit(&["todo", "add", text], &TASKCHAMPION);
    assert!(out.status.success(), "{}", stderr(&out));
    replica(p)
        .into_iter()
        .find(|t| t.description == text)
        .unwrap()
        .id
}

fn replica_todo(p: &Proj, id: &str) -> Todo {
    replica(p).into_iter().find(|t| t.id == id).unwrap()
}

fn ok(out: std::process::Output) {
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn start_takes_a_word_name() {
    let p = Proj::new();
    let id = added(&p, "one");
    ok(p.devkit(&["todo", "start", &word_name(&id).unwrap()], &TASKCHAMPION));
    assert_eq!(replica_todo(&p, &id).status.kind(), StatusKind::InProgress);
}

#[test]
fn start_takes_a_four_char_prefix() {
    let p = Proj::new();
    let id = added(&p, "one");
    ok(p.devkit(&["todo", "start", &id[..4]], &TASKCHAMPION));
    assert_eq!(replica_todo(&p, &id).status.kind(), StatusKind::InProgress);
}

#[test]
fn describe_takes_a_word_name() {
    let p = Proj::new();
    let id = added(&p, "one");
    let name = word_name(&id).unwrap().to_uppercase();
    ok(p.devkit(&["todo", "describe", &name, "renamed"], &TASKCHAMPION));
    assert_eq!(replica_todo(&p, &id).description, "renamed");
}

#[test]
fn move_parent_takes_a_word_name() {
    let p = Proj::new();
    let parent = added(&p, "parent");
    let child = added(&p, "child");
    ok(p.devkit(
        &[
            "todo",
            "move",
            &word_name(&child).unwrap(),
            "--parent",
            &word_name(&parent).unwrap(),
        ],
        &TASKCHAMPION,
    ));
    assert_eq!(replica_todo(&p, &child).parent, Some(parent));
}

#[test]
fn move_order_takes_a_word_name() {
    let p = Proj::new();
    let id = added(&p, "one");
    ok(p.devkit(
        &["todo", "move", &word_name(&id).unwrap(), "--order", "7"],
        &TASKCHAMPION,
    ));
    assert_eq!(replica_todo(&p, &id).order, Some(7));
}

#[test]
fn add_parent_takes_a_word_name() {
    let p = Proj::new();
    let parent = added(&p, "parent");
    ok(p.devkit(
        &[
            "todo",
            "add",
            "child",
            "--parent",
            &word_name(&parent).unwrap(),
        ],
        &TASKCHAMPION,
    ));
    let child = replica(&p)
        .into_iter()
        .find(|t| t.description == "child")
        .unwrap();
    assert_eq!(child.parent, Some(parent));
}

#[test]
fn start_takes_a_bare_ticket_number() {
    let s = two_issues();
    ok(s.sync(&[]));
    ok(s.devkit(&["todo", "start", "66"], &S1));
    assert_eq!(s.todo_for("o/r#66").status.kind(), StatusKind::InProgress);
    assert_eq!(s.todo_for("o/r#65").status, Status::Pending);
}

#[test]
fn a_quoted_hash_key_and_the_full_key_resolve() {
    let s = two_issues();
    ok(s.sync(&[]));
    ok(s.devkit(&["todo", "start", "#66"], &S1));
    ok(s.devkit(&["todo", "stop", "O/R#66"], &S1));
    assert_eq!(s.todo_for("o/r#66").status, Status::Pending);
}

#[test]
fn an_exact_builtin_id_wins_over_a_ticket_number() {
    let s = Synced::new(&format!("{RULES}{STATUSES}"), true);
    ok(s.devkit(&["todo", "add", "plain"], &S1));
    s.serve(
        "graphql.json",
        &graphql(&[issue(1, Some("Todo"), &[])], &OPTIONS),
    );
    ok(s.sync(&[]));
    ok(s.devkit(&["todo", "start", "1"], &S1));
    assert_eq!(s.proj.todo("1").description, "plain");
    assert_eq!(s.proj.todo("1").status.kind(), StatusKind::InProgress);
    assert_eq!(s.todo_for("o/r#1").status, Status::Pending);
}

#[test]
fn an_unmatched_name_says_no_todo() {
    let p = Proj::new();
    added(&p, "one");
    for name in ["brave-nothing", "", "  "] {
        let out = p.devkit(&["todo", "start", name], &TASKCHAMPION);
        assert_eq!(out.status.code(), Some(1), "{name:?}");
        assert!(
            stderr(&out).contains(&format!("no todo {name}")),
            "{name:?}: {}",
            stderr(&out)
        );
    }
}

#[test]
fn an_ambiguous_name_names_each_match_and_changes_nothing() {
    let s = two_issues();
    ok(s.sync(&[]));
    let other = s.devkit(&["todo", "add", "other"], &S1);
    ok(other);
    // With no GitHub tracker in reach, `66` names every GitHub ticket 66;
    // the full key of another repository's ticket still resolves.
    let p = &s.proj;
    p.store()
        .add(devkit_todo::NewTodo {
            description: "#66 Elsewhere".into(),
            project: Some("proj".into()),
            parent: None,
            order: None,
            ticket: Some(devkit_todo::TicketRef {
                tracker: "github".into(),
                key: "x/y#66".into(),
                synced_status: None,
            }),
        })
        .unwrap();
    ok(s.devkit(&["todo", "start", "x/y#66"], &S1));
    std::fs::remove_file(p.path.join("devkit.toml")).unwrap();
    let out = p.devkit(&["todo", "stop", "66"], &S1);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(err.contains("todo name 66 is ambiguous"), "{err}");
    assert!(err.contains(&s.todo_for("o/r#66").id), "{err}");
    assert!(err.contains(&s.todo_for("x/y#66").id), "{err}");
    assert_eq!(s.todo_for("x/y#66").status.kind(), StatusKind::InProgress);
}
