use std::path::Path;

use devkit_common::{
    tracker::{Resolved, TrackerKind, fake::FakeTracker},
    worktree::IssueId,
};

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
        .find(|r| r.issue_id == IssueId::Tracker("ENG-1".into()))
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
        let ids: Vec<&IssueId> = report.worktrees.iter().map(|r| &r.issue_id).collect();
        assert_eq!(
            ids,
            [&IssueId::Tracker("eng-1234".into())],
            "filtering by {spelling}"
        );
    }
}

/// `issue setup --slug` with no issue records an empty id. The branch then
/// carries only the slug, and a slug like `utf-8-fix` looks like an issue id,
/// so the record has to settle it rather than the branch scan.
#[test]
fn an_issueless_record_reads_as_none_not_a_branch_scan() {
    let base = fixture_repo();
    let main = base.path().join("main");
    let wt = base.path().join("utf-8-fix");
    git(
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "lev/utf-8-fix",
            wt.to_str().unwrap(),
        ],
        &main,
    );
    std::fs::create_dir_all(wt.join(".devkit")).unwrap();
    std::fs::write(
        wt.join(".devkit").join("issue.toml"),
        "issue = \"\"\nslug = \"utf-8-fix\"\napps = []\n",
    )
    .unwrap();

    let found = devkit_issue::status::discover(main.to_str().unwrap(), &[]).unwrap();
    let row = found
        .rows()
        .iter()
        .find(|r| r.branch == "lev/utf-8-fix")
        .expect("issueless row present");
    assert_eq!(row.issue_id, IssueId::NoIssue);
    assert!(
        !found.issue_ids().iter().any(|id| id == "NONE"),
        "no tracker lookup for an issueless worktree: {:?}",
        found.issue_ids()
    );
}
