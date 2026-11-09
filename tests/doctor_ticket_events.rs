//! `devkit doctor` checks `[ticket.events]` against the tracker: the project,
//! its status field, every status name, and the token's scope.

#[path = "common/ghfake.rs"]
mod ghfake;

const EVENTS: &str = "[tracker]\nkind = \"github\"\n\
                      [ticket.events.start]\nfrom = [\"\", \"Todo\"]\nto = \"In progress\"\n\
                      [ticket.events.pr_open]\nto = \"In review\"\n";

const BOARD: &str = r#"{"data":{"repositoryOwner":{"projectV2":{"id":"PVT_3","field":{"id":"F","options":[
  {"id":"a","name":"Todo"},{"id":"b","name":"In progress"},{"id":"c","name":"In review"}]}}}}}"#;

/// The `issue events` row of `devkit doctor --json`, if there is one.
fn row(gh: &ghfake::Fake) -> Option<serde_json::Value> {
    let out = gh.devkit_here(&["doctor", "--json"]);
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "doctor --json: {e}\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    rows.as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "ticket_events")
        .cloned()
}

fn board(events: &str) -> ghfake::Fake {
    let gh = ghfake::Fake::without_pr(events);
    gh.github_keys("project = 3");
    gh
}

fn detail(row: &serde_json::Value) -> &str {
    row["detail"].as_str().unwrap_or_default()
}

#[test]
fn no_events_no_row() {
    let gh = board("[tracker]\nkind = \"github\"\n");
    assert_eq!(row(&gh), None);
    assert!(!gh.calls().contains("graphql"), "{}", gh.calls());
}

#[test]
fn a_configured_project_passes_and_lists_each_transition() {
    let gh = board(EVENTS);
    gh.serve_graphql(BOARD);
    let row = row(&gh).expect("a ticket_events row");
    assert_eq!(row["status"], "ok", "{row}");
    assert!(
        detail(&row).contains("start: (none), Todo -> In progress"),
        "{row}"
    );
    assert!(detail(&row).contains("pr_open: * -> In review"), "{row}");
}

#[test]
fn a_missing_option_fails_naming_it() {
    let gh = board(&EVENTS.replace("In review", "Shipping"));
    gh.serve_graphql(BOARD);
    let row = row(&gh).expect("a ticket_events row");
    assert_eq!(row["status"], "invalid", "{row}");
    assert!(detail(&row).contains("no option `Shipping`"), "{row}");
}

#[test]
fn insufficient_scopes_fails_with_the_remedy() {
    let gh = board(EVENTS);
    gh.serve_graphql(include_str!(
        "../crates/devkit-common/src/tracker/fixtures/github_status_insufficient_scopes.json"
    ));
    let row = row(&gh).expect("a ticket_events row");
    assert_eq!(row["status"], "invalid", "{row}");
    assert!(detail(&row).contains("gh auth refresh -s project"), "{row}");
}

#[test]
fn a_missing_writer_names_the_key() {
    let gh = ghfake::Fake::without_pr(EVENTS);
    let row = row(&gh).expect("a ticket_events row");
    assert_eq!(row["status"], "invalid", "{row}");
    assert!(detail(&row).contains("[github] project"), "{row}");
}

#[test]
fn a_sentinel_target_fails_naming_its_key() {
    let gh = board(&EVENTS.replace("to = \"In review\"", "to = \"*\""));
    gh.serve_graphql(BOARD);
    let row = row(&gh).expect("a ticket_events row");
    assert_eq!(row["status"], "invalid", "{row}");
    assert!(detail(&row).contains("[ticket.events.pr_open] to"), "{row}");
}

#[test]
fn a_missing_from_name_fails_naming_its_key() {
    let gh = board(&EVENTS.replace("\"Todo\"]", "\"Backlog\"]"));
    gh.serve_graphql(BOARD);
    let row = row(&gh).expect("a ticket_events row");
    assert_eq!(row["status"], "invalid", "{row}");
    assert!(
        detail(&row).contains("[ticket.events.start] from")
            && detail(&row).contains("no option `Backlog`"),
        "{row}"
    );
}

/// Readers of the old key find the same row under `issue_events`, from the
/// one check: the tracker is asked once.
#[test]
fn the_json_also_names_the_row_by_its_old_key() {
    let gh = board(EVENTS);
    gh.serve_graphql(BOARD);
    let out = gh.devkit_here(&["doctor", "--json"]);
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let find = |key: &str| {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == key)
            .cloned()
            .unwrap_or_else(|| panic!("no {key} row: {rows}"))
    };
    let (current, old) = (find("ticket_events"), find("issue_events"));
    assert_eq!(old["status"], current["status"]);
    assert_eq!(old["detail"], current["detail"]);
    assert_eq!(gh.calls().matches("graphql").count(), 1, "{}", gh.calls());

    let text = gh.devkit_here(&["doctor"]);
    assert!(
        !String::from_utf8_lossy(&text.stdout).contains("issue_events"),
        "the table names the row once"
    );
}
