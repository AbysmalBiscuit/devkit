//! `devkit todo sync --ticket` syncing one ticket on its own.

#[path = "common/ghfake.rs"]
mod ghfake;
#[path = "common/tickets.rs"]
mod tickets;
#[path = "common/todoenv.rs"]
mod todoenv;

use devkit_todo::{Status, StatusKind};
use tickets::*;
use todoenv::stderr;

/// Issue 65 at `option` on project 3.
fn at(option: &str) -> String {
    issue(65, Some(option), &[66])
}

/// The tracker answering with `i65` as issue 65, and issue 66 as synced.
fn tracker_shows(s: &Synced, i65: String) {
    s.serve("graphql.json", &graphql(&[i65, issue(66, None, &[])], &OPTIONS));
}

/// Issues 65 and 66 synced, then 65 moved to `In progress` on the tracker
/// and 67 added there as its sub-issue, which no sync has brought in.
fn with_sub_issue() -> Synced {
    let s = Synced::new(&format!("{RULES}{STATUSES}"), true);
    tracker_shows(&s, at("Todo"));
    assert!(s.sync(&[]).status.success());
    let i67 = issue(67, Some("In progress"), &[]).replace(
        r#""parent":null"#,
        r#""parent":{"number":65,"repository":{"nameWithOwner":"o/r"}}"#,
    );
    s.serve(
        "graphql.json",
        &graphql(&[at("In progress"), issue(66, None, &[]), i67], &OPTIONS),
    );
    s
}

#[test]
fn sync_one_adds_the_ticket_under_its_synced_parent() {
    let s = with_sub_issue();
    let searches = s.ticket_queries();
    let out = s.sync(&["--ticket", "o/r#67"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let (t65, t67) = (s.todo_for("o/r#65"), s.todo_for("o/r#67"));
    assert_eq!(t67.parent.as_deref(), Some(t65.id.as_str()));
    assert_eq!(t67.node(), "proj");
    assert_eq!(t67.status.kind(), StatusKind::InProgress);
    assert_eq!(
        s.ticket_queries(),
        searches,
        "no rule search: {}",
        s.calls()
    );
}

#[test]
fn sync_one_leaves_other_tickets_alone() {
    let s = with_sub_issue();
    let out = s.sync(&["--ticket", "o/r#66"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        s.todo_for("o/r#65").status,
        Status::Pending,
        "65 moved on the tracker, but only 66 synced"
    );
    assert!(
        !s.proj
            .todos()
            .iter()
            .any(|t| t.ticket.as_ref().is_some_and(|r| r.key == "o/r#67"))
    );
}

#[test]
fn sync_one_without_rules_does_nothing() {
    let s = Synced::new(STATUSES, true);
    let out = s.sync(&["--ticket", "o/r#65"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!s.calls().contains("api graphql"), "{}", s.calls());
    assert!(s.proj.todos().is_empty());
}
