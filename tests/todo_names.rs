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
fn a_printed_short_id_resolves_even_when_it_reads_as_a_ticket_number() {
    let s = Synced::new(RULES, false);
    let (short_id, elsewhere, numbered) = prefix_like_a_ticket(&s, 8);
    ok(s.devkit(&["todo", "start", &short_id], &TASKCHAMPION));
    assert_eq!(
        replica_todo(&s.proj, &elsewhere).status.kind(),
        StatusKind::InProgress
    );
    assert_eq!(replica_todo(&s.proj, &numbered).status, Status::Pending);
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
fn an_issue_url_is_not_a_key_with_no_tracker_in_reach() {
    let s = two_issues();
    ok(s.sync(&[]));
    std::fs::remove_file(s.proj.path.join("devkit.toml")).unwrap();
    let url = "https://github.com/o/r/issues/66";
    let out = s.devkit(&["todo", "start", url], &S1);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains(&format!("no todo {url}")),
        "{}",
        stderr(&out)
    );
    assert_eq!(s.todo_for("o/r#66").status, Status::Pending);
}

#[test]
fn an_issue_url_on_another_host_is_not_a_key() {
    let s = two_issues();
    ok(s.sync(&[]));
    let url = "https://ghe.example/o/r/issues/66";
    let out = s.devkit(&["todo", "start", url], &S1);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains(&format!("no todo {url}")),
        "{}",
        stderr(&out)
    );
    assert_eq!(s.todo_for("o/r#66").status, Status::Pending);
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

fn listed(p: &Proj, args: &[&str], env: &[(&str, &str)]) -> String {
    let out = p.devkit(args, env);
    assert!(out.status.success(), "{}", stderr(&out));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn ready_and_plan_show_word_names_by_default() {
    let p = Proj::new();
    let id = added(&p, "one");
    let name = word_name(&id).unwrap();
    for command in ["ready", "plan"] {
        let out = listed(&p, &["todo", command], &TASKCHAMPION);
        assert!(out.contains(&name), "{command}: {out}");
        assert!(!out.contains(&id[..8]), "{command}: {out}");
    }
}

#[test]
fn the_flag_overrides_the_config() {
    let p = Proj::with_home_config("[todo]\nids = \"short\"\n");
    let id = added(&p, "one");
    let ready = |extra: &[&str]| listed(&p, &[&["todo", "ready"], extra].concat(), &TASKCHAMPION);
    assert_eq!(ready(&[]), format!("{}  one  *\n", &id[..8]));
    assert_eq!(ready(&["--ids", "full"]), format!("{id}  one  *\n"));
    assert_eq!(
        ready(&["--ids", "name"]),
        format!("{}  one  *\n", word_name(&id).unwrap())
    );
    let refused = p.devkit(&["todo", "ready", "--ids", "hex"], &TASKCHAMPION);
    assert_eq!(refused.status.code(), Some(2), "{}", stderr(&refused));
}

#[test]
fn the_flag_overrides_the_config_in_the_injected_lists() {
    let p = Proj::with_home_config("[todo]\nids = \"name\"\n");
    let id = added(&p, "one");
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "s1",
        "cwd": p.path,
        "source": "startup",
    })
    .to_string();
    let injected = |style: &str| {
        let args = [
            "todo",
            "context",
            "--harness",
            "claude-code",
            "--ids",
            style,
        ];
        let out = p.devkit_in(&p.path, &args, &TASKCHAMPION, &payload);
        assert!(out.status.success(), "{}", stderr(&out));
        String::from_utf8(out.stdout).unwrap()
    };
    let full = injected("full");
    assert!(full.contains(&format!("one ({id})")), "{full}");
    let short = injected("short");
    assert!(short.contains(&format!("one ({})", &id[..8])), "{short}");
    assert!(!short.contains(&id), "{short}");
}

#[test]
fn builtin_ids_show_whole() {
    let p = Proj::new();
    ok(p.devkit(&["todo", "add", "one"], &S1));
    for style in ["name", "short", "full"] {
        let out = listed(&p, &["todo", "ready", "--ids", style], &S1);
        assert_eq!(out, "1  one  *\n", "{style}");
    }
}

#[test]
fn add_prints_the_name() {
    let p = Proj::new();
    let out = listed(&p, &["todo", "add", "one"], &TASKCHAMPION);
    let id = replica(&p)[0].id.clone();
    assert_eq!(out, format!("{}\n", word_name(&id).unwrap()));
}

#[test]
fn ticket_rows_show_the_key_once() {
    let s = two_issues();
    ok(s.sync(&[]));
    devkit_common::record::write(&s.proj.path, &devkit_common::record::IssueRecord {
        issue: "66".into(),
        slug: "fix".into(),
        origin: Some(devkit_common::record::RecordOrigin::Setup),
        ..Default::default()
    })
    .unwrap();
    let plan = listed(&s.proj, &["todo", "plan"], &S1);
    assert!(plan.contains("o #66  Issue 66"), "{plan}");
    assert!(!plan.contains("#66 #66"), "{plan}");
    let ready = listed(&s.proj, &["todo", "ready"], &S1);
    assert!(ready.contains("#66  Issue 66"), "{ready}");
    assert!(!ready.contains("#66 #66"), "{ready}");
    let list = listed(&s.proj, &["todo", "list"], &S1);
    assert!(list.contains("- [ ] Issue 65 (#65"), "{list}");
    let full = listed(&s.proj, &["todo", "list", "--ids", "full"], &S1);
    let id = s.todo_for("o/r#65").id;
    assert!(
        full.contains(&format!("- [ ] #65 Issue 65 ({id}")),
        "{full}"
    );
}

#[test]
fn another_repos_ticket_shows_its_full_key() {
    let s = two_issues();
    ok(s.sync(&[]));
    s.proj
        .store()
        .add(devkit_todo::NewTodo {
            description: "#7 Elsewhere".into(),
            project: Some("other".into()),
            parent: None,
            order: None,
            ticket: Some(devkit_todo::TicketRef {
                tracker: "github".into(),
                key: "x/y#7".into(),
                synced_status: None,
            }),
        })
        .unwrap();
    let list = listed(&s.proj, &["todo", "list", "--all"], &S1);
    assert!(list.contains("- [ ] Elsewhere (x/y#7"), "{list}");
    assert!(list.contains("- [ ] Issue 66 (#66"), "{list}");
    assert!(!list.contains("(#7"), "{list}");
}

#[test]
fn list_json_keeps_full_ids() {
    let p = Proj::new();
    let id = added(&p, "one");
    let out = listed(&p, &["todo", "list", "--json"], &TASKCHAMPION);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    assert_eq!(rows[0]["id"], id.as_str());
    assert_eq!(rows[0]["description"], "one");
}

#[test]
fn mermaid_keeps_hex_node_ids_and_labels_names() {
    let p = Proj::new();
    let id = added(&p, "one");
    let out = listed(&p, &["todo", "plan", "--mermaid"], &TASKCHAMPION);
    let name = word_name(&id).unwrap();
    assert!(
        out.contains(&format!("t{}[\"{name}: one\"]", &id[..8])),
        "{out}"
    );
}

#[test]
fn a_status_verb_with_no_ids_is_an_error() {
    let p = Proj::new();
    let id = added(&p, "one");
    for verb in ["start", "stop", "done", "undone", "cancel"] {
        let out = p.devkit(&["todo", verb], &TASKCHAMPION);
        assert_eq!(out.status.code(), Some(2), "todo {verb}: {}", stderr(&out));
    }
    assert_eq!(replica_todo(&p, &id).status.kind(), StatusKind::Pending);
}

#[test]
fn a_quoted_hash_key_resolves() {
    let s = two_issues();
    ok(s.sync(&[]));
    ok(s.devkit(&["todo", "start", "#66"], &S1));
    assert_eq!(s.todo_for("o/r#66").status.kind(), StatusKind::InProgress);
    ok(s.devkit(&["todo", "stop", "66"], &S1));
    assert_eq!(s.todo_for("o/r#66").status, Status::Pending);
    for name in ["#", "#abc", "#999"] {
        let out = s.devkit(&["todo", "start", name], &S1);
        assert_eq!(out.status.code(), Some(1), "{name:?}");
        assert!(
            stderr(&out).contains(&format!("no todo {name}")),
            "{name:?}: {}",
            stderr(&out)
        );
    }
}

/// Two todos on the replica, the second blocked by the first, with their
/// full ids.
fn blocked_pair(p: &Proj) -> (String, String) {
    let first = added(p, "first");
    let name = word_name(&first).unwrap();
    let out = p.devkit(
        &["todo", "add", "second", "--blocked-by", &name],
        &TASKCHAMPION,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    (
        first,
        replica(p)
            .into_iter()
            .find(|t| t.description == "second")
            .unwrap()
            .id,
    )
}

#[test]
fn a_blocked_start_names_the_todos() {
    let p = Proj::new();
    let (first, second) = blocked_pair(&p);
    let out = p.devkit(&["todo", "start", &second], &TASKCHAMPION);
    assert!(!out.status.success());
    let want = format!(
        "todo {} is blocked by {}",
        word_name(&second).unwrap(),
        word_name(&first).unwrap()
    );
    assert!(stderr(&out).contains(&want), "{}", stderr(&out));
}

#[test]
fn unblocked_names_the_todo() {
    let p = Proj::new();
    let (first, second) = blocked_pair(&p);
    let out = listed(&p, &["todo", "done", &first], &TASKCHAMPION);
    assert_eq!(out, format!("unblocked: {}\n", word_name(&second).unwrap()));
    let p = Proj::new();
    let (first, second) = blocked_pair(&p);
    let out = listed(
        &p,
        &["todo", "done", &first, "--ids", "short"],
        &TASKCHAMPION,
    );
    assert_eq!(out, format!("unblocked: {}\n", &second[..8]));
}

#[test]
fn a_cycle_refusal_names_the_todos() {
    let p = Proj::new();
    let (first, second) = blocked_pair(&p);
    let out = p.devkit(&["todo", "block", &first, "--by", &second], &TASKCHAMPION);
    assert!(!out.status.success());
    let (first, second) = (word_name(&first).unwrap(), word_name(&second).unwrap());
    let want = format!("blocking {first} on {second} would close the cycle");
    assert!(stderr(&out).contains(&want), "{}", stderr(&out));
}

#[test]
fn the_stop_hook_names_open_todos_as_the_config_says() {
    for ids in ["", "ids = \"short\"\n"] {
        let p = Proj::with_home_config(&format!("[todo]\nhold_stop = \"always\"\n{ids}"));
        let id = added(&p, "one");
        let payload = serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": "s1",
            "stop_hook_active": false,
            "cwd": p.path,
        });
        let out = p.hook_with("stop", "claude-code", &payload, &TASKCHAMPION);
        assert!(out.status.success(), "{}", stderr(&out));
        let reason = String::from_utf8_lossy(&out.stdout);
        let name = if ids.is_empty() {
            word_name(&id).unwrap()
        } else {
            id[..8].to_string()
        };
        assert!(reason.contains(&format!("one ({name})")), "{ids}: {reason}");
    }
}

#[test]
fn activity_names_held_todos_as_the_config_says() {
    for ids in ["", "[todo]\nids = \"short\"\n"] {
        let p = Proj::with_home_config(ids);
        let id = added(&p, "one");
        ok(p.devkit(&["todo", "start", &id], &TASKCHAMPION));
        ok(p.devkit(&["todo", "done", &id], &TASKCHAMPION));
        let report = listed(&p, &["activity"], &TASKCHAMPION);
        let (shown, hidden) = (word_name(&id).unwrap(), id[..8].to_string());
        let (shown, hidden) = if ids.is_empty() {
            (shown, hidden)
        } else {
            (hidden, shown)
        };
        assert!(report.contains(&shown), "{ids}: {report}");
        assert!(!report.contains(&hidden), "{ids}: {report}");
    }
}
