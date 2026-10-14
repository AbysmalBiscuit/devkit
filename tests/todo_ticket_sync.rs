//! `devkit todo sync` bringing a GitHub repository's tickets into the todo
//! store, against a `gh` that answers from canned GitHub responses.

#[path = "common/ghfake.rs"]
mod ghfake;
#[path = "common/todoenv.rs"]
mod todoenv;

use std::{
    path::PathBuf,
    time::{Duration, Instant, SystemTime},
};

use devkit_todo::{Filter, Holder, Status, Todo, TodoStore};
use devkit_todo_taskchampion::{SyncTarget, TaskchampionStore};
use serde_json::{Value, json};
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
        Self::in_proj(Proj::new(), sync, project)
    }

    fn in_proj(proj: Proj, sync: &str, project: bool) -> Self {
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
        self.devkit_fed(args, env, "")
    }

    fn devkit_fed(&self, args: &[&str], env: &[(&str, &str)], stdin: &str) -> std::process::Output {
        self.devkit_in(&self.proj.path, args, env, stdin)
    }

    fn devkit_in(
        &self,
        dir: &std::path::Path,
        args: &[&str],
        env: &[(&str, &str)],
        stdin: &str,
    ) -> std::process::Output {
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
        self.proj.devkit_in(dir, args, &all, stdin)
    }

    fn sync(&self, args: &[&str]) -> std::process::Output {
        let mut all = vec!["todo", "sync"];
        all.extend_from_slice(args);
        self.devkit(&all, &[])
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.bin.path().join("gh.log")).unwrap_or_default()
    }

    /// The tracker writes `gh` was asked for.
    fn mutations(&self) -> Vec<String> {
        self.calls()
            .lines()
            .filter(|l| l.contains("query=mutation"))
            .map(String::from)
            .collect()
    }

    /// How many rule searches `gh` was asked for: one per ticket sync here.
    fn ticket_queries(&self) -> usize {
        self.calls()
            .lines()
            .filter(|l| l.contains("search(query"))
            .count()
    }

    /// The ticket sync's state file for this checkout's repository with
    /// extension `ext`: `last`, `failed` or `lock`.
    fn sync_state(&self, ext: &str) -> PathBuf {
        self.proj
            .state()
            .join("ticket-sync")
            .join(format!("proj.{ext}"))
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
            "repository":{{{},"issue":{{"id":"I_65","projectItems":{{"nodes":[
                {{"id":"PVTI_3","project":{{"id":"PVT_3","number":3}},
                  "fieldValueByName":{{"name":"Todo","optionId":"o0"}}}}]}}}}}},
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
    two_issues_in(Proj::new())
}

/// [`two_issues`] in `proj`.
fn two_issues_in(proj: Proj) -> Synced {
    let s = Synced::in_proj(proj, &format!("{RULES}{STATUSES}"), true);
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
    let (t65, t66) = (s.todo_for("o/r#65"), s.todo_for("o/r#66"));
    assert_eq!(t65.description, "#65 Issue 65");
    assert_eq!(t65.node(), "proj");
    assert_eq!(t66.node(), "proj");
    assert_eq!(t65.blocked_by, std::slice::from_ref(&t66.id));
    assert_eq!(t65.status, Status::Pending);
    let synced = t65.ticket.and_then(|t| t.synced_status);
    assert_eq!(synced.as_deref(), Some("Todo"));
}

#[test]
fn a_foreground_sync_pushes_its_tickets_to_the_sync_target() {
    let target = tempfile::tempdir().unwrap();
    let server = target.path().join("server");
    let config = format!(
        "[todo]\nbackend = \"taskchampion\"\n[todo.taskchampion]\nserver_dir = {}\n",
        toml::Value::String(server.display().to_string())
    );
    let s = two_issues_in(Proj::with_home_config(&config));
    let out = s.sync(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("2 added"), "{}", stdout(&out));
    let other =
        TaskchampionStore::at(target.path().join("other")).with_target(SyncTarget::Dir(server));
    other.sync_once().unwrap();
    let mut on_target: Vec<String> = other
        .list(&Filter::all())
        .unwrap()
        .into_iter()
        .map(|t| t.description)
        .collect();
    on_target.sort();
    assert_eq!(on_target, ["#65 Issue 65", "#66 Issue 66"]);
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
    assert!(shown.contains("block o/r#65 by o/r#66"), "{shown}");
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
    let id = s.todo_for("o/r#66").id;
    let agent = [("DEVKIT_CALLER", "agent"), ("CLAUDE_CODE_SESSION_ID", "S")];
    let start = s.devkit(&["todo", "start", &id], &agent);
    assert!(start.status.success(), "{}", stderr(&start));
    assert!(s.sync(&[]).status.success());
    assert_eq!(s.todo_for("o/r#66").status, Status::InProgress {
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
    assert_eq!(s.todo_for("o/r#65").status, Status::Completed {
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
    let (t65, t66) = (s.todo_for("o/r#65"), s.todo_for("o/r#66"));
    assert_eq!(t65.blocked_by, std::slice::from_ref(&t66.id));
    assert_eq!(t66.parent.as_deref(), Some(t65.id.as_str()));
    assert!(s.calls().contains("assignee=LevValle"), "{}", s.calls());
}

#[test]
fn a_slug_in_another_case_names_the_same_tickets() {
    let s = two_issues();
    assert!(s.sync(&[]).status.success());
    let config = s.proj.path.join("devkit.toml");
    let upper = std::fs::read_to_string(&config)
        .unwrap()
        .replace("\"o/r\"", "\"O/R\"");
    std::fs::write(&config, upper).unwrap();
    let out = s.sync(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("0 added, 0 changed"),
        "{}",
        stdout(&out)
    );
    assert_eq!(s.proj.todos().len(), 2, "{:?}", s.proj.todos());
}

/// Polls `done` until it holds, failing with `what` after 30 seconds.
fn poll(what: &str, done: impl Fn() -> bool) {
    poll_or(what, done, String::new);
}

/// [`poll`], adding `detail` to the failure.
fn poll_or(what: &str, done: impl Fn() -> bool, detail: impl Fn() -> String) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}{}",
            detail()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Waits long enough for a background sync a call may have started to reach
/// `gh`, for a test that asserts none did.
fn settle() {
    std::thread::sleep(Duration::from_secs(1));
}

#[test]
fn a_stale_store_starts_one_background_sync() {
    let s = two_issues();
    for _ in 0..2 {
        let out = s.devkit(&["todo", "list"], &[]);
        assert!(out.status.success(), "{}", stderr(&out));
    }
    poll("the background sync", || s.sync_state("last").exists());
    assert_eq!(s.ticket_queries(), 1, "{}", s.calls());
    assert_eq!(s.proj.todos().len(), 2, "{:?}", s.proj.todos());
}

#[test]
fn a_fresh_store_starts_none() {
    let s = two_issues();
    assert!(s.sync(&[]).status.success());
    assert!(s.sync_state("last").exists());
    let before = s.ticket_queries();
    let out = s.devkit(&["todo", "list"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    settle();
    assert_eq!(s.ticket_queries(), before, "{}", s.calls());
}

#[test]
fn a_failed_sync_holds_off_the_next() {
    let s = Synced::new(&format!("{RULES}{STATUSES}"), true);
    let out = s.devkit(&["todo", "list"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    poll("the failed sync", || s.sync_state("failed").exists());
    assert_eq!(s.ticket_queries(), 1, "{}", s.calls());
    let out = s.devkit(&["todo", "ready"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    settle();
    assert_eq!(s.ticket_queries(), 1, "{}", s.calls());
    assert!(!s.sync_state("last").exists());
}

#[test]
fn a_hook_injection_answers_while_the_tracker_hangs() {
    let s = two_issues();
    assert!(s.sync(&[]).status.success());
    let day_old = SystemTime::now() - Duration::from_secs(24 * 3600);
    std::fs::File::options()
        .write(true)
        .open(s.sync_state("last"))
        .unwrap()
        .set_modified(day_old)
        .unwrap();
    let hang = s.bin.path().join("hang");
    std::fs::write(&hang, "").unwrap();
    let before = s.calls().lines().count();
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "s1",
        "cwd": s.proj.path,
        "source": "startup",
    });
    let started = Instant::now();
    let out = s.devkit_fed(
        &["todo", "context", "--harness", "claude-code"],
        &[],
        &payload.to_string(),
    );
    let took = started.elapsed();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(took < Duration::from_secs(3), "the injection took {took:?}");
    assert!(stdout(&out).contains("#65 Issue 65"), "{}", stdout(&out));
    poll("the background sync to reach the tracker", || {
        s.calls().lines().count() > before
    });
    std::fs::remove_file(&hang).unwrap();
    poll_or(
        "the background sync to finish",
        || {
            std::fs::metadata(s.sync_state("last"))
                .and_then(|m| m.modified())
                .is_ok_and(|at| at > day_old + Duration::from_secs(3600))
        },
        || {
            std::fs::read_to_string(s.sync_state("failed"))
                .map(|reason| format!("; the sync failed: {reason}"))
                .unwrap_or_default()
        },
    );
}

/// A taskchampion replica whose sync server refuses every connection.
const UNREACHABLE_SERVER: [(&str, &str); 4] = [
    ("DEVKIT_TODO_BACKEND", "taskchampion"),
    ("DEVKIT_TODO_SYNC_URL", "http://127.0.0.1:1"),
    (
        "DEVKIT_TODO_SYNC_CLIENT_ID",
        "6a1b3c5e-0000-4000-8000-000000000001",
    ),
    ("DEVKIT_TODO_SYNC_SECRET", "secret"),
];

/// Asserts no ticket sync started: the tracker saw no query and the sync
/// left no state behind.
fn assert_no_ticket_sync(s: &Synced) {
    settle();
    assert_eq!(s.ticket_queries(), 0, "{}", s.calls());
    assert!(!s.sync_state("last").exists());
    assert!(!s.sync_state("failed").exists());
}

#[test]
fn list_sync_skips_tickets_when_the_replica_sync_fails() {
    let s = two_issues();
    let out = s.devkit(&["todo", "list", "--sync"], &UNREACHABLE_SERVER);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_no_ticket_sync(&s);
}

#[test]
fn session_start_skips_tickets_when_the_replica_sync_fails() {
    let s = two_issues();
    let payload = json!({
        "hook_event_name": "SessionStart",
        "session_id": "s1",
        "cwd": s.proj.path,
        "source": "startup",
    });
    let out = s.devkit_fed(
        &["todo", "context", "--harness", "claude-code"],
        &UNREACHABLE_SERVER,
        &payload.to_string(),
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_no_ticket_sync(&s);
}

#[test]
fn a_completed_replica_sync_refreshes_tickets() {
    let server = tempfile::tempdir().unwrap();
    let config = format!(
        "[todo]\nbackend = \"taskchampion\"\n[todo.taskchampion]\nserver_dir = {}\n",
        toml::Value::String(server.path().display().to_string())
    );
    let s = two_issues_in(Proj::with_home_config(&config));
    let out = s.devkit(&["todo", "list", "--sync"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    poll("the background sync", || s.sync_state("last").exists());
    assert_eq!(s.ticket_queries(), 1, "{}", s.calls());
}

/// [`two_issues`] on a taskchampion replica at `replica` that syncs to a
/// directory server the caller keeps alive as `server`.
fn two_issues_on_a_synced_replica(server: &std::path::Path, replica: &std::path::Path) -> Synced {
    let config = format!(
        "[todo]\nbackend = \"taskchampion\"\n[todo.taskchampion]\ndata_dir = {}\nserver_dir = {}\n",
        toml::Value::String(replica.display().to_string()),
        toml::Value::String(server.display().to_string())
    );
    two_issues_in(Proj::with_home_config(&config))
}

#[test]
fn reads_on_a_replica_that_never_pulled_sync_no_tickets() {
    let dir = tempfile::tempdir().unwrap();
    let s = two_issues_on_a_synced_replica(&dir.path().join("server"), &dir.path().join("replica"));
    for read in [&["todo", "list"][..], &["todo", "ready"]] {
        let out = s.devkit(read, &[]);
        assert!(out.status.success(), "{}", stderr(&out));
    }
    let prompt = json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "s1",
        "cwd": s.proj.path,
        "prompt": "go",
    });
    let out = s.devkit_fed(
        &[
            "todo",
            "context",
            "--harness",
            "claude-code",
            "--guide",
            "none",
            "--if-changed",
        ],
        &[],
        &prompt.to_string(),
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_no_ticket_sync(&s);
}

#[test]
fn a_read_after_the_replica_pulls_syncs_tickets() {
    let dir = tempfile::tempdir().unwrap();
    let replica = dir.path().join("replica");
    let s = two_issues_on_a_synced_replica(&dir.path().join("server"), &replica);
    let out = s.devkit(&["todo", "add", "plain"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    poll("the replica's sync", || {
        replica.join("sync.pulled").exists()
    });
    let out = s.devkit(&["todo", "list"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    poll("the background sync", || s.sync_state("last").exists());
    assert_eq!(s.ticket_queries(), 1, "{}", s.calls());
}

const TODO_EVENTS: &str = "
[ticket.events.todo_start]
to = \"In progress\"

[ticket.events.todo_done]
to = \"Done\"
";

/// [`two_issues`] synced, with `todo_start` and `todo_done` moving tickets
/// on project 3, whose writes `gh` accepts.
fn moving() -> Synced {
    moving_in(Proj::new())
}

/// [`moving`] in `proj`.
fn moving_in(proj: Proj) -> Synced {
    let s = Synced::in_proj(proj, &format!("{RULES}{STATUSES}{TODO_EVENTS}"), true);
    s.serve(
        "graphql.json",
        &graphql(
            &[issue(65, Some("Todo"), &[66]), issue(66, None, &[])],
            &OPTIONS,
        ),
    );
    s.serve("graphql_mutation.json", r#"{"data":{}}"#);
    let out = s.sync(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    s
}

/// Whether `gh` was asked to set project 3's status field to option `option`.
fn moved_to(s: &Synced, option: &str) -> bool {
    s.mutations().iter().any(|m| {
        m.contains("updateProjectV2ItemFieldValue") && m.contains(&format!("\"{option}\""))
    })
}

#[test]
fn done_on_a_ticket_todo_moves_the_ticket() {
    let s = moving();
    let id = s.todo_for("o/r#65").id;
    let out = s.devkit(&["todo", "done", &id], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    poll("the move to Done", || moved_to(&s, "o2"));
}

#[test]
fn start_on_a_ticket_todo_moves_the_ticket() {
    let s = moving();
    let id = s.todo_for("o/r#65").id;
    let agent = [("DEVKIT_CALLER", "agent"), ("CLAUDE_CODE_SESSION_ID", "S")];
    let out = s.devkit(&["todo", "start", "--force", &id], &agent);
    assert!(out.status.success(), "{}", stderr(&out));
    poll("the move to In progress", || moved_to(&s, "o1"));
}

#[test]
fn a_sync_completing_a_ticket_fires_nothing() {
    let s = moving();
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
    assert_eq!(s.todo_for("o/r#65").status, Status::Completed {
        by: Some(Holder::tracker())
    });
    settle();
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
}

#[test]
fn a_ticket_event_whose_status_the_todo_left_moves_nothing() {
    let s = moving();
    let id = s.todo_for("o/r#65").id;
    let out = s.devkit(&["todo", "done", &id], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    poll("the move to Done", || moved_to(&s, "o2"));
    let out = s.devkit(
        &["ticket", "event", "todo_start", "o/r#65", "--todo", &id],
        &[],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("skipped"), "{}", stderr(&out));
    assert!(!moved_to(&s, "o1"), "{:?}", s.mutations());
}

#[test]
fn done_from_outside_the_repository_warns() {
    let s = moving();
    let id = s.todo_for("o/r#65").id;
    let out = s.devkit_in(s.proj.outside(), &["todo", "done", &id], &[], "");
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("ticket o/r#65 not moved: run from its repository"),
        "{}",
        stderr(&out)
    );
    assert!(s.proj.todo(&id).status.kind() == devkit_todo::StatusKind::Completed);
    settle();
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
}

#[test]
fn a_plain_todo_fires_nothing() {
    let s = moving();
    let out = s.devkit(&["todo", "add", "plain"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let id = stdout(&out).trim().to_string();
    let out = s.devkit(&["todo", "done", &id], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    settle();
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
}

/// Claude Code's `tool` run in `s`'s checkout with `input` and answered with
/// `response`, as the post-tool-use hook receives it.
fn native(s: &Synced, tool: &str, input: Value, response: Value) {
    let payload = json!({
        "session_id": "S",
        "cwd": s.proj.path,
        "hook_event_name": "PostToolUse",
        "tool_name": tool,
        "tool_input": input,
        "tool_response": response,
    });
    let out = s.devkit_fed(
        &["hook", "post-tool-use", "--harness", "claude-code"],
        &[],
        &payload.to_string(),
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));
}

fn task_create(s: &Synced, id: &str, subject: &str) {
    native(
        s,
        "TaskCreate",
        json!({"subject": subject, "description": subject}),
        json!({"task": {"id": id, "subject": subject}}),
    );
}

fn task_update(s: &Synced, id: &str, status: &str) {
    native(
        s,
        "TaskUpdate",
        json!({"taskId": id, "status": status}),
        json!({"success": true}),
    );
}

#[test]
fn a_harness_task_naming_a_ticket_moves_it() {
    let s = moving();
    task_create(&s, "1", "#65");
    assert_eq!(s.proj.todos().len(), 2, "{:?}", s.proj.todos());
    task_update(&s, "1", "in_progress");
    poll("the move to In progress", || moved_to(&s, "o1"));
    task_update(&s, "1", "completed");
    poll("the move to Done", || moved_to(&s, "o2"));
    assert_eq!(
        s.todo_for("o/r#65").status.kind(),
        devkit_todo::StatusKind::Completed
    );
}

#[test]
fn a_queued_harness_task_naming_a_ticket_moves_it() {
    let s = moving_in(Proj::with_home_config(
        "[todo]\nbackend = \"taskchampion\"\n",
    ));
    task_create(&s, "1", "o/r#65");
    task_update(&s, "1", "completed");
    poll("the move to Done", || moved_to(&s, "o2"));
}

/// Puts a `git` ahead of the real one on `s`'s `PATH` that logs each spawn's
/// arguments, one line per spawn, to the returned file.
#[cfg(unix)]
fn trace_git(s: &Synced) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let real = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("git on PATH");
    let trace = s.bin.path().join("git-spawns");
    let wrapper = s.bin.path().join("git");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            trace.display(),
            real.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    trace
}

#[cfg(unix)]
#[test]
fn a_ticket_task_update_asks_git_no_more_than_a_plain_one() {
    let s = two_issues();
    assert!(s.sync(&[]).status.success());
    task_create(&s, "1", "#65");
    task_create(&s, "2", "write the fix");
    let trace = trace_git(&s);
    let spawns = |task: &str| {
        let _ = std::fs::remove_file(&trace);
        task_update(&s, task, "in_progress");
        std::fs::read_to_string(&trace).unwrap_or_default()
    };
    let plain = spawns("2");
    let ticket = spawns("1");
    assert_eq!(ticket.lines().count(), plain.lines().count(), "{ticket}");
}

#[test]
fn a_harness_task_naming_no_ticket_adds_a_plain_todo() {
    let s = moving();
    task_create(&s, "1", "write the fix");
    let todos = s.proj.todos();
    let plain = todos
        .iter()
        .find(|t| t.description == "write the fix")
        .unwrap_or_else(|| panic!("{todos:?}"));
    assert!(plain.ticket.is_none());
    task_update(&s, "1", "completed");
    assert_eq!(s.todo_for("o/r#65").status, Status::Pending);
    settle();
    assert!(s.mutations().is_empty(), "{:?}", s.mutations());
}

#[test]
fn a_plan_step_naming_a_ticket_moves_it_and_dropping_it_keeps_it() {
    let s = moving();
    let write = |todos: Value| {
        native(
            &s,
            "TodoWrite",
            json!({"todos": todos}),
            json!({"oldTodos": [], "newTodos": todos}),
        )
    };
    write(json!([{"content": "#65 Issue 65", "status": "in_progress", "activeForm": "Fixing"}]));
    assert_eq!(s.proj.todos().len(), 2, "{:?}", s.proj.todos());
    poll("the move to In progress", || moved_to(&s, "o1"));
    write(json!([]));
    let ticket = s.todo_for("o/r#65");
    assert_eq!(ticket.status.kind(), devkit_todo::StatusKind::InProgress);
    assert_eq!(ticket.node(), "proj");
}

#[test]
fn a_plan_resent_after_a_sync_closes_its_ticket_moves_nothing() {
    let s = moving();
    let plan =
        json!([{"content": "#65 Issue 65", "status": "in_progress", "activeForm": "Fixing"}]);
    let write = || {
        native(
            &s,
            "TodoWrite",
            json!({"todos": plan}),
            json!({"oldTodos": [], "newTodos": plan}),
        )
    };
    write();
    poll("the move to In progress", || moved_to(&s, "o1"));
    let closed = issue(65, Some("In progress"), &[66]).replace(
        r#""state":"OPEN","stateReason":null"#,
        r#""state":"CLOSED","stateReason":"COMPLETED""#,
    );
    s.serve(
        "graphql.json",
        &graphql(&[closed, issue(66, None, &[])], &OPTIONS)
            .replace(r#""nodes":[{"number":65"#, r#""nodes":[{"number":66"#),
    );
    assert!(s.sync(&[]).status.success());
    let before = s.mutations();
    write();
    assert_eq!(
        s.todo_for("o/r#65").status.kind(),
        devkit_todo::StatusKind::Completed
    );
    settle();
    assert_eq!(s.mutations(), before);
}

/// Makes `s`'s checkout a `workspace setup` worktree for issue `issue`.
fn record_issue(s: &Synced, issue: &str) {
    record_issue_from(s, issue, devkit_common::record::RecordOrigin::Setup);
}

/// Records `s`'s checkout as made by `origin` for issue `issue`.
fn record_issue_from(s: &Synced, issue: &str, origin: devkit_common::record::RecordOrigin) {
    devkit_common::record::write(&s.proj.path, &devkit_common::record::IssueRecord {
        issue: issue.into(),
        slug: "fix".into(),
        origin: Some(origin),
        ..Default::default()
    })
    .unwrap();
}

const AGENT: [(&str, &str); 2] = [("DEVKIT_CALLER", "agent"), ("CLAUDE_CODE_SESSION_ID", "S")];

#[test]
fn ready_in_a_ticket_workspace_offers_the_ticket() {
    let s = two_issues();
    assert!(s.sync(&[]).status.success());
    record_issue(&s, "66");
    let out = s.devkit(&["todo", "ready"], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    let ready = stdout(&out);
    assert!(ready.contains("#66 Issue 66"), "{ready}");
    assert!(!ready.contains("#65"), "{ready}");
}

#[test]
fn a_review_checkout_offers_no_ticket() {
    let s = two_issues();
    assert!(s.sync(&[]).status.success());
    record_issue_from(&s, "66", devkit_common::record::RecordOrigin::Checkout);
    let out = s.devkit(&["todo", "ready"], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!stdout(&out).contains("#66"), "{}", stdout(&out));
}

#[test]
fn ready_without_a_record_is_unchanged() {
    let s = two_issues();
    assert!(s.sync(&[]).status.success());
    let out = s.devkit(&["todo", "ready"], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!stdout(&out).contains("#66"), "{}", stdout(&out));
}

#[test]
fn expand_lists_the_workspace_todos_under_the_ticket() {
    let s = two_issues();
    assert!(s.sync(&[]).status.success());
    record_issue(&s, "66");
    for text in ["write the fix", "already done"] {
        let out = s.devkit(&["todo", "add", text], &AGENT);
        assert!(out.status.success(), "{}", stderr(&out));
    }
    let done = s
        .proj
        .todos()
        .into_iter()
        .find(|t| t.description == "already done")
        .unwrap();
    assert!(
        s.devkit(&["todo", "done", &done.id], &AGENT)
            .status
            .success()
    );
    let ticket = s.todo_for("o/r#66");
    let summary = format!(
        "- [ ] #66 Issue 66 ({}, 1 open)",
        devkit_todo::short_id(&ticket.id)
    );

    let out = s.devkit(&["todo", "list"], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    let listed = stdout(&out);
    assert!(listed.contains(&format!("{summary}\n")), "{listed}");

    let out = s.devkit(&["todo", "list", "--expand"], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    let expanded = stdout(&out);
    let lines: Vec<&str> = expanded.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.starts_with(&summary))
        .unwrap_or_else(|| panic!("{expanded}"));
    assert!(
        lines[at + 1].starts_with("  - [ ] write the fix (")
            && lines[at + 1].contains(" on proj.main"),
        "{expanded}"
    );
    assert!(
        lines
            .get(at + 2)
            .is_none_or(|l| !l.contains("already done")),
        "{expanded}"
    );

    let out = s.devkit(&["todo", "plan", "--expand"], &AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    let plan = stdout(&out);
    let lines: Vec<&str> = plan.lines().collect();
    let row = format!("o {}  #66 Issue 66", devkit_todo::short_id(&ticket.id));
    let at = lines
        .iter()
        .position(|l| l.trim_start().starts_with(&row))
        .unwrap_or_else(|| panic!("{plan}"));
    assert!(lines[at].ends_with(", 1 open"), "{plan}");
    assert!(
        lines[at + 1].starts_with("    - [ ] write the fix (")
            && lines[at + 1].contains(" on proj.main"),
        "{plan}"
    );
    assert!(
        lines
            .get(at + 2)
            .is_none_or(|l| !l.contains("already done")),
        "{plan}"
    );
}
