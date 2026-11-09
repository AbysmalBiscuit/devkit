use std::path::Path;

use devkit_common::{
    tracker::{Resolved, TrackerKind, fake::FakeTracker},
    worktree::IssueId,
};

fn git(args: &[&str], cwd: &Path) {
    devkit_git::Git::fixture(cwd)
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

    let report = devkit_ticket::status::gather_local(main.to_str().unwrap(), &[]).unwrap();
    let row = report
        .worktrees
        .iter()
        .find(|r| r.issue_id == IssueId::Tracker("ENG-1".into()))
        .expect("eng-1 row present");
    assert_eq!(row.pr.state_label(), "NO_PR");
    assert_eq!(row.pr.number(), None);
    assert!(row.state.is_none());
    assert_eq!(row.tree, devkit_ticket::status::Tree::Clean);
}

#[test]
fn gather_with_builds_tracker_info_from_the_injected_tracker() {
    // `tracker::resolve` never pairs ready with a `None` kind, so that pair and
    // the fake's link base show the injected tracker answered.
    let base = fixture_repo();
    let mut t = FakeTracker::with_states([]);
    t.kind = TrackerKind::None;
    let injected = Resolved {
        tracker: Box::new(t),
        declared: true,
        reason: "chosen by this test".into(),
    };
    let main = base.path().join("main");
    let forge = devkit_common::forge::resolve(
        &devkit_config::ForgeConfig::default(),
        &devkit_config::GithubConfig::default(),
        main.to_str().unwrap(),
        None,
    );
    let report = devkit_ticket::status::gather_with(
        main.to_str().unwrap(),
        &["NOPE-1".into()],
        &injected,
        &forge,
        false,
    )
    .unwrap();
    assert!(report.worktrees.is_empty());
    assert_eq!(report.tracker.kind, TrackerKind::None);
    assert!(report.tracker.ready);
    assert_eq!(
        report.tracker.link_base.as_deref(),
        Some("https://example.test/issue/")
    );
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
        wt.join(".devkit").join("ticket.toml"),
        "issue = \"eng-1234\"\nslug = \"fix\"\napps = []\n",
    )
    .unwrap();

    for spelling in ["eng-1234", "ENG-1234", "Eng-1234"] {
        let report =
            devkit_ticket::status::gather_local(main.to_str().unwrap(), &[spelling.to_string()])
                .unwrap();
        let ids: Vec<&IssueId> = report.worktrees.iter().map(|r| &r.issue_id).collect();
        assert_eq!(
            ids,
            [&IssueId::Tracker("eng-1234".into())],
            "filtering by {spelling}"
        );
    }
}

/// `workspace setup --slug` with no issue records an empty id. The branch then
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
        wt.join(".devkit").join("ticket.toml"),
        "issue = \"\"\nslug = \"utf-8-fix\"\napps = []\n",
    )
    .unwrap();

    let found = devkit_ticket::status::discover(main.to_str().unwrap(), &[]).unwrap();
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

/// A PR number is known only after the PR fetch, so selecting by it has to
/// wait for that fetch rather than filter in discovery.
#[test]
fn gather_with_selects_a_worktree_by_its_pr_number() {
    for selector in ["7", "#7"] {
        assert_eq!(
            branches_selected_by(&[7], selector),
            ["lev/eng-1-bar"],
            "selecting by {selector}"
        );
    }
}

/// Two PRs from one head branch leave the row ambiguous, and each candidate
/// still names it.
#[test]
fn gather_with_selects_a_worktree_by_an_ambiguous_pr_candidate() {
    assert_eq!(branches_selected_by(&[7, 8], "#8"), ["lev/eng-1-bar"]);
}

/// The branches of the rows `gather_with` keeps for `selector`, with one more
/// worktree beside `lev/eng-1-bar` and a PR from that branch for each number.
fn branches_selected_by(numbers: &[u64], selector: &str) -> Vec<String> {
    use devkit_common::forge;

    let base = fixture_repo();
    let main = base.path().join("main");
    let other = base.path().join("eng-2-baz");
    git(
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "lev/eng-2-baz",
            other.to_str().unwrap(),
        ],
        &main,
    );
    let fake = numbers
        .iter()
        .fold(forge::fake::FakeForge::new(), |f, &number| {
            f.with_pr(forge::PrBrief {
                number,
                state: "OPEN".into(),
                url: format!("https://github.com/o/r/pull/{number}"),
                title: String::new(),
                head_ref_name: "lev/eng-1-bar".into(),
                head_ref_oid: String::new(),
                head_repo_owner: None,
                is_draft: false,
                author_login: None,
            })
        });
    let forge = forge::Resolved {
        forge: Box::new(fake),
        declared: true,
        reason: "chosen by this test".into(),
        repos: forge::Repos::from_parts(
            &Default::default(),
            &Default::default(),
            Some("o/r".into()),
            None,
        ),
    };
    let tracker = Resolved {
        tracker: Box::new(FakeTracker::with_states([])),
        declared: true,
        reason: "chosen by this test".into(),
    };

    let report = devkit_ticket::status::gather_with(
        main.to_str().unwrap(),
        &[selector.to_string()],
        &tracker,
        &forge,
        false,
    )
    .unwrap();
    report.worktrees.into_iter().map(|r| r.branch).collect()
}
