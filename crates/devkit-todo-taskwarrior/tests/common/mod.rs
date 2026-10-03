//! A private taskwarrior in a temp dir, so the user's taskrc, data and hooks
//! never run.

use std::{
    path::Path,
    process::{Command, Stdio},
};

use devkit_todo::{Filter, TodoStore};
use devkit_todo_taskwarrior::{TaskwarriorNotFound, TaskwarriorStore, schema};
use tempfile::TempDir;

fn env(dir: &Path) -> Vec<(String, String)> {
    vec![
        ("TASKRC".into(), dir.join("taskrc").display().to_string()),
        ("TASKDATA".into(), dir.join("data").display().to_string()),
    ]
}

/// A store on an empty private taskwarrior, or `None` when `task` is not
/// installed. Any other failure of its first export panics, so a broken
/// backend never passes as a skip.
pub fn private() -> Option<(TempDir, TaskwarriorStore)> {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::write(dir.path().join("taskrc"), "").expect("an empty taskrc");
    let store = TaskwarriorStore::new("task")
        .with_env(env(dir.path()))
        .with_lock_at(dir.path().join("taskwarrior.lock"));
    match store.list(&Filter::all()) {
        Err(e) if e.downcast_ref::<TaskwarriorNotFound>().is_some() => None,
        result => {
            result.expect("a private taskwarrior exports");
            Some((dir, store))
        }
    }
}

/// `task` run directly against the private taskwarrior in `dir`, the way a
/// person would, with the UDAs declared. Returns stdout.
pub fn task(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("task")
        .envs(env(dir))
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

/// The task `uuid` as `task export` prints it.
pub fn exported(dir: &Path, uuid: &str) -> serde_json::Value {
    let tasks: serde_json::Value = serde_json::from_str(&task(dir, &[uuid, "export"])).unwrap();
    tasks[0].clone()
}

/// Adds a task the way a person would, and returns its uuid.
pub fn add_by_hand(dir: &Path, args: &[&str]) -> String {
    let out = task(dir, &[&["rc.verbose=new-uuid", "add"], args].concat());
    out.trim()
        .strip_prefix("Created task ")
        .and_then(|rest| rest.strip_suffix('.'))
        .unwrap_or_else(|| panic!("no uuid in {out:?}"))
        .to_string()
}
