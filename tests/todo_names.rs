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
fn start_takes_an_issue_url_of_the_configured_repo() {
    let s = two_issues();
    ok(s.sync(&[]));
    ok(s.devkit(&["todo", "start", "https://github.com/o/r/issues/66"], &S1));
    assert_eq!(s.todo_for("o/r#66").status.kind(), StatusKind::InProgress);
    assert_eq!(s.todo_for("o/r#65").status, Status::Pending);
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

/// Two ticket todos on a taskchampion replica, sharing a `digits`-long
/// name: one of `x/y` whose uuid starts with those decimal digits, and
/// `o/r`'s issue of that number, which the tracker reads them as. Returns
/// the digits and each todo's id, the `x/y` one first.
fn prefix_like_a_ticket(s: &Synced, digits: usize) -> (String, String, String) {
    let (key, prefix) = (1..)
        .map(|n| format!("x/y#{n}"))
        .find_map(|key| {
            let id = devkit_todo::ticket_todo_uuid("", "github", &key).to_string();
            let prefix = &id[..digits];
            prefix
                .bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| (key.clone(), prefix.to_string()))
        })
        .unwrap();
    let number: u64 = prefix.parse().unwrap();
    let store = TaskchampionStore::at(s.proj.state().join("todo/taskchampion"));
    let add = |key: &str| {
        store
            .add(devkit_todo::NewTodo {
                description: key.into(),
                project: Some("proj".into()),
                parent: None,
                order: None,
                ticket: Some(devkit_todo::TicketRef {
                    tracker: "github".into(),
                    key: key.into(),
                    synced_status: None,
                }),
            })
            .unwrap()
    };
    let elsewhere = add(&key);
    let numbered = add(&format!("o/r#{number}"));
    (prefix, elsewhere, numbered)
}

#[test]
fn an_ambiguous_name_names_each_match_and_changes_nothing() {
    let s = Synced::new(RULES, false);
    let (prefix, elsewhere, numbered) = prefix_like_a_ticket(&s, 4);
    ok(s.devkit(&["todo", "start", &elsewhere], &TASKCHAMPION));
    let out = s.devkit(&["todo", "stop", &prefix], &TASKCHAMPION);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(
        err.contains(&format!("todo name {prefix} is ambiguous")),
        "{err}"
    );
    assert!(err.contains(&elsewhere[..8]), "{err}");
    assert!(err.contains(&numbered[..8]), "{err}");
    assert_eq!(
        replica_todo(&s.proj, &elsewhere).status.kind(),
        StatusKind::InProgress
    );
}

#[test]
fn a_uuid_prefix_that_is_also_a_ticket_number_is_ambiguous() {
    let s = Synced::new(RULES, false);
    let (prefix, elsewhere, numbered) = prefix_like_a_ticket(&s, 8);
    let out = s.devkit(&["todo", "start", &prefix], &TASKCHAMPION);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains(&format!("todo name {prefix} is ambiguous")),
        "{err}"
    );
    assert!(err.contains(&elsewhere[..8]), "{err}");
    assert!(err.contains(&numbered[..8]), "{err}");
    for id in [elsewhere, numbered] {
        assert_eq!(replica_todo(&s.proj, &id).status, Status::Pending);
    }
}

#[test]
fn a_number_with_a_non_github_tracker_is_not_a_key() {
    let s = two_issues();
    ok(s.sync(&[]));
    std::fs::write(
        s.proj.path.join("devkit.toml"),
        "[tracker]\nkind = \"linear\"\n",
    )
    .unwrap();
    for name in ["66", "#66"] {
        let out = s.devkit(&["todo", "start", name], &S1);
        assert_eq!(out.status.code(), Some(1), "{name}");
        assert!(
            stderr(&out).contains(&format!("no todo {name}")),
            "{}",
            stderr(&out)
        );
    }
    assert_eq!(s.todo_for("o/r#66").status, Status::Pending);
}

#[test]
fn an_issue_url_resolves_with_no_tracker_in_reach() {
    let s = two_issues();
    ok(s.sync(&[]));
    std::fs::remove_file(s.proj.path.join("devkit.toml")).unwrap();
    let out = s.devkit(&["todo", "start", "66"], &S1);
    assert!(stderr(&out).contains("no todo 66"), "{}", stderr(&out));
    ok(s.devkit(&["todo", "start", "https://github.com/O/R/issues/66"], &S1));
    assert_eq!(s.todo_for("o/r#66").status.kind(), StatusKind::InProgress);
    assert_eq!(s.todo_for("o/r#65").status, Status::Pending);
}

#[test]
fn a_purged_blocker_numbered_like_tickets_names_none_of_them() {
    let s = Synced::new(&format!("{RULES}{STATUSES}"), true);
    for text in ["one", "two", "three", "four", "five"] {
        ok(s.devkit(&["todo", "add", text], &S1));
    }
    ok(s.devkit(&["todo", "block", "5", "--by", "3"], &S1));
    ok(s.devkit(&["todo", "purge", "3"], &[("DEVKIT_CALLER", "human")]));
    s.serve(
        "graphql.json",
        &graphql(&[issue(3, Some("Todo"), &[])], &OPTIONS),
    );
    ok(s.sync(&[]));
    let out = s.devkit(&["todo", "ready"], &S1);
    assert!(out.status.success(), "{}", stderr(&out));
    let ready = String::from_utf8_lossy(&out.stdout);
    for text in ["one", "two", "four", "five"] {
        assert_eq!(ready.matches(text).count(), 1, "{text}: {ready}");
    }
    let ticket = s.todo_for("o/r#3").id;
    assert!(!s.proj.todo("5").blocked_by.contains(&ticket));
}
