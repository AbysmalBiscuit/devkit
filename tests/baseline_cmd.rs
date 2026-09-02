//! `devrun baseline list` and `devrun baseline prune` over the real binary: the
//! operator's view of the baseline directory, what a sweep reclaims, and what it
//! refuses to touch.

#[path = "common/baselinetest.rs"]
mod baselinetest;

use baselinetest::{devkit_ok, git, project, up};
use std::path::{Path, PathBuf};

struct Fx {
    _tmp: tempfile::TempDir,
    state: PathBuf,
    repo: PathBuf,
    wt: PathBuf,
    baselines: PathBuf,
    baseline: PathBuf,
    stray: PathBuf,
}

/// One worktree that has pinned a baseline, plus a hand-made directory under
/// `baseline_dir` that devkit did not create and cannot claim.
fn fixture() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let repo = tmp.path().join("proj");
    project(&repo);

    let wt = tmp.path().join("proj_worktrees").join("a");
    git(&repo, &["worktree", "add", "-b", "a", wt.to_str().unwrap()]);
    up(&wt, &state);

    let baselines = tmp.path().join("proj_worktrees").join("_baselines");
    let baseline = std::fs::read_dir(&baselines)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.join(".devkit").join("baseline.toml").exists())
        .expect("up created a baseline");
    let stray = baselines.join("notabaseline");
    std::fs::create_dir_all(&stray).unwrap();

    Fx {
        _tmp: tmp,
        state,
        repo,
        wt,
        baselines,
        baseline,
        stray,
    }
}

/// The slot's directory name, which is what `list` puts in its own column and
/// what every path a sweep prints ends in.
fn name_of(baseline: &Path) -> String {
    baseline.file_name().unwrap().to_string_lossy().into_owned()
}

/// Give `baseline` a port row with a live pid: this test process, which is alive
/// by definition. A reservation `up` wrote carries no pid, and a row without one
/// is not a running server.
fn seed_live_row(state: &Path, baseline: &Path) {
    devkit_ok(
        baseline,
        state,
        &[
            "ports",
            "alloc",
            "--holder",
            baseline.to_str().unwrap(),
            "--role",
            "baseline",
            "api",
        ],
    );
    let path = state.join("devkit").join("ports.json");
    let body = std::fs::read_to_string(&path).unwrap();
    let mut doc: serde_json::Value = serde_json::from_str(&body).unwrap();
    let holder = baseline.to_string_lossy();
    let mut seeded = false;
    for entry in doc["entries"].as_object_mut().unwrap().values_mut() {
        if entry["holder"].as_str() == Some(&*holder) {
            entry["pid"] = serde_json::json!(std::process::id());
            seeded = true;
        }
    }
    assert!(seeded, "no port row for {}: {body}", baseline.display());
    std::fs::write(&path, serde_json::to_string(&doc).unwrap()).unwrap();
}

/// An unreferenced baseline goes; a directory with no marker is reported, not
/// deleted, because devkit cannot prove it created it.
#[test]
fn prune_removes_an_unreferenced_baseline_and_reports_a_markerless_directory() {
    let f = fixture();
    git(
        &f.repo,
        &["worktree", "remove", "--force", f.wt.to_str().unwrap()],
    );

    let out = devkit_ok(&f.repo, &f.state, &["run", "baseline", "prune"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !f.baseline.exists(),
        "unreferenced baseline survived:\n{stdout}"
    );
    assert!(
        f.stray.exists(),
        "a markerless directory must never be deleted"
    );
    assert!(
        stdout.contains("notabaseline"),
        "prune must name what it left alone:\n{stdout}"
    );
}

/// A worktree still pinned to a baseline is what keeps it alive.
#[test]
fn prune_leaves_a_referenced_baseline_alone() {
    let f = fixture();

    let out = devkit_ok(&f.repo, &f.state, &["run", "baseline", "prune"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        f.baseline.exists(),
        "prune took the baseline {} still names:\n{stdout}",
        f.wt.display()
    );
}

#[test]
fn dry_run_removes_nothing_and_still_reports() {
    let f = fixture();
    git(
        &f.repo,
        &["worktree", "remove", "--force", f.wt.to_str().unwrap()],
    );

    let out = devkit_ok(
        &f.repo,
        &f.state,
        &["run", "baseline", "prune", "--dry-run"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(f.baseline.exists(), "dry run deleted a baseline:\n{stdout}");
    assert!(f.stray.exists());
    assert!(
        stdout.contains(&name_of(&f.baseline)),
        "dry run must name what it would remove:\n{stdout}"
    );
}

/// A baseline with a running server is refused by a real sweep, so a dry run
/// that named it would be promising a removal the operator cannot get.
#[test]
fn a_dry_run_promises_no_removal_a_real_prune_refuses() {
    let f = fixture();
    seed_live_row(&f.state, &f.baseline);
    git(
        &f.repo,
        &["worktree", "remove", "--force", f.wt.to_str().unwrap()],
    );

    let dry = devkit_ok(
        &f.repo,
        &f.state,
        &["run", "baseline", "prune", "--dry-run"],
    );
    let stdout = String::from_utf8_lossy(&dry.stdout);
    assert!(
        !stdout.contains(&name_of(&f.baseline)),
        "dry run promised to remove a baseline with a running server:\n{stdout}"
    );

    let real = devkit_ok(&f.repo, &f.state, &["run", "baseline", "prune"]);
    assert!(
        f.baseline.exists(),
        "a baseline with a running server was removed:\n{}",
        String::from_utf8_lossy(&real.stdout)
    );
}

/// `list` enumerates with `read_dir`, so a tree git has no registration for is
/// visible. `git worktree list` would not show it at all, which is exactly the
/// state an operator needs to see.
#[test]
fn list_shows_a_baseline_git_no_longer_knows_about() {
    let f = fixture();
    let orphan = f.baselines.join("000000000000");
    std::fs::create_dir_all(orphan.join(".devkit")).unwrap();
    std::fs::write(
        orphan.join(".devkit").join("baseline.toml"),
        "sha = 'abc'\n",
    )
    .unwrap();

    let out = devkit_ok(&f.repo, &f.state, &["run", "baseline", "list"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("000000000000"),
        "unregistered baseline not listed:\n{stdout}"
    );
}

/// A project with no baseline directory yet has nothing to list and nothing to
/// sweep, which is an empty answer rather than an error.
#[test]
fn list_and_prune_are_quiet_before_any_baseline_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let repo = tmp.path().join("proj");
    project(&repo);

    devkit_ok(&repo, &state, &["run", "baseline", "list"]);
    devkit_ok(&repo, &state, &["run", "baseline", "prune"]);
    assert!(
        !tmp.path()
            .join("proj_worktrees")
            .join("_baselines")
            .exists(),
        "a read-only pass created the baseline directory"
    );
}
