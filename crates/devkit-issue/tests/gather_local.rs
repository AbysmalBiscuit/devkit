use devkit_common::tracker::TrackerKind;
use devkit_common::tracker::fake::FakeTracker;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(args: &[&str], cwd: &Path) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("git runs")
        .success();
    assert!(ok, "git {args:?} failed");
}

/// A repo with one commit and one `lev/eng-1-bar` worktree beside it, under a
/// `tag`-specific directory so tests in this binary never share a fixture.
fn fixture_repo(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("devkit-gl-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let main = base.join("main");
    std::fs::create_dir_all(&main).unwrap();

    git(&["init", "-q", "-b", "main"], &main);
    git(&["config", "user.email", "t@t"], &main);
    git(&["config", "user.name", "t"], &main);
    std::fs::write(main.join("f"), "x").unwrap();
    git(&["add", "."], &main);
    git(&["commit", "-qm", "init"], &main);

    let wt = base.join("eng-1-foo");
    git(
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "lev/eng-1-bar",
            wt.to_str().unwrap(),
        ],
        &main,
    );
    base
}

#[test]
fn gather_local_returns_offline_rows_without_network() {
    let base = fixture_repo("local");
    let main = base.join("main");

    let report = devkit_issue::status::gather_local(main.to_str().unwrap(), &[]).unwrap();
    let row = report
        .worktrees
        .iter()
        .find(|r| r.issue_id == "ENG-1")
        .expect("eng-1 row present");
    assert_eq!(row.pr_state, "NO_PR");
    assert_eq!(row.pr_number, None);
    assert!(row.state.is_none());
    assert!(!row.dirty);

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn gather_with_builds_tracker_info_from_the_injected_tracker() {
    // Ready plus a `None` kind is a pairing `tracker::resolve` cannot produce —
    // `NoneTracker::ready()` is always false — so finding it in the report
    // proves the injected tracker answered, whatever the environment holds. No
    // worktree matches the filter, so nothing is fetched and no link base is
    // resolved.
    let base = fixture_repo("gw");
    let mut t = FakeTracker::with_states([]);
    t.kind = TrackerKind::None;
    let report = devkit_issue::status::gather_with(
        base.join("main").to_str().unwrap(),
        &["NOPE-1".into()],
        &t,
    )
    .unwrap();
    assert!(report.worktrees.is_empty());
    assert_eq!(report.tracker.kind, TrackerKind::None);
    assert!(report.tracker.ready);
    assert_eq!(report.tracker.link_base, None);

    let _ = std::fs::remove_dir_all(&base);
}
