//! `devkit todo` driven as a person and as an agent would run it.

#[path = "common/todoenv.rs"]
mod todoenv;

use devkit_todo::{Holder, Status, TodoStore};
use serde_json::Value;
use todoenv::{Proj, stderr, stdout};

const S1: [(&str, &str); 1] = [("CLAUDE_CODE_SESSION_ID", "s1")];
const HUMAN: [(&str, &str); 1] = [("DEVKIT_CALLER", "human")];

#[test]
fn add_prints_the_id_and_list_shows_it() {
    let p = Proj::new();
    let add = p.devkit(&["todo", "add", "one"], &S1);
    assert!(add.status.success(), "{}", stderr(&add));
    assert_eq!(stdout(&add), "1\n");
    let list = stdout(&p.devkit(&["todo", "list"], &S1));
    assert!(
        list.contains("## proj.main.claude-s1\n- [ ] one (1)\n"),
        "{list}"
    );
}

#[test]
fn scope_prefers_codex() {
    let p = Proj::new();
    let out = p.devkit(&["todo", "scope"], &[
        ("CLAUDE_CODE_SESSION_ID", "k1"),
        ("CODEX_SESSION_ID", "c1"),
    ]);
    assert_eq!(stdout(&out), "proj.main.codex-c1\n");
}

#[test]
fn outside_a_repository_everything_is_global() {
    let p = Proj::new();
    let scope = p.devkit_in(p.outside(), &["todo", "scope"], &S1, "");
    assert_eq!(stdout(&scope), "global\n");
    let add = p.devkit_in(p.outside(), &["todo", "add", "far"], &S1, "");
    assert!(add.status.success(), "{}", stderr(&add));
    assert_eq!(p.todo("1").project, None);
}

#[test]
fn an_agent_without_a_session_acts_as_agent() {
    let p = Proj::new();
    let agent = [("DEVKIT_CALLER", "agent")];
    assert_eq!(stdout(&p.devkit(&["todo", "scope"], &agent)), "proj.main\n");
    p.devkit(&["todo", "add", "one"], &agent);
    let start = p.devkit(&["todo", "start", "1"], &agent);
    assert!(start.status.success(), "{}", stderr(&start));
    assert_eq!(p.todo("1").status, Status::InProgress {
        by: Holder::new("agent")
    });
    let json: Value =
        serde_json::from_str(&stdout(&p.devkit(&["todo", "list", "--json"], &agent))).unwrap();
    assert_eq!(json[0]["started"], true);
}

#[test]
fn a_sibling_start_is_refused_naming_the_holder() {
    let p = Proj::new();
    p.devkit(&["todo", "add", "one"], &HUMAN);
    assert!(p.devkit(&["todo", "start", "1"], &HUMAN).status.success());
    let other = p.devkit(&["todo", "start", "1"], &[("CLAUDE_CODE_SESSION_ID", "s2")]);
    assert_eq!(other.status.code(), Some(1));
    assert!(
        stderr(&other).contains("in progress by human"),
        "{}",
        stderr(&other)
    );
}

#[test]
fn unknown_ids_fail_cleanly() {
    let p = Proj::new();
    p.devkit(&["todo", "add", "one"], &S1);
    let before = p.todos();
    for verb in ["start", "stop", "done", "undone", "cancel"] {
        let out = p.devkit(&["todo", verb, "99"], &S1);
        assert_eq!(out.status.code(), Some(1), "{verb}");
        assert!(
            stderr(&out).contains("no todo 99"),
            "{verb}: {}",
            stderr(&out)
        );
    }
    let describe = p.devkit(&["todo", "describe", "99", "x"], &S1);
    assert!(stderr(&describe).contains("no todo 99"));
    assert_eq!(p.todos(), before);
}

#[test]
fn purge_needs_a_human() {
    let p = Proj::new();
    p.devkit(&["todo", "add", "secret"], &HUMAN);
    let agent = p.devkit(&["todo", "purge", "1"], &[("DEVKIT_CALLER", "agent")]);
    assert_eq!(agent.status.code(), Some(1));
    assert!(stderr(&agent).contains("terminal"), "{}", stderr(&agent));
    assert_eq!(p.todos().len(), 1);
    let human = p.devkit(&["todo", "purge", "1"], &HUMAN);
    assert!(human.status.success(), "{}", stderr(&human));
    assert!(p.todos().is_empty());
}

#[test]
fn json_matches_alacritrees_task_shape() {
    let p = Proj::new();
    for text in ["one", "two", "three"] {
        p.devkit(&["todo", "add", text], &S1);
    }
    p.devkit(&["todo", "start", "1"], &S1);
    p.devkit(&["todo", "cancel", "2"], &S1);
    p.devkit(&["todo", "done", "3"], &S1);
    let json: Value =
        serde_json::from_str(&stdout(&p.devkit(&["todo", "list", "--json"], &S1))).unwrap();
    let rows = json.as_array().unwrap();
    assert_eq!(rows.len(), 2, "a cancelled todo is left out: {json}");
    let started = &rows[0];
    assert_eq!(started["id"], "1");
    assert_eq!(started["description"], "one");
    assert_eq!(started["status"], "pending");
    assert_eq!(started["started"], true);
    assert_eq!(started["project"], "proj.main.claude-s1");
    assert_eq!(started["order"], 1024);
    assert_eq!(started["parent"], Value::Null);
    assert!(started["entry"].as_str().is_some_and(|e| e.ends_with('Z')));
    assert!(started["modified"].is_string());
    assert_eq!(rows[1]["status"], "completed");
    assert_eq!(rows[1]["started"], false);
}

#[test]
fn move_nests_and_reorders() {
    let p = Proj::new();
    for text in ["a", "b"] {
        p.devkit(&["todo", "add", text], &S1);
    }
    let moved = p.devkit(&["todo", "move", "2", "--parent", "1", "--order", "5"], &S1);
    assert!(moved.status.success(), "{}", stderr(&moved));
    assert_eq!(p.todo("2").parent.as_deref(), Some("1"));
    p.devkit(&["todo", "move", "2", "--order", "7"], &S1);
    assert_eq!(
        p.todo("2").parent.as_deref(),
        Some("1"),
        "order alone keeps the parent"
    );
    assert_eq!(p.todo("2").order, Some(7));
}

/// A checkout git could not be asked about is not "outside any repository",
/// so nothing lands on the global list by mistake.
#[test]
fn a_failed_repository_lookup_is_an_error_not_global() {
    let p = Proj::new();
    let no_git = tempfile::tempdir().unwrap();
    let path = no_git.path().to_str().unwrap();
    let scope = p.devkit(&["todo", "scope"], &[("PATH", path), S1[0]]);
    assert_eq!(scope.status.code(), Some(1), "{}", stdout(&scope));
    let add = p.devkit(&["todo", "add", "lost"], &[("PATH", path), S1[0]]);
    assert_eq!(add.status.code(), Some(1));
    assert!(p.todos().is_empty());
}

#[test]
fn a_batch_with_one_claimed_todo_changes_nothing() {
    let p = Proj::new();
    for text in ["one", "two"] {
        p.devkit(&["todo", "add", text], &S1);
    }
    p.store()
        .apply(&devkit_todo::Edit::SetStatus {
            id: "2".into(),
            to: devkit_todo::StatusKind::InProgress,
            actor: Holder::new("s1/a2"),
        })
        .unwrap();
    let out = p.devkit(&["todo", "start", "1", "2"], &[(
        "CLAUDE_CODE_SESSION_ID",
        "t",
    )]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("in progress by s1/a2"),
        "{}",
        stderr(&out)
    );
    assert_eq!(p.todo("1").status, Status::Pending);
    let unknown = p.devkit(&["todo", "done", "1", "99"], &S1);
    assert!(stderr(&unknown).contains("no todo 99"));
    assert_eq!(p.todo("1").status, Status::Pending);
}
