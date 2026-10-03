//! The retry on a locked taskwarrior database, against a fake `task` that
//! fails a set number of times before it answers. The fake is a shell
//! script, so these run on Unix only.
#![cfg(unix)]

use std::{os::unix::fs::PermissionsExt, path::Path};

use devkit_todo::{Filter, TodoStore};
use devkit_todo_taskwarrior::TaskwarriorStore;

/// Fails its first `$FAILS` runs with `$MESSAGE` on stderr, then prints an
/// empty export. Counts every run in `$COUNT`.
const FAKE_TASK: &str = r#"#!/bin/sh
n=$(cat "$COUNT" 2>/dev/null || echo 0)
n=$((n + 1))
echo "$n" > "$COUNT"
if [ "$n" -le "$FAILS" ]; then
    echo "$MESSAGE" >&2
    exit 1
fi
echo '[]'
"#;

/// Lists through the fake, and returns the result and how many times `task`
/// ran.
fn list_through_fake(fails: u32, message: &str) -> (anyhow::Result<usize>, u32) {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("task");
    std::fs::write(&program, FAKE_TASK).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let count = dir.path().join("count");
    let store = TaskwarriorStore::new(program.to_string_lossy())
        .with_env(vec![
            ("COUNT".into(), count.to_string_lossy().into_owned()),
            ("FAILS".into(), fails.to_string()),
            ("MESSAGE".into(), message.into()),
        ])
        .with_lock_at(dir.path().join("taskwarrior.lock"));
    let listed = store.list(&Filter::all()).map(|todos| todos.len());
    (listed, runs(&count))
}

fn runs(count: &Path) -> u32 {
    std::fs::read_to_string(count)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
fn a_locked_database_is_retried_until_it_answers() {
    let (listed, runs) = list_through_fake(3, "database is locked");
    assert_eq!(listed.unwrap(), 0);
    assert_eq!(runs, 4);
}

#[test]
fn a_database_locked_past_the_retries_is_an_error() {
    let (listed, runs) = list_through_fake(4, "database is locked");
    let err = listed.unwrap_err();
    assert!(err.to_string().contains("database is locked"), "{err:#}");
    assert_eq!(runs, 4);
}

#[test]
fn any_other_failure_is_not_retried() {
    let (listed, runs) = list_through_fake(1, "Mismatched parentheses in expression");
    let err = listed.unwrap_err();
    assert!(
        err.to_string().contains("Mismatched parentheses"),
        "{err:#}"
    );
    assert_eq!(runs, 1);
}
