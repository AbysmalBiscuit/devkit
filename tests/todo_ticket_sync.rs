//! `devkit todo sync` bringing a GitHub repository's tickets into the todo
//! store, against a `gh` that answers from canned GitHub responses.

#[path = "common/ghfake.rs"]
mod ghfake;
#[path = "common/todoenv.rs"]
mod todoenv;

use std::path::PathBuf;

use devkit_todo::{Holder, Status, Todo};
use todoenv::{Proj, stderr, stdout};

const STATUSES: &str = r#"
[ticket.sync.statuses]
"" = "pending"
"Todo" = "pending"
"In progress" = "in_progress"
"Done" = "completed"
"#;

const RULES: &str = "\n[[ticket.sync.rules]]\nassignee = \"me\"\n";

/// A checkout `proj` whose issues live in GitHub repository `o/r`, with
/// project 3 holding their status when `project`, and a fake `gh`.
struct Synced {
    proj: Proj,
    bin: tempfile::TempDir,
}

impl Synced {
    fn new(sync: &str, project: bool) -> Self {
        let proj = Proj::new();
        let project = if project { "project = 3\n" } else { "" };
        std::fs::write(
            proj.path.join("devkit.toml"),
            format!(
                "[tracker]\nkind = \"github\"\n\n[forge]\nkind = \"github\"\nrepo = \"o/r\"\n\n\
                 [github]\nissues_repo = \"o/r\"\n{project}{sync}"
            ),
        )
        .unwrap();
        let bin = tempfile::tempdir().unwrap();
        ghfake::install_fake_gh(bin.path());
        Self { proj, bin }
    }

    fn serve(&self, file: &str, body: &str) {
        std::fs::write(self.bin.path().join(file), body).unwrap();
    }

    fn devkit(&self, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
        let path = std::env::join_paths(std::iter::once(self.bin.path().to_path_buf()).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        let path = path.to_str().unwrap();
        let ghfake: PathBuf = self.bin.path().to_path_buf();
        let mut all = vec![
            ("PATH", path),
            ("GHFAKE_DIR", ghfake.to_str().unwrap()),
            ("GH_TOKEN", ""),
            ("GITHUB_TOKEN", ""),
        ];
        all.extend_from_slice(env);
        self.proj.devkit(args, &all)
    }

    fn sync(&self, args: &[&str]) -> std::process::Output {
        let mut all = vec!["todo", "sync"];
        all.extend_from_slice(args);
        self.devkit(&all, &[])
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.bin.path().join("gh.log")).unwrap_or_default()
    }

    fn todo_for(&self, key: &str) -> Todo {
        self.proj
            .todos()
            .into_iter()
            .find(|t| t.ticket.as_ref().is_some_and(|r| r.key == key))
            .unwrap_or_else(|| panic!("no todo for ticket {key}: {:?}", self.proj.todos()))
    }
}

/// An open issue as the sync query reads it, in project 3 at `option` when
/// one is given.
fn issue(number: u64, option: Option<&str>, blocked_by: &[u64]) -> String {
    let items = match option {
        Some(name) => {
            format!(r#"[{{"project":{{"id":"PVT_3"}},"fieldValueByName":{{"name":"{name}"}}}}]"#)
        }
        None => "[]".into(),
    };
    let blockers: Vec<String> = blocked_by
        .iter()
        .map(|n| format!(r#"{{"number":{n},"repository":{{"nameWithOwner":"o/r"}}}}"#))
        .collect();
    format!(
        r#"{{"number":{number},"title":"Issue {number}","state":"OPEN","stateReason":null,
            "parent":null,"blockedBy":{{"nodes":[{}]}},"projectItems":{{"nodes":{items}}}}}"#,
        blockers.join(",")
    )
}

/// One answer for every query a sync sends: the search, the issues by
/// number, and project 3's status field with `options`.
fn graphql(issues: &[String], options: &[&str]) -> String {
    let options: Vec<String> = options
        .iter()
        .enumerate()
        .map(|(i, n)| format!(r#"{{"id":"o{i}","name":"{n}"}}"#))
        .collect();
    let by_number: Vec<String> = issues
        .iter()
        .map(|i| {
            let v: serde_json::Value = serde_json::from_str(i).unwrap();
            format!(r#""i{}":{i}"#, v["number"])
        })
        .collect();
    format!(
        r#"{{"data":{{
            "search":{{"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[{}]}},
            "repository":{{{}}},
            "repositoryOwner":{{"projectV2":{{"id":"PVT_3",
                "field":{{"id":"PVTSSF_status","options":[{}]}}}}}}
        }}}}"#,
        issues.join(","),
        by_number.join(","),
        options.join(",")
    )
}

const OPTIONS: [&str; 3] = ["Todo", "In progress", "Done"];

/// Issue 65, at `Todo` and blocked by 66, and issue 66, outside the project.
fn two_issues() -> Synced {
    let s = Synced::new(&format!("{RULES}{STATUSES}"), true);
    s.serve(
        "graphql.json",
        &graphql(
            &[issue(65, Some("Todo"), &[66]), issue(66, None, &[])],
            &OPTIONS,
        ),
    );
    s
}

#[test]
fn sync_adds_one_todo_per_ticket_with_edges() {
    let s = two_issues();
    let out = s.sync(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("2 added"), "{}", stdout(&out));
    let todos = s.proj.todos();
    assert_eq!(todos.len(), 2, "{todos:?}");
    let (t65, t66) = (s.todo_for("65"), s.todo_for("66"));
    assert_eq!(t65.description, "#65 Issue 65");
    assert_eq!(t65.node(), "proj");
    assert_eq!(t66.node(), "proj");
    assert_eq!(t65.blocked_by, std::slice::from_ref(&t66.id));
    assert_eq!(t65.status, Status::Pending);
    let synced = t65.ticket.and_then(|t| t.synced_status);
    assert_eq!(synced.as_deref(), Some("Todo"));
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
    assert!(shown.contains("block 65 by 66"), "{shown}");
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
    let id = s.todo_for("66").id;
    let agent = [("DEVKIT_CALLER", "agent"), ("CLAUDE_CODE_SESSION_ID", "S")];
    let start = s.devkit(&["todo", "start", &id], &agent);
    assert!(start.status.success(), "{}", stderr(&start));
    assert!(s.sync(&[]).status.success());
    assert_eq!(s.todo_for("66").status, Status::InProgress {
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
    assert_eq!(s.todo_for("65").status, Status::Completed {
        by: Some(Holder::tracker())
    });
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
    let (t65, t66) = (s.todo_for("65"), s.todo_for("66"));
    assert_eq!(t65.blocked_by, std::slice::from_ref(&t66.id));
    assert_eq!(t66.parent.as_deref(), Some(t65.id.as_str()));
    assert!(s.calls().contains("assignee=LevValle"), "{}", s.calls());
}
