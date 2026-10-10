//! `--role workspace` names a worktree's own servers on `devrun` and `portm`.
//! `issue`, the role's old name, is still accepted on the command line, and a
//! registry row written under it reads as a workspace row.

#[path = "common/baselinetest.rs"]
mod baselinetest;

use std::path::Path;

use baselinetest::{devkit_ok, git, holders};

/// A registry holding one pid-less reservation for `holder`, written the way
/// a devkit that called the role `issue` stored it.
fn seed_old_record(state: &Path, holder: &str) {
    let dir = state.join("devkit");
    std::fs::create_dir_all(&dir).unwrap();
    let doc = serde_json::json!({
        "version": 1,
        "entries": {
            "9150": {
                "app": "api",
                "holder": holder,
                "role": "issue",
                "pid": null,
                "logfile": null,
                "ts": 1,
            }
        }
    });
    std::fs::write(dir.join("ports.json"), doc.to_string()).unwrap();
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn portm_releases_an_old_issue_record_as_workspace() {
    for role in ["workspace", "issue"] {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let holder = tmp.path().join("wt");
        std::fs::create_dir_all(&holder).unwrap();
        let holder = holder.to_str().unwrap();
        seed_old_record(&state, holder);

        let out = devkit_ok(tmp.path(), &state, &[
            "ports", "release", "--holder", holder, "--role", role,
        ]);

        assert_eq!(stdout(&out).trim(), "released: [9150]", "--role {role}");
        assert!(holders(&state).is_empty(), "--role {role}");
    }
}

#[test]
fn portm_keeps_an_old_issue_record_out_of_a_baseline_release() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let holder = tmp.path().join("wt");
    std::fs::create_dir_all(&holder).unwrap();
    let holder = holder.to_str().unwrap();
    seed_old_record(&state, holder);

    let out = devkit_ok(tmp.path(), &state, &[
        "ports", "release", "--holder", holder, "--role", "baseline",
    ]);

    assert_eq!(stdout(&out).trim(), "released: []");
    assert_eq!(holders(&state), [holder]);
}

#[test]
fn devrun_down_stops_an_old_issue_record_as_workspace() {
    for role in ["workspace", "issue"] {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let repo = tmp.path().join("proj");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        let root = std::fs::canonicalize(&repo).unwrap();
        let root = root.to_str().unwrap();
        seed_old_record(&state, root);

        devkit_ok(&repo, &state, &["run", "down", "--role", role]);

        assert!(holders(&state).is_empty(), "--role {role}");
    }
}
