//! `issue end` and the baseline the worktree it removes was pinned to. The
//! removal takes the record with it, and with the record goes `devrun down`'s
//! sole-referencer exemption — `devrun reap` is terminal-gated with no bypass —
//! so servers left behind here are unreachable afterwards.

#[path = "common/baselinetest.rs"]
mod baselinetest;

use baselinetest::{baseline_of, devkit, devkit_ok, git, holders, project, up};

/// The rows under the baseline come down while this worktree is still the
/// referencer that entitles the run to stop them.
#[test]
fn ending_a_worktree_stops_its_baselines_servers() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let repo = tmp.path().join("proj");
    project(&repo);

    // The triage `issue end` runs first reads the `origin` remote; the URL
    // itself is never fetched here.
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/proj.git",
        ],
    );

    let wt = tmp.path().join("proj_worktrees").join("a");
    git(&repo, &["worktree", "add", "-b", "a", wt.to_str().unwrap()]);
    up(&wt, &state);
    let baseline = baseline_of(&wt);

    // A pid-less reservation is what `ports alloc` writes before anything
    // binds, so the teardown releases the row instead of signalling a process —
    // no child to spawn, and nothing to poll for.
    devkit_ok(
        &wt,
        &state,
        &[
            "ports", "alloc", "--holder", &baseline, "--role", "baseline", "api",
        ],
    );
    assert!(
        holders(&state).contains(&baseline),
        "reservation not seeded"
    );

    let out = devkit(
        &repo,
        &state,
        &[
            "issue",
            "end",
            wt.to_str().unwrap(),
            "--yes",
            "--clean-worktree",
            "--no-preserve",
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!wt.exists(), "the worktree survived `issue end`: {stderr}");
    assert!(
        !holders(&state).contains(&baseline),
        "rows under the ended worktree's baseline survived: {stderr}"
    );
}
