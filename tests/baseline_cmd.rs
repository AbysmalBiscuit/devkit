//! `devrun baseline list` and `devrun baseline prune` over the real binary: the
//! operator's view of the baseline directory, what a sweep reclaims, and what it
//! refuses to touch.

#[path = "common/baselinetest.rs"]
mod baselinetest;

use baselinetest::{devkit, devkit_ok, git, project, up};
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
/// that named it would be promising a removal the operator cannot get. A
/// refusal is also what the exit status is for: a sweep that did not do what it
/// was asked must not report success to the script that asked.
#[test]
fn a_dry_run_promises_no_removal_a_real_prune_refuses() {
    let f = fixture();
    seed_live_row(&f.state, &f.baseline);
    git(
        &f.repo,
        &["worktree", "remove", "--force", f.wt.to_str().unwrap()],
    );

    let dry = devkit(
        &f.repo,
        &f.state,
        &["run", "baseline", "prune", "--dry-run"],
    );
    let stdout = String::from_utf8_lossy(&dry.stdout);
    assert!(
        !stdout.contains(&name_of(&f.baseline)),
        "dry run promised to remove a baseline with a running server:\n{stdout}"
    );
    assert!(!dry.status.success(), "a refused dry run exited 0");

    let real = devkit(&f.repo, &f.state, &["run", "baseline", "prune"]);
    assert!(
        f.baseline.exists(),
        "a baseline with a running server was removed:\n{}",
        String::from_utf8_lossy(&real.stdout)
    );
    assert!(!real.status.success(), "a refused sweep exited 0");
}

/// A marked tree git has no registration for cannot go through `git worktree
/// remove`, so the sweep reclaims the directory itself — and the dry run says
/// so, because a dry run that named a removal the real run then failed at would
/// leave the operator with a tree nothing ever reclaims.
#[test]
fn an_orphaned_baseline_is_reclaimed_and_the_dry_run_agrees() {
    let f = fixture();
    git(
        &f.repo,
        &["worktree", "remove", "--force", f.wt.to_str().unwrap()],
    );
    let orphan = f.baselines.join("000000000000");
    std::fs::create_dir_all(orphan.join(".devkit")).unwrap();
    std::fs::write(
        orphan.join(".devkit").join("baseline.toml"),
        "sha = 'abc'\n",
    )
    .unwrap();

    let dry = devkit_ok(
        &f.repo,
        &f.state,
        &["run", "baseline", "prune", "--dry-run"],
    );
    let stdout = String::from_utf8_lossy(&dry.stdout);
    assert!(
        stdout.contains("000000000000"),
        "the dry run did not name the orphaned tree:\n{stdout}"
    );
    assert!(orphan.exists(), "a dry run removed it");

    let real = devkit_ok(&f.repo, &f.state, &["run", "baseline", "prune"]);
    assert!(
        !orphan.exists(),
        "the orphaned tree the dry run promised survived:\n{}",
        String::from_utf8_lossy(&real.stdout)
    );
}

/// Removing the directory the operator is standing in leaves their shell in a
/// path that no longer resolves. `issue end` refuses the same way for a
/// worktree.
#[test]
fn prune_refuses_the_baseline_it_is_standing_in() {
    let f = fixture();
    git(
        &f.repo,
        &["worktree", "remove", "--force", f.wt.to_str().unwrap()],
    );

    let out = devkit(&f.baseline, &f.state, &["run", "baseline", "prune"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(f.baseline.exists(), "prune removed its own cwd:\n{stderr}");
    assert!(!out.status.success(), "the refusal exited 0");
    assert!(
        stderr.contains("cd out of"),
        "unexpected refusal:\n{stderr}"
    );
}

/// A modified tracked file is somebody's edit, and it is the only thing in a
/// baseline a rebuild would not bring back. Untracked files are not that
/// signal — every baseline carries rendered prep files and its own marker, and
/// the sweep tests around this one prove those do not block a removal.
///
/// The two waivers stay apart: an operator whose baseline carries an edit must
/// not have to switch off the running-servers gate to get past it.
#[test]
fn prune_refuses_a_baseline_somebody_edited_until_the_edits_are_discarded() {
    let f = fixture();
    git(
        &f.repo,
        &["worktree", "remove", "--force", f.wt.to_str().unwrap()],
    );
    std::fs::write(f.baseline.join("devkit.toml"), "# edited by hand\n").unwrap();

    let out = devkit(&f.repo, &f.state, &["run", "baseline", "prune"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(f.baseline.exists(), "an edited baseline was removed");
    assert!(!out.status.success(), "the refusal exited 0");
    assert!(
        stderr.contains("modified tracked files"),
        "unexpected refusal:\n{stderr}"
    );

    let forced = devkit(&f.repo, &f.state, &["run", "baseline", "prune", "--force"]);
    assert!(
        f.baseline.exists(),
        "--force waived an edit it has no business waiving"
    );
    assert!(!forced.status.success(), "the refusal exited 0");

    devkit_ok(
        &f.repo,
        &f.state,
        &["run", "baseline", "prune", "--discard-edits"],
    );
    assert!(
        !f.baseline.exists(),
        "--discard-edits did not waive the edit"
    );
}

/// The one path in devkit that reaches `remove_dir_all`. A marked directory
/// under `_baselines` that git has no registration for is reclaimed — unless
/// its `.git` resolves, which makes it some other repository's checkout. The
/// unit tests prove the classifier; this proves the sweep an operator runs is
/// wired to it.
#[test]
fn prune_leaves_another_repositorys_tree_alone() {
    let f = fixture();
    git(
        &f.repo,
        &["worktree", "remove", "--force", f.wt.to_str().unwrap()],
    );
    // Marked, so it is a candidate; unregistered here, so it takes the orphan
    // path; standing on a git directory that resolves, so it is somebody's.
    let stranger = f.baselines.join("aaaaaaaaaaaa");
    std::fs::create_dir_all(stranger.join(".devkit")).unwrap();
    std::fs::write(
        stranger.join(".devkit").join("baseline.toml"),
        "sha = 'abc'\n",
    )
    .unwrap();
    std::fs::write(stranger.join("uncommitted.txt"), "somebody's work\n").unwrap();
    std::fs::write(
        stranger.join(".git"),
        format!("gitdir: {}\n", f.repo.join(".git").display()),
    )
    .unwrap();

    let dry = devkit(
        &f.repo,
        &f.state,
        &["run", "baseline", "prune", "--dry-run"],
    );
    let stdout = String::from_utf8_lossy(&dry.stdout);
    assert!(
        !stdout.contains("aaaaaaaaaaaa"),
        "the dry run promised somebody else's tree:\n{stdout}"
    );

    let out = devkit(&f.repo, &f.state, &["run", "baseline", "prune", "--force"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stranger.join("uncommitted.txt").exists(),
        "another repository's tree was deleted:\n{stderr}"
    );
    assert!(!out.status.success(), "the refusal exited 0");
    assert!(
        stderr.contains("repository that owns it"),
        "unexpected refusal:\n{stderr}"
    );
}

/// Git refuses to remove a locked worktree, `--force` included, so the sweep
/// refuses it first. A dry run that promised it would be describing a removal
/// nobody can perform.
#[test]
fn prune_refuses_a_locked_baseline_in_both_modes() {
    let f = fixture();
    git(
        &f.repo,
        &["worktree", "remove", "--force", f.wt.to_str().unwrap()],
    );
    git(&f.repo, &["worktree", "lock", f.baseline.to_str().unwrap()]);

    let dry = devkit(
        &f.repo,
        &f.state,
        &["run", "baseline", "prune", "--dry-run"],
    );
    let stdout = String::from_utf8_lossy(&dry.stdout);
    assert!(
        !stdout.contains(&name_of(&f.baseline)),
        "the dry run promised a locked worktree:\n{stdout}"
    );

    let out = devkit(&f.repo, &f.state, &["run", "baseline", "prune", "--force"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(f.baseline.exists(), "a locked baseline was removed");
    assert!(!out.status.success(), "the refusal exited 0");
    assert!(
        stderr.contains("git worktree unlock"),
        "unexpected refusal:\n{stderr}"
    );
}

/// One unreadable record means no baseline can be proven unreferenced, so the
/// sweep reclaims nothing. A table that showed every baseline with no
/// referencers would read as "all free to prune" and contradict it.
#[test]
fn list_names_the_worktrees_it_could_not_read() {
    let f = fixture();
    std::fs::write(f.wt.join(".devkit").join("issue.toml"), "not = toml = [").unwrap();

    let listed = devkit_ok(&f.repo, &f.state, &["run", "baseline", "list"]);
    let stderr = String::from_utf8_lossy(&listed.stderr);
    assert!(
        stderr.contains("cannot be read") && stderr.contains("proj_worktrees"),
        "the listing hid the unreadable worktree:\n{stderr}"
    );

    devkit_ok(&f.repo, &f.state, &["run", "baseline", "prune"]);
    assert!(
        f.baseline.exists(),
        "a sweep reclaimed a baseline while a record could not be read"
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
