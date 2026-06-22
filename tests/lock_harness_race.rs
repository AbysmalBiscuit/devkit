//! Two holders race to write the same free file through the real `decide_write`
//! facade in separate processes; exactly one acquires. Registry isolated via
//! HOME + XDG_STATE_HOME pinned to a temp dir (mirrors tests/registry.rs).

use devkit_locks::model::WriteDecision;
use std::process::Command;

#[test]
fn concurrent_write_decide_yields_one_winner() {
    // Worker mode: decide a write on the shared path and print the outcome.
    if let Ok(holder) = std::env::var("DEVKIT_TEST_WRITE") {
        let file = std::env::var("DEVKIT_TEST_FILE").unwrap();
        let d = devkit_locks::decide_write(&file, &holder, Some("race"), 0).unwrap();
        let tag = match d {
            WriteDecision::Acquired => "acquired",
            WriteDecision::AllowedByOwnership => "owned",
            WriteDecision::Denied(_) => "denied",
        };
        print!("{tag}");
        std::process::exit(0);
    }

    let tmp = std::env::temp_dir().join(format!("devkit-wrace-{}", std::process::id()));
    let repo = tmp.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    let file = repo.join("src/a.rs");
    let exe = std::env::current_exe().unwrap();

    let kids: Vec<_> = ["A", "B"]
        .into_iter()
        .map(|holder| {
            Command::new(&exe)
                // Pin BOTH state-home inputs so the worker registry is isolated;
                // setting only one leaks to the developer's real registry.
                .env("HOME", &tmp)
                .env("XDG_STATE_HOME", &tmp)
                .env("DEVKIT_TEST_WRITE", holder)
                .env("DEVKIT_TEST_FILE", &file)
                .args(["--exact", "concurrent_write_decide_yields_one_winner", "--nocapture"])
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();

    let tags: Vec<String> = kids
        .into_iter()
        .map(|c| {
            let o = c.wait_with_output().unwrap();
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .last()
                .unwrap_or("")
                .to_string()
        })
        .collect();

    let acquired = tags.iter().filter(|t| *t == "acquired").count();
    let denied = tags.iter().filter(|t| *t == "denied").count();
    assert_eq!(acquired, 1, "exactly one writer acquires: {tags:?}");
    assert_eq!(denied, 1, "the other is denied: {tags:?}");
    let _ = std::fs::remove_dir_all(&tmp);
}
