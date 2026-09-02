//! `devrun up --role baseline` over the real binary: one baseline per fork
//! point, a repin when the fork point moves, a baseline that is left alone when
//! `up` runs from inside one, and a `--dry-run` that reports a baseline without
//! building, repinning or stopping anything.

#[path = "common/baselinetest.rs"]
mod baselinetest;

use baselinetest::{baseline_of, devkit_ok, git, holders, project, slots, up};

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
