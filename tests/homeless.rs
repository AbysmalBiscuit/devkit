//! Systemd units, cron and some sandboxes run devkit with no `HOME`. Startup
//! must survive that: hooks fail open and `devkit doctor` diagnoses a broken
//! environment, so neither can afford a panic before it parses its arguments.

#[path = "common/testenv.rs"]
mod testenv;

use std::process::{Command, Output};

/// The exit code Rust gives a process that panicked.
const PANIC_EXIT: i32 = 101;

fn homeless(args: &[&str]) -> Output {
    let cwd = tempfile::tempdir().expect("temp cwd");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    testenv::scrub_identity(&mut cmd);
    testenv::scrub_xdg_homes(&mut cmd)
        .env_remove("HOME")
        .env_remove("XDG_STATE_HOME")
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .current_dir(cwd.path())
        .args(args);
    cmd.output()
        .unwrap_or_else(|e| panic!("spawn devkit {args:?}: {e}"))
}

fn assert_no_panic(args: &[&str]) -> Output {
    let out = homeless(args);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(
        out.status.code(),
        Some(PANIC_EXIT),
        "devkit {args:?} panicked without HOME: {stderr}"
    );
    assert!(
        !stderr.contains("panicked") && !stderr.contains("HOME must be set"),
        "devkit {args:?} reported a panic without HOME: {stderr}"
    );
    out
}

#[test]
fn version_runs_without_home() {
    let out = assert_no_panic(&["--version"]);
    assert!(out.status.success(), "devkit --version should exit 0");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("devkit {}", devkit::VERSION)
    );
}

#[test]
fn startup_still_migrates_the_legacy_state_home() {
    let (home, mut cmd) = testenv::isolated(env!("CARGO_BIN_EXE_devkit"));
    let legacy = home.path().join(".claude/state/devkit");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("ports.json"), b"{}").unwrap();
    let xdg = home.path().join("xdg-state");

    let out = cmd
        .env("XDG_STATE_HOME", &xdg)
        .arg("--version")
        .output()
        .expect("spawn devkit --version");

    assert!(out.status.success(), "devkit --version should exit 0");
    assert!(
        xdg.join("devkit/ports.json").exists(),
        "the legacy state moved under XDG_STATE_HOME"
    );
    assert!(!legacy.exists(), "the legacy state home is gone after the move");
}
