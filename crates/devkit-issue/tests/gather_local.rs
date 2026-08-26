use devkit_common::tracker::fake::FakeTracker;
use devkit_common::tracker::{Resolved, TrackerKind};
use std::path::Path;

fn git(args: &[&str], cwd: &Path) {
    devkit_common::git::Git::fixture(cwd)
        .args(args.iter().copied())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
}

/// A repo with one commit and one `lev/eng-1-bar` worktree beside it, in a
/// fresh directory so tests in this binary never share a fixture.
fn fixture_repo() -> tempfile::TempDir {
    let base = tempfile::tempdir().unwrap();
    let main = base.path().join("main");
    std::fs::create_dir_all(&main).unwrap();

    git(&["init", "-q", "-b", "main"], &main);
    std::fs::write(main.join("f"), "x").unwrap();
    git(&["add", "."], &main);
    git(&["commit", "-qm", "init"], &main);

    let wt = base.path().join("eng-1-foo");
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
    let base = fixture_repo();
    let main = base.path().join("main");

    let report = devkit_issue::status::gather_local(main.to_str().unwrap(), &[]).unwrap();
    let row = report
        .worktrees
        .iter()
        .find(|r| r.issue_id == "ENG-1")
        .expect("eng-1 row present");
    assert_eq!(row.pr.state_label(), "NO_PR");
    assert_eq!(row.pr.number(), None);
    assert!(row.state.is_none());
    assert!(!row.dirty);
}

#[test]
fn gather_with_builds_tracker_info_from_the_injected_tracker() {
    // Ready plus a `None` kind is a pairing `tracker::resolve` cannot produce —
    // `NoneTracker::ready()` is always false — so finding it in the report
    // proves the injected tracker answered, whatever the environment holds. No
    // worktree matches the filter, so nothing is fetched and no link base is
    // resolved.
    let base = fixture_repo();
    let mut t = FakeTracker::with_states([]);
    t.kind = TrackerKind::None;
    let injected = Resolved {
        tracker: Box::new(t),
        declared: true,
        reason: "chosen by this test".into(),
    };
    let repos = devkit_common::github::Repos::from_parts(
        &devkit_config::GithubConfig::default(),
        None,
        None,
    );
    let report = devkit_issue::status::gather_with(
        base.path().join("main").to_str().unwrap(),
        &["NOPE-1".into()],
        &injected,
        &repos,
    )
    .unwrap();
    assert!(report.worktrees.is_empty());
    assert_eq!(report.tracker.kind, TrackerKind::None);
    assert!(report.tracker.ready);
    assert_eq!(report.tracker.link_base, None);
}

/// An issue id is a case-insensitive identifier, and the record stores whatever
/// spelling the tracker was given, so a worktree recorded lowercase has to be
/// reachable by either spelling of its id.
#[test]
fn a_lowercase_record_id_is_found_by_either_spelling() {
    let base = fixture_repo();
    let main = base.path().join("main");
    let wt = base.path().join("eng-1-foo");
    std::fs::create_dir_all(wt.join(".devkit")).unwrap();
    std::fs::write(
        wt.join(".devkit").join("issue.toml"),
        "issue = \"eng-1234\"\nslug = \"fix\"\napps = []\n",
    )
    .unwrap();

    for spelling in ["eng-1234", "ENG-1234", "Eng-1234"] {
        let report =
            devkit_issue::status::gather_local(main.to_str().unwrap(), &[spelling.to_string()])
                .unwrap();
        let ids: Vec<&str> = report
            .worktrees
            .iter()
            .map(|r| r.issue_id.as_str())
            .collect();
        assert_eq!(ids, ["eng-1234"], "filtering by {spelling}");
    }
}
