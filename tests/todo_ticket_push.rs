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
    let moved = mutation_at(&s, "updateProjectV2ItemFieldValue").expect("status moved");
    assert!(
        s.mutations()[moved].contains("\"o2\""),
        "{:?}",
        s.mutations()
    );
    assert_eq!(s.todo_for("o/r#65").status.kind(), StatusKind::Completed);
    assert_eq!(synced_status(&s).as_deref(), Some("closed:completed"));
}

#[test]
fn a_move_failing_after_the_close_finishes_on_a_plain_rerun() {
    let s = pushing(DONE_EVENT, true);
    s.serve(
        "graphql_mutation_2.json",
        r#"{"errors":[{"message":"field write refused"}]}"#,
    );
    let out = s.devkit(&["todo", "done", &ticket_id(&s)], &AGENT);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains("ticket o/r#65 closed")
            && err.contains("Rerun `devkit todo done")
            && err.contains("needs no --overwrite-tracker"),
        "{err}"
    );
    assert_eq!(s.todo_for("o/r#65").status, Status::Pending);

    std::fs::remove_file(s.bin.path().join("graphql_mutation_2.json")).unwrap();
    tracker_shows(&s, closed("COMPLETED"));
    let out = s.devkit(&["todo", "done", &ticket_id(&s)], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    let moves = s
        .mutations()
        .iter()
        .filter(|m| m.contains("updateProjectV2ItemFieldValue"))
        .count();
    assert_eq!(moves, 2, "{:?}", s.mutations());
    assert_eq!(s.todo_for("o/r#65").status.kind(), StatusKind::Completed);
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
fn overwrite_recloses_an_issue_closed_as_not_planned_as_completed() {
    let s = pushing("", true);
    s.serve(
        "graphql_mutation.json",
        r#"{"data":{"closeIssue":{"issue":{"state":"CLOSED","stateReason":"COMPLETED"}}}}"#,
    );
    tracker_shows(&s, closed("NOT_PLANNED"));
    let out = s.devkit(
        &["todo", "done", "--overwrite-tracker", &ticket_id(&s)],
        &AGENT,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let close = mutation_at(&s, "closeIssue").expect("closeIssue sent");
    assert!(
        s.mutations()[close].contains("stateReason: COMPLETED"),
        "{:?}",
        s.mutations()
    );
    assert_eq!(s.todo_for("o/r#65").status.kind(), StatusKind::Completed);
    assert_eq!(synced_status(&s).as_deref(), Some("closed:completed"));
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

#[cfg(target_os = "linux")]
#[test]
fn declining_before_the_replica_pulled_is_a_decline_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let replica = dir.path().join("replica");
    let config = format!(
        "[todo]\nbackend = \"taskchampion\"\n[todo.taskchampion]\ndata_dir = {}\nserver_dir = {}\n",
        toml::Value::String(replica.display().to_string()),
        toml::Value::String(dir.path().join("server").display().to_string())
    );
    let s = Synced::in_proj(
        Proj::with_home_config(&config),
        &format!("{PUSH}{RULES}{STATUSES}{DONE_EVENT}"),
        true,
    );
    tracker_shows(&s, at("Todo"));
    let out = s.sync(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let listed = |s: &Synced| -> serde_json::Value {
        let out = s.devkit(&["todo", "list", "--all", "--json"], &[]);
        assert!(out.status.success(), "{}", stderr(&out));
        let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout(&out)).unwrap();
        rows.into_iter()
            .find(|row| row["description"].as_str().unwrap().contains("Issue 65"))
            .unwrap_or_else(|| panic!("no todo for 65: {}", stdout(&out)))
    };
    let id = listed(&s)["id"].as_str().unwrap().to_string();
    std::fs::remove_file(replica.join("sync.pulled")).unwrap();
    tracker_shows(&s, at("Backlog"));
    let human = [("DEVKIT_CALLER", "human")];
    let out = s.devkit_on_a_terminal(&["todo", "done", &id], &human, "n\n");
    let said = stdout(&out);
    assert!(out.status.success(), "{said}");
    assert!(
        said.contains("Write over the tracker?") && said.contains("declined"),
        "{said}"
    );
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
    assert_eq!(listed(&s)["status"], "pending");
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
fn sync_one_takes_the_key_in_any_spelling_the_tracker_reads() {
    for key in ["O/R#65", "#65", "65"] {
        let s = with_sub_issue();
        let out = s.sync(&["--ticket", key]);
        assert!(out.status.success(), "{key}: {}", stderr(&out));
        assert!(
            stdout(&out).contains("ticket o/r#65 synced"),
            "{key}: {}",
            stdout(&out)
        );
        assert_eq!(
            s.todo_for("o/r#65").status.kind(),
            StatusKind::InProgress,
            "{key}"
        );
    }
}

#[test]
fn sync_one_without_rules_does_nothing() {
    let s = Synced::new(STATUSES, true);
    let out = s.sync(&["--ticket", "o/r#65"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!s.calls().contains("api graphql"), "{}", s.calls());
    assert!(s.proj.todos().is_empty());
}

/// Issues 65, 66 and 67, with 67 a sub-issue of `parent` when one is given
/// and 65 blocked by 66 when `blocked`, synced into a repository pushing
/// `push`, whose writes `gh` accepts.
fn linked(push: &str, parent: Option<u64>, blocked: bool) -> Synced {
    let s = Synced::new(
        &format!("\n[ticket.sync]\npush = [{push}]\n{RULES}{STATUSES}"),
        true,
    );
    s.serve("graphql_mutation.json", r#"{"data":{}}"#);
    links_show(&s, parent, blocked);
    let out = s.sync(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    s
}

/// The tracker answering with 67 under `parent` and 65 blocked by 66 when
/// `blocked`.
fn links_show(s: &Synced, parent: Option<u64>, blocked: bool) {
    let i67 = issue(67, Some("Todo"), &[]);
    let i67 = match parent {
        Some(p) => under(i67, p),
        None => i67,
    };
    let blockers: &[u64] = if blocked { &[66] } else { &[] };
    s.serve(
        "graphql.json",
        &graphql(
            &[
                issue(65, Some("Todo"), blockers),
                issue(66, Some("Todo"), &[]),
                i67,
            ],
            &OPTIONS,
        ),
    );
}

fn id_of(s: &Synced, n: u64) -> String {
    s.todo_for(&format!("o/r#{n}")).id
}

fn parent_of(s: &Synced, n: u64) -> Option<String> {
    s.todo_for(&format!("o/r#{n}")).parent
}

#[test]
fn move_under_a_ticket_makes_a_sub_issue() {
    let s = linked("\"parent\"", None, false);
    let out = s.devkit(
        &["todo", "move", &id_of(&s, 67), "--parent", &id_of(&s, 65)],
        &AGENT,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(s.mutations().len(), 1, "{:?}", s.mutations());
    assert!(
        s.mutations()[0].contains(
            "addSubIssue(input: {issueId: \"I_65\", subIssueId: \"I_67\", replaceParent: false})"
        ),
        "{:?}",
        s.mutations()
    );
    assert_eq!(parent_of(&s, 67), Some(id_of(&s, 65)));
}

#[test]
fn move_top_removes_the_tracker_parent() {
    let s = linked("\"parent\"", Some(65), false);
    assert_eq!(parent_of(&s, 67), Some(id_of(&s, 65)));
    let out = s.devkit(&["todo", "move", &id_of(&s, 67), "--top"], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(s.mutations().len(), 1, "{:?}", s.mutations());
    assert!(
        s.mutations()[0]
            .contains("removeSubIssue(input: {issueId: \"I_65\", subIssueId: \"I_67\"})"),
        "{:?}",
        s.mutations()
    );
    assert_eq!(parent_of(&s, 67), None);
}

#[test]
fn a_third_parent_on_the_tracker_conflicts() {
    let s = linked("\"parent\"", None, false);
    links_show(&s, Some(66), false);
    let out = s.devkit(
        &["todo", "move", &id_of(&s, 67), "--parent", &id_of(&s, 65)],
        &AGENT,
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains("o/r#67")
            && err.contains("tracker \"o/r#66\"")
            && err.contains("last synced \"(none)\"")
            && err.contains("local \"o/r#65\"")
            && err.contains("--overwrite-tracker"),
        "{err}"
    );
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
    assert_eq!(parent_of(&s, 67), None);
}

#[test]
fn overwrite_sends_replace_parent_true() {
    let s = linked("\"parent\"", None, false);
    links_show(&s, Some(66), false);
    let out = s.devkit(
        &[
            "todo",
            "move",
            &id_of(&s, 67),
            "--parent",
            &id_of(&s, 65),
            "--overwrite-tracker",
        ],
        &AGENT,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(s.mutations().len(), 1, "{:?}", s.mutations());
    assert!(
        s.mutations()[0].contains(
            "addSubIssue(input: {issueId: \"I_65\", subIssueId: \"I_67\", replaceParent: true})"
        ),
        "{:?}",
        s.mutations()
    );
    assert_eq!(parent_of(&s, 67), Some(id_of(&s, 65)));
}

#[test]
fn block_between_tickets_adds_blocked_by() {
    let s = linked("\"blocking\"", None, false);
    let out = s.devkit(
        &["todo", "block", &id_of(&s, 65), "--by", &id_of(&s, 66)],
        &AGENT,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(s.mutations().len(), 1, "{:?}", s.mutations());
    assert!(
        s.mutations()[0]
            .contains("addBlockedBy(input: {issueId: \"I_65\", blockingIssueId: \"I_66\"})"),
        "{:?}",
        s.mutations()
    );
    assert_eq!(s.todo_for("o/r#65").blocked_by, [id_of(&s, 66)]);

    let out = s.devkit(
        &["todo", "unblock", &id_of(&s, 65), "--by", &id_of(&s, 66)],
        &AGENT,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        s.mutations().len(),
        1,
        "the tracker never showed the link: {:?}",
        s.mutations()
    );
    assert!(s.todo_for("o/r#65").blocked_by.is_empty());
}

#[test]
fn unblock_of_a_link_the_tracker_lacks_is_already_there() {
    let s = linked("\"blocking\"", None, true);
    assert_eq!(s.todo_for("o/r#65").blocked_by, [id_of(&s, 66)]);
    links_show(&s, None, false);
    let out = s.devkit(
        &["todo", "unblock", &id_of(&s, 65), "--by", &id_of(&s, 66)],
        &AGENT,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
    assert!(s.todo_for("o/r#65").blocked_by.is_empty());

    let s = linked("\"parent\"", Some(65), false);
    links_show(&s, None, false);
    let out = s.devkit(&["todo", "move", &id_of(&s, 67), "--top"], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
    assert_eq!(parent_of(&s, 67), None);
}

#[test]
fn a_relation_edit_outside_the_repository_is_refused() {
    let s = linked("\"parent\", \"blocking\"", None, false);
    let other = s.proj.outside().join("other");
    std::fs::create_dir(&other).unwrap();
    devkit_git::Git::fixture(&other)
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    std::fs::copy(s.proj.path.join("devkit.toml"), other.join("devkit.toml")).unwrap();
    let block = ["todo", "block", &id_of(&s, 65), "--by", &id_of(&s, 66)].map(String::from);
    let parent = ["todo", "move", &id_of(&s, 67), "--parent", &id_of(&s, 65)].map(String::from);
    for args in [block, parent] {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = s.devkit_in(&other, &args, &AGENT, "");
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", stderr(&out));
        assert!(
            stderr(&out).contains("from a checkout of it"),
            "{args:?}: {}",
            stderr(&out)
        );
    }
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
    assert!(s.todo_for("o/r#65").blocked_by.is_empty());
    assert_eq!(parent_of(&s, 67), None);
}

#[test]
fn a_relation_edit_without_the_opt_in_is_refused() {
    let s = linked("\"status\"", None, false);
    let block = ["todo", "block", &id_of(&s, 65), "--by", &id_of(&s, 66)].map(String::from);
    let parent = ["todo", "move", &id_of(&s, 67), "--parent", &id_of(&s, 65)].map(String::from);
    for (args, kind) in [(block, "\"blocking\""), (parent, "\"parent\"")] {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = s.devkit(&args, &AGENT);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", stderr(&out));
        let err = stderr(&out);
        assert!(
            err.contains(&format!("{kind} in [ticket.sync] push")) && err.contains("next sync"),
            "{args:?}: {err}"
        );
    }
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
    assert!(s.todo_for("o/r#65").blocked_by.is_empty());
    assert_eq!(parent_of(&s, 67), None);
}

#[test]
fn add_under_a_ticket_is_refused_naming_promote() {
    let s = linked("\"parent\"", None, false);
    let before = s.proj.todos().len();
    let out = s.devkit(
        &["todo", "add", "a step", "--parent", &id_of(&s, 65)],
        &AGENT,
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains("ticket create") && err.contains("todo promote"),
        "{err}"
    );
    assert_eq!(s.proj.todos().len(), before);
}

#[test]
fn move_a_step_under_a_ticket_is_refused() {
    let s = linked("\"parent\"", None, false);
    let out = s.devkit(&["todo", "add", "a step"], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    let step = stdout(&out).trim().to_string();
    let out = s.devkit(&["todo", "move", &step, "--parent", &id_of(&s, 65)], &AGENT);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains("ticket create") && err.contains("todo promote"),
        "{err}"
    );
    let step = s.proj.todos().into_iter().find(|t| t.id.starts_with(&step));
    assert_eq!(step.unwrap().parent, None);

    let out = s.devkit(&["todo", "add", "another step"], &AGENT);
    let other = stdout(&out).trim().to_string();
    let out = s.devkit(
        &["todo", "move", &id_of(&s, 67), "--parent", &other],
        &AGENT,
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains("devkit ticket create") && err.contains("devkit todo promote"),
        "{err}"
    );
    assert_eq!(parent_of(&s, 67), None);
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
}

#[test]
fn a_step_blocked_by_a_ticket_stays_local() {
    let s = linked("\"parent\", \"blocking\"", None, false);
    let out = s.devkit(
        &["todo", "add", "a step", "--blocked-by", &id_of(&s, 65)],
        &AGENT,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let step = stdout(&out).trim().to_string();
    let out = s.devkit(&["todo", "block", &step, "--by", &id_of(&s, 66)], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    let out = s.devkit(&["todo", "block", &id_of(&s, 67), "--by", &step], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    let step = s
        .proj
        .todos()
        .into_iter()
        .find(|t| t.id.starts_with(&step))
        .unwrap();
    let mut blockers = step.blocked_by.clone();
    blockers.sort();
    let mut want = vec![id_of(&s, 65), id_of(&s, 66)];
    want.sort();
    assert_eq!(blockers, want);
    assert_eq!(s.todo_for("o/r#67").blocked_by, [step.id]);
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
}

#[test]
fn linear_block_with_a_related_pair_conflicts() {
    let linear = linearfake::LinearFake::start();
    let proj = Proj::new();
    std::fs::write(
        proj.path.join("devkit.toml"),
        format!(
            "[tracker]\nkind = \"linear\"\n\n[ticket.sync]\npush = [\"blocking\"]\n{RULES}\n\
             [ticket.sync.statuses]\n\"Todo\" = \"pending\"\n"
        ),
    )
    .unwrap();
    let node = |id: &str| {
        serde_json::json!({
            "identifier": id, "title": format!("Fix {id}"), "state": { "name": "Todo" },
            "parent": null, "inverseRelations": { "nodes": [] }
        })
    };
    let page = serde_json::json!({ "data": { "issues": {
        "pageInfo": { "hasNextPage": false, "endCursor": null },
        "nodes": [node("ENG-1"), node("ENG-2")]
    }}});
    linear.answer("issues(first: 10", &page.to_string());
    linear.answer(
        "teams(first: 10",
        r#"{"data":{"teams":{"pageInfo":{"hasNextPage":false},"nodes":[
            {"key":"ENG","states":{"nodes":[{"name":"Todo"}]}}]}}}"#,
    );
    linear.answer(
        "relations(first",
        r#"{"data":{
            "b":{"id":"uuid-2","identifier":"ENG-2",
                 "relations":{"nodes":[{"id":"rel-1","type":"related","relatedIssue":{"identifier":"ENG-1"}}]},
                 "inverseRelations":{"nodes":[]}},
            "r":{"id":"uuid-1","identifier":"ENG-1"}}}"#,
    );
    linear.answer(
        "issueRelationCreate",
        r#"{"data":{"issueRelationCreate":{"success":true}}}"#,
    );
    let mut env = linear.env().to_vec();
    env.push(("DEVKIT_CALLER", "agent"));
    let out = proj.devkit(&["todo", "sync"], &env);
    assert!(out.status.success(), "{}", stderr(&out));
    let id = |key: &str| {
        proj.todos()
            .into_iter()
            .find(|t| t.ticket.as_ref().is_some_and(|r| r.key == key))
            .unwrap_or_else(|| panic!("{key} synced"))
            .id
    };
    let (eng1, eng2) = (id("ENG-1"), id("ENG-2"));

    let out = proj.devkit(&["todo", "block", &eng2, "--by", &eng1], &env);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains("ENG-2")
            && err.contains("tracker \"related\"")
            && err.contains("--overwrite-tracker"),
        "{err}"
    );
    assert!(linear.sent("issueRelationCreate").is_empty());
    assert!(proj.todo(&eng2).blocked_by.is_empty());

    let out = proj.devkit(
        &["todo", "block", &eng2, "--by", &eng1, "--overwrite-tracker"],
        &env,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let created = linear.sent("issueRelationCreate");
    assert_eq!(created.len(), 1, "{created:?}");
    assert_eq!(
        created[0]["variables"]["input"],
        serde_json::json!({ "issueId": "uuid-1", "relatedIssueId": "uuid-2", "type": "blocks" })
    );
    assert_eq!(proj.todo(&eng2).blocked_by, [eng1]);
}

/// A checkout of another repository, sharing the todo store and its
/// `devkit.toml`.
fn other_checkout(s: &Synced) -> std::path::PathBuf {
    let other = s.proj.outside().join("other");
    std::fs::create_dir(&other).unwrap();
    devkit_git::Git::fixture(&other)
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    std::fs::copy(s.proj.path.join("devkit.toml"), other.join("devkit.toml")).unwrap();
    other
}

#[test]
fn a_link_edit_the_store_already_holds_is_not_refused() {
    let s = linked("\"status\"", Some(65), true);
    let other = other_checkout(&s);
    let (i65, i66, i67) = (id_of(&s, 65), id_of(&s, 66), id_of(&s, 67));
    let edits = [
        vec!["todo", "move", &i67, "--parent", &i65, "--order", "2"],
        vec!["todo", "block", &i65, "--by", &i66],
        vec!["todo", "unblock", &i66, "--by", &i65],
    ];
    let calls = s.calls();
    for args in &edits {
        let out = s.devkit(args, &AGENT);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        let out = s.devkit_in(&other, args, &AGENT, "");
        assert!(out.status.success(), "{args:?} outside: {}", stderr(&out));
    }
    assert_eq!(s.calls(), calls, "the tracker was not asked");
    assert_eq!(parent_of(&s, 67), Some(i65.clone()));
    assert_eq!(s.todo_for("o/r#67").order, Some(2));
    assert_eq!(s.todo_for("o/r#65").blocked_by, [i66]);
    assert!(s.todo_for("o/r#66").blocked_by.is_empty());
}

#[test]
fn a_step_already_under_a_ticket_reorders_without_a_refusal() {
    let s = linked("\"status\"", None, false);
    let out = s.devkit(
        &["todo", "add", "a step", "--parent", &id_of(&s, 65)],
        &AGENT,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let step = stdout(&out).trim().to_string();
    let toml = s.proj.path.join("devkit.toml");
    let config = std::fs::read_to_string(&toml).unwrap();
    std::fs::write(
        &toml,
        config.replace("push = [\"status\"]", "push = [\"parent\"]"),
    )
    .unwrap();
    let out = s.devkit(
        &[
            "todo",
            "move",
            &step,
            "--parent",
            &id_of(&s, 65),
            "--order",
            "2",
        ],
        &AGENT,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let step = s
        .proj
        .todos()
        .into_iter()
        .find(|t| t.id.starts_with(&step))
        .unwrap();
    assert_eq!((step.parent, step.order), (Some(id_of(&s, 65)), Some(2)));
}

#[test]
fn a_step_under_a_ticket_of_another_repository_stays_local() {
    let s = linked("\"parent\"", None, false);
    let other = other_checkout(&s);
    let out = s.devkit_in(
        &other,
        &["todo", "add", "a step", "--parent", &id_of(&s, 65)],
        &AGENT,
        "",
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let step = stdout(&out).trim().to_string();
    let out = s.devkit_in(&other, &["todo", "add", "another step"], &AGENT, "");
    assert!(out.status.success(), "{}", stderr(&out));
    let moved = stdout(&out).trim().to_string();
    let out = s.devkit_in(
        &other,
        &["todo", "move", &moved, "--parent", &id_of(&s, 66)],
        &AGENT,
        "",
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let parent = |short: &str| {
        s.proj
            .todos()
            .into_iter()
            .find(|t| t.id.starts_with(short))
            .unwrap()
            .parent
    };
    assert_eq!(parent(&step), Some(id_of(&s, 65)));
    assert_eq!(parent(&moved), Some(id_of(&s, 66)));
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
}

#[test]
fn an_unreadable_issue_is_reported_as_a_read() {
    let s = linked("\"parent\"", None, false);
    s.serve(
        "graphql.json",
        r#"{"data":{"repository":{"i67":null,"i65":{"id":"I_65"}}},
            "errors":[{"type":"NOT_FOUND","message":"Could not resolve to an Issue with the number of 67."}]}"#,
    );
    let out = s.devkit(
        &["todo", "move", &id_of(&s, 67), "--parent", &id_of(&s, 65)],
        &AGENT,
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains("o/r#67 was not found") && !err.contains("refused the write"),
        "{err}"
    );
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
    assert_eq!(parent_of(&s, 67), None);
}
