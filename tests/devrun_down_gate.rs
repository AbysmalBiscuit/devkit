//! `devrun down --all` refuses to touch another worktree without a terminal.
use std::path::PathBuf;
use std::process::Command;

mod common;
use common::unique;

#[test]
fn down_all_without_tty_refuses() {
    // Throwaway state home with a seeded foreign reservation.
    let home = std::env::temp_dir().join(format!("devrun-gate-{}", unique()));
    let xdg_state = home.join("state");
    let devkit_dir = xdg_state.join("devkit");
    std::fs::create_dir_all(devkit_dir.join("logs")).unwrap();

    // A foreign holder dir that exists on disk (so snapshot() does not prune it).
    let foreign = home.join("foreign-wt");
    std::fs::create_dir_all(&foreign).unwrap();

    // A current worktree that is a real git repo (toplevel resolves), distinct from foreign.
    let cur = home.join("cur-wt");
    std::fs::create_dir_all(&cur).unwrap();
    run_git(&cur, &["init", "-q"]);

    // Seed ports.json: one pidless, fresh reservation under the foreign holder.
    let now = now_secs();
    let ports_json = format!(
        r#"{{"version":1,"entries":{{"9100":{{"app":"api","holder":{holder:?},"role":"issue","pid":null,"logfile":null,"ts":{now}}}}}}}"#,
        holder = foreign.to_string_lossy(),
    );
    std::fs::write(devkit_dir.join("ports.json"), ports_json).unwrap();

    let bin = env!("CARGO_BIN_EXE_devrun");
    let out = Command::new(bin)
        .arg("-C")
        .arg(&cur)
        .args(["down", "--all"])
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &xdg_state)
        // No daemon; force the direct path.
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run devrun down --all");

    assert!(
        !out.status.success(),
        "expected non-zero exit; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("requires an interactive terminal"),
        "stderr was: {stderr}"
    );

    // The reservation must be untouched.
    let after = std::fs::read_to_string(devkit_dir.join("ports.json")).unwrap();
    assert!(after.contains("9100"), "reservation must survive a refusal");

    let _ = std::fs::remove_dir_all(&home);
}

fn run_git(dir: &PathBuf, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn now_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
