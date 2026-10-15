//! `[ticket.sync] push`: `devkit todo start`, `done` and `cancel` on a ticket
//! todo move the ticket before returning, and stop instead of writing over a
//! tracker change the last sync did not see; and `devkit todo sync --ticket`
//! syncing one ticket on its own.

#[path = "common/ghfake.rs"]
mod ghfake;
#[path = "common/linearfake.rs"]
mod linearfake;
#[path = "common/tickets.rs"]
mod tickets;
#[path = "common/todoenv.rs"]
mod todoenv;

use devkit_todo::{Status, StatusKind};
use tickets::*;
use todoenv::{Proj, stderr, stdout};

const PUSH: &str = "\n[ticket.sync]\npush = [\"status\"]\n";

const DONE_EVENT: &str = "\n[ticket.events.todo_done]\nto = \"Done\"\n";

const AGENT: [(&str, &str); 1] = [("DEVKIT_CALLER", "agent")];

/// Issue 65 at `Todo` and issue 66, synced, in a repository pushing
/// statuses with `events`, with project 3 when `project`, whose writes `gh`
/// accepts. Without a project every open issue has the status `""`.
fn pushing(events: &str, project: bool) -> Synced {
    let statuses = match project {
        true => STATUSES,
        false => "\n[ticket.sync.statuses]\n\"\" = \"pending\"\n",
    };
    let s = Synced::new(&format!("{PUSH}{RULES}{statuses}{events}"), project);
    s.serve("graphql_mutation.json", r#"{"data":{}}"#);
    tracker_shows(&s, issue(65, Some("Todo"), &[66]));
    let out = s.sync(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    s
}

/// The tracker answering with `i65` as issue 65, and issue 66 as synced.
fn tracker_shows(s: &Synced, i65: String) {
    s.serve(
        "graphql.json",
        &graphql(&[i65, issue(66, None, &[])], &OPTIONS),
    );
}

/// Issue 65 at `option` on project 3.
fn at(option: &str) -> String {
    issue(65, Some(option), &[66])
}

/// Issue 65 closed for `reason`.
fn closed(reason: &str) -> String {
    issue(65, Some("Todo"), &[66]).replace(
        r#""state":"OPEN","stateReason":null"#,
        &format!(r#""state":"CLOSED","stateReason":"{reason}""#),
    )
}

/// Where among the mutations `gh` was sent the first holding `needle` is.
fn mutation_at(s: &Synced, needle: &str) -> Option<usize> {
    s.mutations().iter().position(|m| m.contains(needle))
}

fn ticket_id(s: &Synced) -> String {
    s.todo_for("o/r#65").id
}

fn synced_status(s: &Synced) -> Option<String> {
    s.todo_for("o/r#65").ticket.and_then(|t| t.synced_status)
}

#[test]
fn done_closes_then_moves_and_records_the_status() {
    let s = pushing(DONE_EVENT, true);
    let out = s.devkit(&["todo", "done", &ticket_id(&s)], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    let close = mutation_at(&s, "closeIssue").expect("closeIssue sent");
    assert!(s.mutations()[close].contains("stateReason: COMPLETED"));
    let moved = mutation_at(&s, "updateProjectV2ItemFieldValue").expect("status set");
    assert!(close < moved, "{:?}", s.mutations());
    assert!(
        s.mutations()[moved].contains("\"o2\""),
        "{:?}",
        s.mutations()
    );
    assert_eq!(s.todo_for("o/r#65").status.kind(), StatusKind::Completed);
    assert_eq!(synced_status(&s).as_deref(), Some("closed:completed"));
}

#[test]
fn done_closes_without_a_project() {
    let s = pushing("", false);
    let out = s.devkit(&["todo", "done", &ticket_id(&s)], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    let close = mutation_at(&s, "closeIssue").expect("closeIssue sent");
    assert!(s.mutations()[close].contains("stateReason: COMPLETED"));
    assert_eq!(mutation_at(&s, "updateProjectV2ItemFieldValue"), None);
    assert_eq!(s.todo_for("o/r#65").status.kind(), StatusKind::Completed);
}

#[test]
fn done_closes_when_an_event_has_no_project_to_move() {
    let s = pushing(DONE_EVENT, false);
    let out = s.devkit(&["todo", "done", &ticket_id(&s)], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        mutation_at(&s, "closeIssue").is_some(),
        "{:?}",
        s.mutations()
    );
    assert!(
        stderr(&out).contains("[ticket.events.todo_done] not moved")
            && stderr(&out).contains("[github] project"),
        "{}",
        stderr(&out)
    );
    assert_eq!(s.todo_for("o/r#65").status.kind(), StatusKind::Completed);
}

#[test]
fn cancel_closes_as_not_planned() {
    let s = pushing(DONE_EVENT, true);
    let out = s.devkit(&["todo", "cancel", &ticket_id(&s)], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    let close = mutation_at(&s, "closeIssue").expect("closeIssue sent");
    assert!(s.mutations()[close].contains("stateReason: NOT_PLANNED"));
    assert_eq!(
        mutation_at(&s, "updateProjectV2ItemFieldValue"),
        None,
        "no todo_cancel event moves the field"
    );
    assert_eq!(s.todo_for("o/r#65").status.kind(), StatusKind::Cancelled);
    assert_eq!(synced_status(&s).as_deref(), Some("closed:not_planned"));
}

#[test]
fn done_on_an_issue_already_closed_as_completed_closes_nothing() {
    let s = pushing(DONE_EVENT, true);
    tracker_shows(&s, closed("COMPLETED"));
    let out = s.devkit(&["todo", "done", &ticket_id(&s)], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(mutation_at(&s, "closeIssue"), None, "{:?}", s.mutations());
    assert_eq!(s.todo_for("o/r#65").status.kind(), StatusKind::Completed);
    assert_eq!(synced_status(&s).as_deref(), Some("closed:completed"));
}

#[test]
fn a_tracker_move_conflicts_for_an_agent() {
    let s = pushing(DONE_EVENT, true);
    tracker_shows(&s, at("Backlog"));
    let out = s.devkit(&["todo", "done", &ticket_id(&s)], &AGENT);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains("o/r#65")
            && err.contains("tracker \"Backlog\"")
            && err.contains("last synced \"Todo\"")
            && err.contains("local done")
            && err.contains("Ask the user")
            && err.contains("--overwrite-tracker"),
        "{err}"
    );
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
    assert_eq!(s.todo_for("o/r#65").status, Status::Pending);
    assert_eq!(synced_status(&s).as_deref(), Some("Todo"));
}

#[test]
fn a_tracker_move_inside_named_from_writes() {
    let s = pushing(
        "\n[ticket.events.todo_done]\nfrom = [\"Todo\", \"In review\"]\nto = \"Done\"\n",
        true,
    );
    tracker_shows(&s, at("In review"));
    let out = s.devkit(&["todo", "done", &ticket_id(&s)], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        mutation_at(&s, "closeIssue").is_some(),
        "{:?}",
        s.mutations()
    );
    assert!(mutation_at(&s, "updateProjectV2ItemFieldValue").is_some());
}

#[test]
fn overwrite_tracker_writes_over_a_conflict() {
    let s = pushing(DONE_EVENT, true);
    tracker_shows(&s, at("Backlog"));
    let out = s.devkit(
        &["todo", "done", "--overwrite-tracker", &ticket_id(&s)],
        &AGENT,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        mutation_at(&s, "closeIssue").is_some(),
        "{:?}",
        s.mutations()
    );
    assert!(mutation_at(&s, "updateProjectV2ItemFieldValue").is_some());
    assert_eq!(s.todo_for("o/r#65").status.kind(), StatusKind::Completed);
}

#[test]
fn a_conflict_without_a_terminal_fails_instead_of_prompting() {
    let s = pushing(DONE_EVENT, true);
    tracker_shows(&s, at("Backlog"));
    let human = [("DEVKIT_CALLER", "human")];
    let out = s.devkit_fed(&["todo", "done", &ticket_id(&s)], &human, "y\n");
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("--overwrite-tracker"),
        "{}",
        stderr(&out)
    );
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
    assert_eq!(s.todo_for("o/r#65").status, Status::Pending);
}

#[test]
fn an_unreadable_ticket_fails_and_changes_nothing() {
    let s = pushing(DONE_EVENT, true);
    s.serve("graphql.json", &graphql(&[issue(66, None, &[])], &OPTIONS));
    let out = s.devkit(&["todo", "done", &ticket_id(&s)], &AGENT);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("o/r#65"), "{}", stderr(&out));
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
    assert_eq!(s.todo_for("o/r#65").status, Status::Pending);

    std::fs::remove_file(s.bin.path().join("graphql.json")).unwrap();
    let out = s.devkit(&["todo", "cancel", &ticket_id(&s)], &AGENT);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("o/r#65"), "{}", stderr(&out));
    assert_eq!(s.todo_for("o/r#65").status, Status::Pending);
}

#[test]
fn steps_commit_even_when_a_ticket_conflicts() {
    let s = pushing(DONE_EVENT, true);
    let out = s.devkit(&["todo", "add", "a step"], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    let step = stdout(&out).trim().to_string();
    tracker_shows(&s, at("Backlog"));
    let ticket = ticket_id(&s);
    let out = s.devkit(&["todo", "done", &ticket, &step], &AGENT);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    let short = devkit_todo::short_id(&ticket);
    assert!(
        err.contains(&format!("done: {step}")) && err.contains(&format!("not done: {short}")),
        "{err}"
    );
    let step = s.proj.todos().into_iter().find(|t| t.id.starts_with(&step));
    assert_eq!(step.unwrap().status.kind(), StatusKind::Completed);
    assert_eq!(s.todo_for("o/r#65").status, Status::Pending);
}

#[test]
fn done_from_another_repository_commits_and_warns() {
    for (verb, kind) in [
        ("done", StatusKind::Completed),
        ("cancel", StatusKind::Cancelled),
    ] {
        let s = pushing(DONE_EVENT, true);
        let other = s.proj.outside().join("other");
        std::fs::create_dir(&other).unwrap();
        devkit_git::Git::fixture(&other)
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        std::fs::copy(s.proj.path.join("devkit.toml"), other.join("devkit.toml")).unwrap();
        let out = s.devkit_in(&other, &["todo", verb, &ticket_id(&s)], &AGENT, "");
        assert!(out.status.success(), "{verb}: {}", stderr(&out));
        assert!(
            stderr(&out).contains("ticket o/r#65 not moved: run from its repository"),
            "{verb}: {}",
            stderr(&out)
        );
        assert_eq!(s.todo_for("o/r#65").status.kind(), kind, "{verb}");
        settle();
        assert!(s.mutations().is_empty(), "{verb}: {:?}", s.mutations());
    }
}

#[test]
fn without_push_done_fires_the_background_event() {
    let s = Synced::new(&format!("{RULES}{STATUSES}{DONE_EVENT}"), true);
    s.serve("graphql_mutation.json", r#"{"data":{}}"#);
    tracker_shows(&s, at("Todo"));
    assert!(s.sync(&[]).status.success());
    tracker_shows(&s, at("Backlog"));
    let out = s.devkit(&["todo", "done", &ticket_id(&s)], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    poll("the background move to Done", || {
        mutation_at(&s, "updateProjectV2ItemFieldValue").is_some()
    });
    assert_eq!(mutation_at(&s, "closeIssue"), None);
}

#[test]
fn linear_done_sets_the_state_by_name() {
    let linear = linearfake::LinearFake::start();
    let proj = Proj::new();
    std::fs::write(
        proj.path.join("devkit.toml"),
        format!(
            "[tracker]\nkind = \"linear\"\n{PUSH}{RULES}\n[ticket.sync.statuses]\n\
             \"Todo\" = \"pending\"\n\"Done\" = \"completed\"\n{DONE_EVENT}"
        ),
    )
    .unwrap();
    let page = |state: &str| {
        serde_json::json!({ "data": { "issues": {
            "pageInfo": { "hasNextPage": false, "endCursor": null },
            "nodes": [{
                "identifier": "ENG-1", "title": "Fix it", "state": { "name": state },
                "parent": null, "inverseRelations": { "nodes": [] }
            }]
        }}})
        .to_string()
    };
    linear.answer("issues(first: 10", &page("Todo"));
    linear.answer(
        "teams(first: 10",
        r#"{"data":{"teams":{"pageInfo":{"hasNextPage":false},"nodes":[
            {"key":"ENG","states":{"nodes":[{"name":"Todo"},{"name":"Done"}]}}]}}}"#,
    );
    linear.answer(
        "issues(filter:",
        r#"{"data":{"issues":{"nodes":[{"id":"uuid-1","state":{"name":"Todo"},
            "team":{"states":{"nodes":[{"id":"st-todo","name":"Todo"},{"id":"st-done","name":"Done"}]}}}]}}}"#,
    );
    linear.answer(
        "issueUpdate",
        r#"{"data":{"issueUpdate":{"success":true}}}"#,
    );
    let mut env = linear.env().to_vec();
    env.push(("DEVKIT_CALLER", "agent"));
    let out = proj.devkit(&["todo", "sync"], &env);
    assert!(out.status.success(), "{}", stderr(&out));
    let todo = proj
        .todos()
        .into_iter()
        .find(|t| t.ticket.as_ref().is_some_and(|r| r.key == "ENG-1"))
        .expect("ENG-1 synced");

    let out = proj.devkit(&["todo", "done", &todo.id], &env);
    assert!(out.status.success(), "{}", stderr(&out));
    let updates = linear.sent("issueUpdate");
    assert_eq!(updates.len(), 1, "{updates:?}");
    let query = updates[0]["query"].as_str().unwrap();
    assert!(
        query.contains("\"uuid-1\"") && query.contains("stateId: \"st-done\""),
        "{query}"
    );
    let todo = proj.todo(&todo.id);
    assert_eq!(todo.status.kind(), StatusKind::Completed);
    let synced = todo.ticket.and_then(|t| t.synced_status);
    assert_eq!(synced.as_deref(), Some("Done"));
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
