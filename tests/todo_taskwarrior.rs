//! `devkit todo` and the todo hooks with `[todo] backend = "taskwarrior"`,
//! against a private taskwarrior. Each test returns early when `task` is not
//! installed.

#[path = "common/todoenv.rs"]
mod todoenv;

use std::{
    path::Path,
    process::{Command, Stdio},
};

use devkit_todo::{Filter, TodoStore};
use devkit_todo_taskwarrior::{TaskwarriorNotFound, TaskwarriorStore};
use serde_json::{Value, json};
use todoenv::{Proj, stderr, stdout};

const SESSION: &str = "732b6b74-6009-478a-abe2-4129415b6007";

/// A project whose home config picks taskwarrior, with `TASKRC` and
/// `TASKDATA` pointing into a temp dir of its own.
struct Tw {
    p: Proj,
    dir: tempfile::TempDir,
    taskrc: String,
    taskdata: String,
}

impl Tw {
    fn new() -> Option<Self> {
        Self::in_repo("proj")
    }

    /// `None` when `task` is not installed. Any other failure of a first
    /// export panics, so a broken backend never passes as a skip.
    fn in_repo(name: &str) -> Option<Self> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("taskrc"), "").unwrap();
        let store = TaskwarriorStore::new("task")
            .with_env(env(dir.path()))
            .with_lock_at(dir.path().join("taskwarrior.lock"));
        match store.list(&Filter::all()) {
            Err(e) if e.downcast_ref::<TaskwarriorNotFound>().is_some() => return None,
            result => {
                result.expect("a private taskwarrior exports");
            }
        }
        let p = Proj::named(name).home_config_of("[todo]\nbackend = \"taskwarrior\"\n");
        let [(_, taskrc), (_, taskdata)]: [(String, String); 2] =
            env(dir.path()).try_into().unwrap();
        Some(Self {
            p,
            dir,
            taskrc,
            taskdata,
        })
    }

    fn env<'a>(&'a self, extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
        let mut env = vec![("TASKRC", &*self.taskrc), ("TASKDATA", &*self.taskdata)];
        env.extend_from_slice(extra);
        env
    }

    fn devkit(&self, args: &[&str], extra: &[(&str, &str)]) -> std::process::Output {
        let out = self.p.devkit(args, &self.env(extra));
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        out
    }

    /// Every task in the private taskwarrior, as `task export` prints it.
    fn export(&self) -> Vec<Value> {
        let out = Command::new("task")
            .envs(env(self.dir.path()))
            .args(["rc.uda.holder.type=string", "rc.json.array=on", "export"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

fn env(dir: &Path) -> Vec<(String, String)> {
    vec![
        ("TASKRC".into(), dir.join("taskrc").display().to_string()),
        ("TASKDATA".into(), dir.join("data").display().to_string()),
    ]
}

const S1: (&str, &str) = ("CLAUDE_CODE_SESSION_ID", "s1");
const HUMAN: (&str, &str) = ("DEVKIT_CALLER", "human");

fn short(out: &std::process::Output) -> String {
    let id = stdout(out).trim().to_string();
    assert!(
        id.len() == 8 && id.chars().all(|c| c.is_ascii_hexdigit()),
        "{id:?}"
    );
    id
}

#[test]
fn the_cli_keeps_todos_in_taskwarrior() {
    let Some(tw) = Tw::new() else {
        return;
    };
    let id = short(&tw.devkit(&["todo", "add", "one"], &[S1]));
    tw.devkit(&["todo", "start", &id], &[S1]);
    let started = tw.export();
    assert_eq!(started[0]["holder"], "s1", "{started:?}");
    assert!(started[0]["start"].is_string(), "{started:?}");
    tw.devkit(&["todo", "done", &id], &[S1]);
    let tasks = tw.export();
    assert_eq!(tasks.len(), 1, "{tasks:?}");
    let task = &tasks[0];
    assert!(task["uuid"].as_str().unwrap().starts_with(&id), "{task}");
    assert_eq!(task["description"], "one");
    assert_eq!(task["status"], "completed");
    assert_eq!(task["holder"], "s1");
    assert_eq!(task["project"], "proj.main.claude-s1");
    assert!(
        !tw.p.state().join("todo/todos.json").exists(),
        "nothing lands in the built-in store"
    );
}

#[test]
fn lists_render_short_ids() {
    let Some(tw) = Tw::new() else {
        return;
    };
    let id = short(&tw.devkit(&["todo", "add", "one"], &[S1]));
    let list = stdout(&tw.devkit(&["todo", "list"], &[S1]));
    assert!(
        list.contains(&format!("## proj.main.claude-s1\n- [ ] one ({id})\n")),
        "{list}"
    );
    let json: Value =
        serde_json::from_str(&stdout(&tw.devkit(&["todo", "list", "--json"], &[S1]))).unwrap();
    let full = json[0]["id"].as_str().unwrap();
    assert_eq!(full.len(), 36, "{json}");
    assert!(full.starts_with(&id), "{json}");
}

#[test]
fn purge_by_short_id_forgets_the_native_mapping() {
    let Some(tw) = Tw::new() else {
        return;
    };
    let create = json!({
        "session_id": SESSION,
        "cwd": tw.p.path,
        "hook_event_name": "PostToolUse",
        "tool_name": "TaskCreate",
        "tool_input": {"subject": "alpha", "description": "alpha"},
        "tool_response": {"task": {"id": "1", "subject": "alpha"}},
    });
    let out =
        tw.p.hook_with("post-tool-use", "claude-code", &create, &tw.env(&[]));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    let tasks = tw.export();
    assert_eq!(tasks.len(), 1, "{tasks:?}");
    let uuid = tasks[0]["uuid"].as_str().unwrap().to_string();
    let native = || std::fs::read_to_string(tw.p.state().join("todo/native.json")).unwrap();
    assert!(native().contains(&uuid), "{}", native());

    tw.devkit(&["todo", "purge", &uuid[..8]], &[HUMAN]);
    assert!(tw.export().is_empty());
    assert!(!native().contains(&uuid), "{}", native());
}

/// A repository whose name taskwarrior's filter syntax would split: a space
/// and a quote.
const ODD_REPO: &str = "bob's repo";

/// Adds `text` to `node` and returns its short id.
fn add_on(tw: &Tw, node: &str, text: &str) -> String {
    short(&tw.devkit(&["todo", "add", text, "--node", node], &[S1]))
}

#[test]
fn an_exact_listing_matches_a_node_with_a_space() {
    let Some(tw) = Tw::in_repo(ODD_REPO) else {
        return;
    };
    let shared = add_on(&tw, "bob's repo.main", "shared");
    let second = add_on(&tw, "bob's repo.main", "second");
    add_on(&tw, "bob's repo.main.claude-s1", "mine");
    let list = stdout(&tw.devkit(&["todo", "list", "--node", "bob's repo.main"], &[S1]));
    assert_eq!(
        list,
        format!("## bob's repo.main\n- [ ] shared ({shared})\n- [ ] second ({second})\n")
    );
    let json: Value = serde_json::from_str(&stdout(
        &tw.devkit(&["todo", "list", "--node", "bob's repo.main", "--json"], &[
            S1,
        ]),
    ))
    .unwrap();
    let orders: Vec<&Value> = json
        .as_array()
        .unwrap()
        .iter()
        .map(|t| &t["order"])
        .collect();
    assert_eq!(orders, [1024, 2048], "{json}");
}

#[test]
fn a_subtree_listing_matches_a_repository_with_a_space() {
    let Some(tw) = Tw::in_repo(ODD_REPO) else {
        return;
    };
    let shared = add_on(&tw, "bob's repo.main", "shared");
    let mine = add_on(&tw, "bob's repo.main.claude-s1", "mine");
    add_on(&tw, "bob's repo-web", "elsewhere");
    let list = stdout(&tw.devkit(&["todo", "list", "--subtree", "bob's repo"], &[S1]));
    assert!(list.contains(&format!("- [ ] shared ({shared})")), "{list}");
    assert!(list.contains(&format!("- [ ] mine ({mine})")), "{list}");
    assert!(!list.contains("elsewhere"), "{list}");
}

#[test]
fn the_default_listing_matches_nodes_with_a_space() {
    let Some(tw) = Tw::in_repo(ODD_REPO) else {
        return;
    };
    let mine = short(&tw.devkit(&["todo", "add", "mine"], &[S1]));
    let shared = add_on(&tw, "bob's repo.main", "shared");
    let list = stdout(&tw.devkit(&["todo", "list"], &[S1]));
    assert_eq!(
        list,
        format!(
            "## bob's repo.main.claude-s1\n- [ ] mine ({mine})\n\n\
             ## bob's repo.main\n- [ ] shared ({shared})\n"
        )
    );
}
