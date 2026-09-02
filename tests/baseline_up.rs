//! `devrun up --role baseline` over the real binary: one baseline per fork
//! point, a repin when the fork point moves, and a baseline that is left alone
//! when `up` runs from inside one.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn git(cwd: &Path, args: &[&str]) {
    devkit_common::git::Git::fixture(cwd)
        .args(args.iter().copied())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
}

/// `HOME`, `XDG_STATE_HOME` and `XDG_CONFIG_HOME` all point at the run's own
/// tempdir, so the developer's port registry and personal `config.toml` take no
/// part in the run.
fn devkit(cwd: &Path, state: &Path, args: &[&str]) -> Output {
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

fn devkit_ok(cwd: &Path, state: &Path, args: &[&str]) -> Output {
    let out = devkit(cwd, state, args);
    assert!(
        out.status.success(),
        "devkit {args:?} exited {}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

const PROJECT: &str = "[config]\nroot = true\n\
                       [defaults]\nbaseline_ref = 'main'\n\
                       [apps.api]\nbase_port = 4000\npath = 'apps/api'\nlaunch = ['echo', 'x']\n";

/// A project whose `worktree_root` is derived (`<name>_worktrees` beside the
/// checkout), so its baselines land in `proj_worktrees/_baselines`. An app is
/// required: `cmd_up` bails with "no apps to run" before it builds any group,
/// so a project with none never reaches the baseline path.
fn project(at: &Path) {
    std::fs::create_dir_all(at.join("apps").join("api")).unwrap();
    git(at, &["init", "-q", "-b", "main"]);
    std::fs::write(at.join("devkit.toml"), PROJECT).unwrap();
    git(at, &["add", "-A"]);
    git(at, &["commit", "-qm", "init"]);
}

/// The baseline slots under `root`. `.locks` sits alongside them and is not one.
fn slots(root: &Path) -> Vec<PathBuf> {
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
fn baseline_of(wt: &Path) -> String {
    let body = std::fs::read_to_string(wt.join(".devkit").join("issue.toml")).unwrap();
    let doc: toml::Value = toml::from_str(&body).unwrap();
    doc["baseline"]["path"].as_str().unwrap().to_string()
}

/// Every holder the port registry currently has a row for.
fn holders(state: &Path) -> Vec<String> {
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

/// Two worktrees cut from the same commit resolve to one baseline directory.
#[test]
fn two_worktrees_at_one_fork_point_share_a_baseline() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let repo = tmp.path().join("proj");
    project(&repo);

    for name in ["a", "b"] {
        let wt = tmp.path().join("proj_worktrees").join(name);
        git(
            &repo,
            &["worktree", "add", "-b", name, wt.to_str().unwrap()],
        );
        devkit_ok(
            &wt,
            &state,
            &["run", "up", "--role", "baseline", "--dry-run", "api"],
        );
    }

    let found = slots(&tmp.path().join("proj_worktrees").join("_baselines"));
    assert_eq!(found.len(), 1, "one fork point, one baseline: {found:?}");
}

/// A rebase moves the merge base, so `up` repins. The old baseline's rows have
/// a holder no worktree names any more, so they come down first.
#[test]
fn repinning_stops_the_abandoned_baselines_servers() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let repo = tmp.path().join("proj");
    project(&repo);

    let wt = tmp.path().join("proj_worktrees").join("a");
    git(&repo, &["worktree", "add", "-b", "a", wt.to_str().unwrap()]);
    // `--dry-run` skips the spawn, not the group build: the baseline is still
    // created and pinned, which is all this asserts. Nothing is launched, so
    // `launch` never has to be a real program on any platform.
    devkit_ok(
        &wt,
        &state,
        &["run", "up", "--role", "baseline", "--dry-run", "api"],
    );
    let first = baseline_of(&wt);

    // A pid-less reservation is what `ports alloc` writes before anything
    // binds, so the repin's teardown releases the row instead of signalling a
    // process — no child to spawn, and nothing to poll for.
    devkit_ok(
        &wt,
        &state,
        &[
            "ports", "alloc", "--holder", &first, "--role", "baseline", "api",
        ],
    );
    assert!(holders(&state).contains(&first), "reservation not seeded");

    git(&repo, &["commit", "-qm", "second", "--allow-empty"]);
    git(&wt, &["rebase", "-q", "main"]);
    devkit_ok(
        &wt,
        &state,
        &["run", "up", "--role", "baseline", "--dry-run", "api"],
    );

    let second = baseline_of(&wt);
    assert_ne!(first, second, "the record still names the old baseline");
    assert!(
        !holders(&state).contains(&first),
        "rows under the abandoned baseline survived the repin"
    );
}

/// A baseline is its own baseline. `up --role baseline` from inside one must
/// serve from the tree it is standing in: no second tree bootstrapped under it,
/// and no issue record written into it — the record would name a baseline
/// pinned to itself, under the `DETACHED` identity a baseline's HEAD reports.
#[test]
fn up_inside_a_baseline_neither_bootstraps_nor_pins() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let repo = tmp.path().join("proj");
    project(&repo);

    let wt = tmp.path().join("proj_worktrees").join("a");
    git(&repo, &["worktree", "add", "-b", "a", wt.to_str().unwrap()]);
    devkit_ok(
        &wt,
        &state,
        &["run", "up", "--role", "baseline", "--dry-run", "api"],
    );

    let root = tmp.path().join("proj_worktrees").join("_baselines");
    let baseline = slots(&root).pop().expect("a baseline");
    devkit_ok(
        &baseline,
        &state,
        &["run", "up", "--role", "baseline", "--dry-run", "api"],
    );

    assert_eq!(slots(&root), vec![baseline.clone()], "a baseline nested");
    assert!(
        !baseline.join(".devkit").join("issue.toml").exists(),
        "an issue record was written into a baseline"
    );
}
