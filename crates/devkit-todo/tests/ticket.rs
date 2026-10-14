//! The sync planner: fetched tickets diffed against the ticket todos on a
//! repository's node.

use devkit_common::tracker::tickets::{CloseReason, SyncedTicket, TicketStatus};
use devkit_config::{Config, TicketSyncConfig};
use devkit_todo::{
    Holder, Status, StatusKind, TicketRef, Todo,
    ticket::{Change, PlannedEdit, SyncPlan, plan, status_name, todo_text},
};

const NODE: &str = "devkit";

fn config() -> TicketSyncConfig {
    Config::parse(
        r#"
[[ticket.sync.rules]]
assignee = "me"

[ticket.sync.statuses]
"" = "pending"
"Todo" = "pending"
"In progress" = "in_progress"
"Done" = "completed"
"#,
    )
    .unwrap()
    .ticket
    .sync
}

fn known() -> Vec<String> {
    ["", "Todo", "In progress", "Done"].map(String::from).into()
}

fn ticket(key: &str, status: &str) -> SyncedTicket {
    SyncedTicket {
        key: key.into(),
        title: format!("Ticket {key}"),
        status: TicketStatus::Named(status.into()),
        blocked_by: Vec::new(),
        parent: None,
    }
}

/// The todo a sync made for `key`, last synced as `synced`, at id `t<key>`.
fn todo(key: &str, synced: &str, status: Status) -> Todo {
    Todo {
        id: format!("t{key}"),
        description: todo_text("github", key, &format!("Ticket {key}")),
        status,
        parent: None,
        order: Some(1024),
        project: Some(NODE.into()),
        entry: None,
        modified: None,
        blocked_by: Vec::new(),
        ticket: Some(TicketRef {
            tracker: "github".into(),
            key: key.into(),
            synced_status: Some(synced.into()),
        }),
    }
}

fn run(existing: &[Todo], fetched: &[SyncedTicket]) -> SyncPlan {
    plan("github", NODE, existing, fetched, &known(), &config()).unwrap()
}

fn edit(key: &str, change: Change) -> PlannedEdit {
    PlannedEdit {
        key: key.into(),
        change,
    }
}

#[test]
fn a_new_ticket_becomes_one_todo_on_the_node() {
    let out = run(&[], &[ticket("65", "In progress")]);
    assert_eq!(out.adds.len(), 1, "{out:?}");
    let add = &out.adds[0];
    assert_eq!(add.description, "#65 Ticket 65");
    assert_eq!(add.project.as_deref(), Some(NODE));
    assert_eq!(
        add.ticket,
        Some(TicketRef {
            tracker: "github".into(),
            key: "65".into(),
            synced_status: Some("In progress".into()),
        })
    );
    assert_eq!(out.edits, [edit(
        "65",
        Change::SetStatus(StatusKind::InProgress)
    )]);
    let resolved = out.edits[0]
        .resolve(&[("65".to_string(), "9".to_string())].into())
        .unwrap();
    assert_eq!(resolved, devkit_todo::Edit::SetStatus {
        id: "9".into(),
        to: StatusKind::InProgress,
        actor: Holder::tracker(),
    });
}

#[test]
fn todo_text_names_the_ticket_as_its_tracker_does() {
    assert_eq!(todo_text("github", "65", "Add sync"), "#65 Add sync");
    assert_eq!(todo_text("github", "o/r#65", "Add sync"), "#65 Add sync");
    assert_eq!(
        todo_text("linear", "SWE-123", "Fix the\nplate loader"),
        "SWE-123 Fix the plate loader"
    );
}

#[test]
fn an_unmapped_status_fails_naming_it() {
    let err = plan(
        "github",
        NODE,
        &[],
        &[ticket("65", "Todo"), ticket("66", "Blocked")],
        &known(),
        &config(),
    )
    .unwrap_err();
    let shown = format!("{err:#}");
    assert!(
        shown.contains("\"Blocked\"") && shown.contains("66"),
        "{shown}"
    );
}

#[test]
fn a_mapped_name_the_tracker_lacks_fails() {
    let mut config = config();
    config
        .statuses
        .insert("Doing".into(), devkit_config::MappedStatus::InProgress);
    let err = plan(
        "github",
        NODE,
        &[],
        &[ticket("65", "Todo")],
        &known(),
        &config,
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("\"Doing\""), "{err:#}");
}

#[test]
fn an_agents_start_survives_an_unmoved_ticket() {
    let started = todo("65", "Todo", Status::InProgress {
        by: Holder::new("S"),
    });
    let out = run(&[started], &[ticket("65", "Todo")]);
    assert!(out.is_empty(), "{out:?}");
}

#[test]
fn a_moved_ticket_moves_its_todo_as_tracker() {
    let started = todo("65", "Todo", Status::InProgress {
        by: Holder::new("S"),
    });
    let out = run(&[started], &[ticket("65", "Done")]);
    assert_eq!(out.edits, [
        edit("65", Change::SetStatus(StatusKind::Completed)),
        edit("65", Change::RecordSync("Done".into())),
    ]);
    assert!(out.adds.is_empty());
}

#[test]
fn a_closed_github_issue_maps_by_reason() {
    let closed = |reason| SyncedTicket {
        status: TicketStatus::Closed(reason),
        ..ticket("65", "")
    };
    let existing = [todo("65", "In progress", Status::Pending)];
    let out = run(&existing, &[closed(CloseReason::NotPlanned)]);
    assert_eq!(
        out.edits[0],
        edit("65", Change::SetStatus(StatusKind::Cancelled))
    );
    assert_eq!(
        out.edits[1],
        edit("65", Change::RecordSync("closed:not_planned".into()))
    );
    let out = run(&existing, &[closed(CloseReason::Completed)]);
    assert_eq!(
        out.edits[0],
        edit("65", Change::SetStatus(StatusKind::Completed))
    );
    assert_eq!(
        status_name(&TicketStatus::Closed(CloseReason::Completed)),
        "closed:completed"
    );
}

#[test]
fn blocking_links_between_synced_tickets_become_edges() {
    let blocked = SyncedTicket {
        blocked_by: vec!["66".into(), "99".into()],
        ..ticket("65", "Todo")
    };
    let out = run(&[], &[blocked, ticket("66", "Todo")]);
    assert_eq!(out.edits, [edit("65", Change::Block(vec!["66".into()]))]);
}

#[test]
fn a_sub_issue_nests_under_its_parent_ticket() {
    let child = SyncedTicket {
        parent: Some("60".into()),
        ..ticket("65", "Todo")
    };
    let out = run(&[], &[ticket("60", "Todo"), child.clone()]);
    assert_eq!(out.edits, [edit("65", Change::Move(Some("60".into())))]);
    let existing = [todo("60", "Todo", Status::Pending), Todo {
        parent: Some("t60".into()),
        ..todo("65", "Todo", Status::Pending)
    }];
    assert!(run(&existing, &[ticket("60", "Todo"), child]).is_empty());
}

#[test]
fn local_children_and_edges_are_left_alone() {
    let local = Todo {
        id: "local".into(),
        parent: Some("t65".into()),
        ticket: None,
        ..todo("x", "", Status::Pending)
    };
    let ticket_todo = Todo {
        blocked_by: vec!["local".into(), "t66".into()],
        ..todo("65", "Todo", Status::Pending)
    };
    let nested = Todo {
        parent: Some("local".into()),
        ..todo("66", "Todo", Status::Pending)
    };
    let out = run(&[local, ticket_todo, nested], &[
        ticket("65", "Todo"),
        ticket("66", "Todo"),
    ]);
    assert_eq!(out.edits, [edit("65", Change::Unblock(vec!["66".into()]))]);
}

#[test]
fn a_renamed_ticket_is_described_again() {
    let existing = [todo("65", "Todo", Status::Pending)];
    let renamed = SyncedTicket {
        title: "Add ticket sync".into(),
        ..ticket("65", "Todo")
    };
    let out = run(&existing, &[renamed]);
    assert_eq!(out.edits, [edit(
        "65",
        Change::Describe("#65 Add ticket sync".into())
    )]);
}

#[test]
fn render_names_each_change_by_ticket() {
    let blocked = SyncedTicket {
        blocked_by: vec!["66".into()],
        ..ticket("65", "In progress")
    };
    let shown = run(&[], &[blocked, ticket("66", "Todo")]).render();
    assert_eq!(
        shown,
        "add #65 Ticket 65\nadd #66 Ticket 66\nstatus 65 -> in progress\nblock 65 by 66\n"
    );
}
