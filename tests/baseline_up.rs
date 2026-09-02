//! `devrun up --role baseline` over the real binary: one baseline per fork
//! point, a repin when the fork point moves, a baseline that is left alone when
//! `up` runs from inside one, and a `--dry-run` that reports a baseline without
//! building, repinning or stopping anything.

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

/// The app's launch program, which is a path nothing occupies. See [`up`].
const MISSING_PROGRAM: &str = "no-such-server";

/// A project whose `worktree_root` is derived (`<name>_worktrees` beside the
/// checkout), so its baselines land in `proj_worktrees/_baselines`. An app is
/// required: `cmd_up` bails with "no apps to run" before it builds any group,
/// so a project with none never reaches the baseline path.
fn project(at: &Path) {
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
fn up(wt: &Path, state: &Path) {
    let out = devkit(wt, state, &["run", "up", "--role", "baseline", "api"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("spawning") && stderr.contains(MISSING_PROGRAM),
        "up did not reach the spawn (exited {}): {stderr}",
        out.status
    );
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
        up(&wt, &state);
    }

    let found = slots(&tmp.path().join("proj_worktrees").join("_baselines"));
    assert_eq!(found.len(), 1, "one fork point, one baseline: {found:?}");
}

/// A rebase moves the merge base, so `up` repins. The rebasing worktree was the
/// old baseline's only referencer, so nothing names it once the pin moves and
/// its rows come down first.
#[test]
fn repinning_stops_the_abandoned_baselines_servers() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let repo = tmp.path().join("proj");
    project(&repo);

    let wt = tmp.path().join("proj_worktrees").join("a");
    git(&repo, &["worktree", "add", "-b", "a", wt.to_str().unwrap()]);
    up(&wt, &state);
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
    up(&wt, &state);

    let second = baseline_of(&wt);
    assert_ne!(first, second, "the record still names the old baseline");
    assert!(
        !holders(&state).contains(&first),
        "rows under the abandoned baseline survived the repin"
    );
}

/// A baseline two worktrees share survives one of them rebasing. Its rows serve
/// the worktree still pinned there, and `up` has no terminal to confirm
/// stopping another worktree's servers with.
#[test]
fn repinning_leaves_a_shared_baselines_servers_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let repo = tmp.path().join("proj");
    project(&repo);

    let a = tmp.path().join("proj_worktrees").join("a");
    let b = tmp.path().join("proj_worktrees").join("b");
    for (name, wt) in [("a", &a), ("b", &b)] {
        git(
            &repo,
            &["worktree", "add", "-b", name, wt.to_str().unwrap()],
        );
        up(wt, &state);
    }
    let shared = baseline_of(&a);
    assert_eq!(shared, baseline_of(&b), "one fork point, one baseline");

    devkit_ok(
        &a,
        &state,
        &[
            "ports", "alloc", "--holder", &shared, "--role", "baseline", "api",
        ],
    );
    assert!(holders(&state).contains(&shared), "reservation not seeded");

    git(&repo, &["commit", "-qm", "second", "--allow-empty"]);
    git(&a, &["rebase", "-q", "main"]);
    up(&a, &state);

    assert_ne!(baseline_of(&a), shared, "a still names the old baseline");
    assert_eq!(baseline_of(&b), shared, "b was repinned by a's run");
    assert!(
        holders(&state).contains(&shared),
        "a's repin took down the baseline b is still pinned to"
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
    up(&wt, &state);

    let root = tmp.path().join("proj_worktrees").join("_baselines");
    let baseline = slots(&root).pop().expect("a baseline");
    up(&baseline, &state);

    assert_eq!(slots(&root), vec![baseline.clone()], "a baseline nested");
    assert!(
        !baseline.join(".devkit").join("issue.toml").exists(),
        "an issue record was written into a baseline"
    );
}

/// A baseline dry run reports the directory it would use and builds none: no
/// worktree, no `setup` commands, no `after_worktree_create` hooks, and no pin.
#[test]
fn a_baseline_dry_run_builds_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let repo = tmp.path().join("proj");
    project(&repo);

    let wt = tmp.path().join("proj_worktrees").join("a");
    git(&repo, &["worktree", "add", "-b", "a", wt.to_str().unwrap()]);
    let out = devkit_ok(
        &wt,
        &state,
        &["run", "up", "--role", "baseline", "--dry-run", "api"],
    );

    let root = tmp.path().join("proj_worktrees").join("_baselines");
    assert!(!root.exists(), "a dry run created {}", root.display());
    assert!(
        !wt.join(".devkit").join("issue.toml").exists(),
        "a dry run pinned the worktree"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("_baselines"),
        "the plan names no baseline directory: {stdout}"
    );
}

/// A dry run after a rebase does not repin, so it stops nothing: the rows under
/// the baseline this worktree is still pinned to survive it. Killing a live
/// server is the one thing a dry run must never do.
#[test]
fn a_baseline_dry_run_stops_no_servers() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let repo = tmp.path().join("proj");
    project(&repo);

    let wt = tmp.path().join("proj_worktrees").join("a");
    git(&repo, &["worktree", "add", "-b", "a", wt.to_str().unwrap()]);
    up(&wt, &state);
    let first = baseline_of(&wt);
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

    assert_eq!(baseline_of(&wt), first, "a dry run moved the pin");
    assert!(
        holders(&state).contains(&first),
        "a dry run released the rows under the baseline still pinned"
    );
}
