//! A replica synced to a directory reads the same in taskwarrior, and
//! taskwarrior's edits read back in devkit. Each test returns early when
//! `task` is not installed.

use std::{
    path::Path,
    process::{Command, Stdio},
};

use devkit_todo::{Edit, Holder, NewTodo, Status, StatusKind, TodoStore};
use devkit_todo_taskchampion::{SyncTarget, TaskchampionStore};
use devkit_todo_taskwarrior::schema;

/// Whether `task` is installed. Any failure other than a missing program
/// panics, so a broken taskwarrior never passes as a skip.
fn installed() -> bool {
    match Command::new("task").arg("--version").output() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipping: taskwarrior is not installed");
            false
        }
        out => {
            assert!(out.expect("task runs").status.success());
            true
        }
    }
}

/// `task` against a private taskwarrior in `dir` whose sync server is
/// `dir/server`. Returns stdout.
fn task(dir: &Path, args: &[&str]) -> String {
    let rc = dir.join("taskrc");
    if !rc.exists() {
        let server = dir.join("server");
        std::fs::write(&rc, format!("sync.local.server_dir={}\n", server.display())).unwrap();
    }
    let out = Command::new("task")
        .env("TASKRC", &rc)
        .env("TASKDATA", dir.join("tw"))
        .args(schema::UDAS.map(|(k, v)| format!("rc.{k}={v}")))
        .args(["rc.confirmation=off", "rc.bulk=0", "rc.json.array=on"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("task runs");
    assert!(
        out.status.success(),
        "task {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// A store on `dir/replica` that syncs to `dir/server`, holding a parent
/// "ship" and a child "spec" that `S/a` has started, both synced.
fn synced(dir: &Path) -> (TaskchampionStore, String, String) {
    let store =
        TaskchampionStore::at(dir.join("replica")).with_target(SyncTarget::Dir(dir.join("server")));
    let new = |description: &str, parent: Option<String>| NewTodo {
        project: Some("proj.main".into()),
        description: description.into(),
        parent,
        order: None,
    };
    let parent = store.add(new("ship", None)).unwrap();
    let child = store.add(new("spec", Some(parent.clone()))).unwrap();
    store
        .apply(&Edit::SetStatus {
            id: child.clone(),
            to: StatusKind::InProgress,
            actor: Holder::new("S/a"),
        })
        .unwrap();
    store.sync_once().unwrap();
    (store, parent, child)
}

fn exported(dir: &Path, uuid: &str) -> serde_json::Value {
    let tasks: serde_json::Value = serde_json::from_str(&task(dir, &[uuid, "export"])).unwrap();
    tasks[0].clone()
}

#[test]
fn taskwarrior_reads_a_synced_replica() {
    let dir = tempfile::tempdir().unwrap();
    if !installed() {
        return;
    }
    let (_store, parent, child) = synced(dir.path());
    task(dir.path(), &["sync"]);
    let task = exported(dir.path(), &child);
    assert_eq!(task["description"], "spec", "{task}");
    assert_eq!(task["project"], "devkit.proj.main", "{task}");
    assert_eq!(task["status"], "pending", "{task}");
    assert!(task["start"].is_string(), "{task}");
    assert_eq!(task["holder"], "S/a", "{task}");
    assert_eq!(task["subof"], parent.as_str(), "{task}");
    assert_eq!(task["order"].as_f64(), Some(1024.0), "{task}");
}

#[test]
fn a_devkit_replica_reads_taskwarriors_edits() {
    let dir = tempfile::tempdir().unwrap();
    if !installed() {
        return;
    }
    let (store, _parent, child) = synced(dir.path());
    task(dir.path(), &["sync"]);
    task(dir.path(), &[&child, "done"]);
    task(dir.path(), &["sync"]);
    store.sync_once().unwrap();
    assert!(matches!(
        store.get(&child).unwrap().unwrap().status,
        Status::Completed { .. }
    ));
}
