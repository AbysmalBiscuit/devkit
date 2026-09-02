//! Shared fixtures for the baseline integration tests: a project whose
//! baselines land under `<name>_worktrees/_baselines`, and a `devrun up` that
//! builds one without leaving a server behind.
//!
//! Compile-time unused helpers are expected: different test binaries include
//! this module via `#[path]` and use different subsets of it.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub fn git(cwd: &Path, args: &[&str]) {
    devkit_common::git::Git::fixture(cwd)
        .args(args.iter().copied())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
}

/// `HOME`, `XDG_STATE_HOME` and `XDG_CONFIG_HOME` all point at the run's own
/// tempdir, so the developer's port registry and personal `config.toml` take no
/// part in the run.
pub fn devkit(cwd: &Path, state: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_devkit"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("XDG_CONFIG_HOME", state.join("config"))
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_SESSION")
        .output()
        .unwrap_or_else(|e| panic!("spawn devkit {args:?}: {e}"))
}

pub fn devkit_ok(cwd: &Path, state: &Path, args: &[&str]) -> Output {
    let out = devkit(cwd, state, args);
    assert!(
        out.status.success(),
        "devkit {args:?} exited {}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// The app's launch program, which is a path nothing occupies. See [`up`].
pub const MISSING_PROGRAM: &str = "no-such-server";

/// A project whose `worktree_root` is derived (`<name>_worktrees` beside the
/// checkout), so its baselines land in `proj_worktrees/_baselines`. An app is
/// required: `cmd_up` bails with "no apps to run" before it builds any group,
/// so a project with none never reaches the baseline path.
pub fn project(at: &Path) {
    std::fs::create_dir_all(at.join("apps").join("api")).unwrap();
    git(at, &["init", "-q", "-b", "main"]);
    // A TOML literal string: a Windows path's backslashes are not escapes.
    let program = at.join(MISSING_PROGRAM);
    std::fs::write(
        at.join("devkit.toml"),
        format!(
            "[config]\nroot = true\n\
             [defaults]\nbaseline_ref = 'main'\n\
             [apps.api]\nbase_port = 4000\npath = 'apps/api'\n\
             launch = ['{}', '{{{{ port }}}}']\n",
            program.display()
        ),
    )
    .unwrap();
    git(at, &["add", "-A"]);
    git(at, &["commit", "-qm", "init"]);
}

/// `devrun up` for the baseline role, run for its effects on the baseline
/// directory, the worktree's pin and the port registry.
///
/// The app's launch names a path nothing occupies, so the run performs every
/// step up to the spawn — fork point, bootstrap, repin, port allocation, plan —
/// and then fails in `Command::spawn` without starting a server. A launch that
/// did spawn would have to bind its port to satisfy the 120 s readiness poll,
/// and no program guaranteed on ubuntu, macos and windows alike binds a port
/// given on its command line. Asserting on the spawn error is what keeps the
/// run from passing for having failed earlier.
pub fn up(wt: &Path, state: &Path) {
    let out = devkit(wt, state, &["run", "up", "--role", "baseline", "api"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("spawning") && stderr.contains(MISSING_PROGRAM),
        "up did not reach the spawn (exited {}): {stderr}",
        out.status
    );
}

/// The baseline slots under `root`. `.locks` sits alongside them and is not one.
pub fn slots(root: &Path) -> Vec<PathBuf> {
    let Ok(dir) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = dir
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n != ".locks"))
        .collect();
    found.sort();
    found
}

/// The `path` under `[baseline]` in a worktree's record.
pub fn baseline_of(wt: &Path) -> String {
    let body = std::fs::read_to_string(wt.join(".devkit").join("issue.toml")).unwrap();
    let doc: toml::Value = toml::from_str(&body).unwrap();
    doc["baseline"]["path"].as_str().unwrap().to_string()
}

/// Every holder the port registry currently has a row for.
pub fn holders(state: &Path) -> Vec<String> {
    let Ok(body) = std::fs::read_to_string(state.join("devkit").join("ports.json")) else {
        return Vec::new();
    };
    let doc: serde_json::Value = serde_json::from_str(&body).unwrap();
    doc["entries"]
        .as_object()
        .map(|m| {
            m.values()
                .filter_map(|e| e["holder"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}
