use anyhow::{Context, Result};
use devkit_common::cmd::{gh_json, git};
use devkit_common::github;
use devkit_common::tracker::{State, StateKind, Tracker, TrackerKind};
use devkit_common::worktree;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
struct Pr {
    number: u64,
    state: String, // MERGED | OPEN | CLOSED
    url: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
}

fn state_rank(s: &str) -> u8 {
    match s {
        "MERGED" => 3,
        "OPEN" => 2,
        "CLOSED" => 1,
        _ => 0,
    }
}

/// Best PR for a head branch: prefer MERGED > OPEN > CLOSED, then higher number.
fn best_pr<'a>(prs: &'a [Pr], head: &str) -> Option<&'a Pr> {
    prs.iter()
        .filter(|p| p.head_ref_name == head)
        .max_by_key(|p| (state_rank(&p.state), p.number))
}

/// One issue worktree with its PR + tracker state and the finished verdict.
#[derive(Debug, Clone, Serialize)]
pub struct IssueWorktree {
    pub worktree: String,
    pub branch: String,
    pub issue_id: String,
    pub dirty: bool,
    pub pr_number: Option<u64>,
    pub pr_state: String, // MERGED|OPEN|CLOSED|NO_PR
    pub pr_url: Option<String>,
    /// The tracker's state for this issue, absent when the tracker has no row
    /// for it or there is no tracker.
    pub state: Option<State>,
    pub finished: bool,
    pub reason_not_finished: Option<String>,
}

/// Which tracker produced this report and whether it could answer.
#[derive(Debug, Clone, Serialize)]
pub struct TrackerInfo {
    pub kind: TrackerKind,
    /// Configured and able to authenticate. False means the state column is
    /// blank because there is nothing to ask, not because the issue is unknown.
    pub ready: bool,
    /// The tracker's issue URL built with an empty id, so
    /// `format!("{link_base}{id}")` is that issue's URL.
    pub link_base: Option<String>,
}

/// The full status snapshot for a set of worktrees.
#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    pub worktrees: Vec<IssueWorktree>,
    pub finished_count: usize,
    pub tracker: TrackerInfo,
}

/// Local-only discovery: worktrees + dirty placeholders + issue ids + the main
/// repo path. The slow network fetches consume this. Fast — no `gh`/tracker.
pub struct Discovered {
    rows: Vec<IssueWorktree>,
    main_path: String,
    issue_ids: Vec<String>,
}

impl Discovered {
    /// Assemble a `Discovered` from pre-built rows, bypassing filesystem
    /// discovery. A seam for tests of callers that re-orchestrate the gather
    /// (e.g. the CLI's live table); real callers use [`discover`].
    #[doc(hidden)]
    pub fn from_parts(
        rows: Vec<IssueWorktree>,
        main_path: String,
        issue_ids: Vec<String>,
    ) -> Discovered {
        Discovered {
            rows,
            main_path,
            issue_ids,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn worktree_paths(&self) -> Vec<String> {
        self.rows.iter().map(|r| r.worktree.clone()).collect()
    }
    pub fn issue_ids(&self) -> &[String] {
        &self.issue_ids
    }
    /// The discovered worktree rows, with dirty/PR/state still unfilled. Lets a
    /// single-worktree caller (`issue info`) pick its target without paying the
    /// per-worktree enrichment cost of a full gather.
    pub fn rows(&self) -> &[IssueWorktree] {
        &self.rows
    }
}

/// An opaque GitHub PR list for a set of worktrees.
pub struct Prs(Vec<Pr>);

impl Prs {
    /// An empty PR list, built without any network call. Used when there are
    /// no worktrees and by tests of code that consumes a `Prs`.
    pub fn empty() -> Prs {
        Prs(Vec::new())
    }

    /// Overlay the best PR for `row`'s branch onto it, leaving the row untouched
    /// when the branch is detached or has no PR. Same rule `assemble` applies
    /// per row, exposed so a single-worktree caller can enrich one row.
    pub fn apply_best(&self, row: &mut IssueWorktree) {
        if row.branch == "DETACHED" {
            return;
        }
        if let Some(p) = best_pr(&self.0, &row.branch) {
            row.pr_number = Some(p.number);
            row.pr_state = p.state.clone();
            row.pr_url = Some(p.url.clone());
        }
    }
}

/// Discover worktrees and their issue ids, filtered to `ids` when non-empty.
/// Rows carry `dirty = false` placeholders; the dirty check is a separate step
/// so callers can drive it with a progress bar.
pub fn discover(start: &str, ids: &[String]) -> Result<Discovered> {
    let (main, others) = worktree::discover(start)?;
    let main_path = main
        .to_str()
        .context("main repo path not UTF-8")?
        .to_string();
    let mut rows = Vec::new();
    for wt in &others {
        let iid = worktree::issue_id_of(&wt.path, &wt.branch);
        // An issue id is case-insensitive in every tracker that has one, and the
        // record holds whichever spelling the tracker was given.
        if !ids.is_empty() && !ids.iter().any(|w| w.eq_ignore_ascii_case(&iid)) {
            continue;
        }
        rows.push(IssueWorktree {
            worktree: wt.path.to_string_lossy().into_owned(),
            branch: wt.branch.clone(),
            issue_id: iid,
            dirty: false,
            pr_number: None,
            pr_state: "NO_PR".to_string(),
            pr_url: None,
            state: None,
            finished: false,
            reason_not_finished: None,
        });
    }
    let issue_ids = rows
        .iter()
        .filter(|r| r.issue_id != "UNKNOWN")
        .map(|r| r.issue_id.clone())
        .collect();
    Ok(Discovered {
        rows,
        main_path,
        issue_ids,
    })
}

/// True when a worktree has uncommitted changes.
pub fn dirty_of(path: &str) -> bool {
    !git(&["status", "--porcelain"], path)
        .unwrap_or_default()
        .trim()
        .is_empty()
}

/// `dirty_of` for many worktrees, run on a bounded thread pool with order
/// preserved. Each check is an independent, I/O-bound `git status` walk, so
/// fanning them across cores turns N serial walks into roughly one walk's
/// latency. The batch form of [`dirty_stream`]: results land in a slot per
/// input index, keeping the output aligned with `paths`.
pub fn dirty_many(paths: &[String]) -> Vec<bool> {
    let out = std::sync::Mutex::new(vec![false; paths.len()]);
    dirty_stream(paths, |i, d| out.lock().unwrap()[i] = d);
    out.into_inner().unwrap()
}

/// `dirty_of` for many worktrees, reporting each result as soon as it is
/// known. `report(i, dirty)` is invoked exactly once per input index, from
/// worker threads on a bounded pool over contiguous chunks; callers that
/// want the batch form should keep using [`dirty_many`].
pub fn dirty_stream(paths: &[String], report: impl Fn(usize, bool) + Send + Clone) {
    if paths.is_empty() {
        return;
    }
    let width = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 16)
        .min(paths.len());
    let chunk = paths.len().div_ceil(width);
    std::thread::scope(|s| {
        for (ci, c) in paths.chunks(chunk).enumerate() {
            let report = report.clone();
            s.spawn(move || {
                for (j, p) in c.iter().enumerate() {
                    report(ci * chunk + j, dirty_of(p));
                }
            });
        }
    });
}

/// The single `gh pr list` round-trip for every worktree PR. Skips the call
/// entirely when there are no worktrees.
pub fn fetch_prs(d: &Discovered) -> Result<Prs> {
    if d.rows.is_empty() {
        return Ok(Prs::empty());
    }
    if let Some(prs) = fetch_prs_http(&d.main_path) {
        return Ok(Prs(prs));
    }
    let prs: Vec<Pr> = gh_json(
        &[
            "pr",
            "list",
            "--state",
            "all",
            "--limit",
            "500",
            "--json",
            "number,state,url,headRefName",
        ],
        &d.main_path,
    )?;
    Ok(Prs(prs))
}

/// The PR list over direct HTTP; `None` on no token / parse / transport failure,
/// so [`fetch_prs`] falls back to `gh`.
fn fetch_prs_http(cwd: &str) -> Option<Vec<Pr>> {
    let slug = github::repo_slug(cwd).ok()?;
    let briefs = github::list_prs(&slug, 500).ok()?;
    Some(
        briefs
            .into_iter()
            .map(|b| Pr {
                number: b.number,
                state: b.state,
                url: b.url,
                head_ref_name: b.head_ref_name,
            })
            .collect(),
    )
}

/// Attach dirty flags (in row order), best PR, tracker state, and the finished
/// verdict. `tracker` is carried through to the report for link building and to
/// tell a blank state column from an unreachable tracker.
pub fn assemble(
    d: Discovered,
    dirty: Vec<bool>,
    prs: Prs,
    states: HashMap<String, State>,
    tracker: TrackerInfo,
) -> StatusReport {
    let mut rows = d.rows;
    let mut finished_count = 0;
    for (i, wt) in rows.iter_mut().enumerate() {
        wt.dirty = dirty.get(i).copied().unwrap_or(false);
        let pr = if wt.branch != "DETACHED" {
            best_pr(&prs.0, &wt.branch)
        } else {
            None
        };
        if let Some(p) = pr {
            wt.pr_number = Some(p.number);
            wt.pr_state = p.state.clone();
            wt.pr_url = Some(p.url.clone());
        }
        if let Some(st) = states.get(&wt.issue_id) {
            wt.state = Some(st.clone());
        }
        let reason = reason_not_finished(wt, &tracker, false);
        wt.finished = reason.is_none();
        if wt.finished {
            finished_count += 1;
        }
        wt.reason_not_finished = reason;
    }
    StatusReport {
        worktrees: rows,
        finished_count,
        tracker,
    }
}

/// How a tracker names itself in user-facing text. `TrackerKind::None` answers
/// with the neutral word: a tracker-less project has no state gate, so nothing
/// that names a state ever reaches it.
pub fn label(kind: TrackerKind) -> &'static str {
    match kind {
        TrackerKind::Linear => "Linear",
        TrackerKind::Github => "GitHub",
        TrackerKind::None => "tracker",
    }
}

/// None when finished; otherwise a short reason it is not.
///
/// The state gate has three shapes. A project whose tracker kind is
/// `TrackerKind::None` has no state to wait for, so its verdict rests on the PR
/// and a clean tree. A tracker that answered gates on the issue having reached a
/// completed state. A tracker that is configured but did not answer holds the
/// gate open, so an unset key or an unreachable API never promotes a worktree to
/// finished.
///
/// With `pr_only` both the state and issue-id gates are dropped (finished = PR
/// merged + clean), so repos whose branches carry no issue id still qualify.
pub fn reason_not_finished(
    wt: &IssueWorktree,
    tracker: &TrackerInfo,
    pr_only: bool,
) -> Option<String> {
    if !pr_only && wt.issue_id == "UNKNOWN" {
        return Some("not an issue worktree".into());
    }
    let mut bits: Vec<String> = Vec::new();
    if wt.pr_state != "MERGED" {
        bits.push(if wt.pr_state != "NO_PR" {
            "PR not merged".into()
        } else {
            "no PR".into()
        });
    }
    // A tracker-less project has no state to wait for; every other kind gates on
    // the issue's state, and says so when it could not read one.
    if !pr_only && tracker.kind != TrackerKind::None {
        match wt.state.as_ref() {
            Some(s) if s.kind != StateKind::Completed => {
                bits.push(format!("{} {}", label(tracker.kind), s.name))
            }
            Some(_) => {}
            None if tracker.ready => bits.push("tracker state unknown".into()),
            None => bits.push("no tracker key".into()),
        }
    }
    if wt.dirty {
        bits.push("dirty".into());
    }
    if bits.is_empty() {
        None
    } else {
        Some(bits.join(", "))
    }
}

/// Discover worktrees, fetch PRs + tracker state concurrently, and compute the
/// finished verdict against a caller-supplied tracker. Silent — no progress
/// output (the CLI re-orchestrates the same pieces with bars). This crate reads
/// no config, so the caller that loaded one resolves the tracker and injects it;
/// tests inject a fake.
pub fn gather_with(start: &str, ids: &[String], t: &dyn Tracker) -> Result<StatusReport> {
    let d = discover(start, ids)?;
    let info = TrackerInfo {
        kind: t.kind(),
        ready: t.ready(),
        link_base: None,
    };
    if d.is_empty() {
        // No worktrees means no ids to look up and no rows to link.
        return Ok(assemble(d, Vec::new(), Prs::empty(), HashMap::new(), info));
    }
    let paths = d.worktree_paths();
    let ids_v: Vec<String> = d.issue_ids().to_vec();
    let (dirty, prs, states, link_base) = std::thread::scope(|s| {
        let dt = s.spawn(|| dirty_many(&paths));
        let pt = s.spawn(|| fetch_prs(&d));
        // The state fetch and the link base share a thread: both go through the
        // tracker, and both can reach the network.
        let tt = s.spawn(|| (t.states(&ids_v), t.issue_url("")));
        let dirty = dt.join().expect("dirty thread panicked");
        let prs = pt.join().expect("prs thread panicked")?;
        let (states, link_base) = tt.join().expect("tracker thread panicked");
        Ok::<_, anyhow::Error>((dirty, prs, states, link_base))
    })?;
    Ok(assemble(
        d,
        dirty,
        prs,
        states,
        TrackerInfo { link_base, ..info },
    ))
}

/// Local-only status: discovery + dirty checks, with no `gh`/tracker network.
/// PRs stay `NO_PR` and the state stays unknown; callers (e.g. `issue info
/// --cache-only`) overlay cached data themselves. Same signature shape as
/// `gather`, and like it the tracker is detected rather than read from config.
pub fn gather_local(start: &str, ids: &[String]) -> Result<StatusReport> {
    let d = discover(start, ids)?;
    let t = devkit_common::tracker::resolve(None, None, Path::new(start));
    let dirty = dirty_many(&d.worktree_paths());
    Ok(assemble(
        d,
        dirty,
        Prs::empty(),
        HashMap::new(),
        TrackerInfo {
            kind: t.kind(),
            ready: t.ready(),
            link_base: None,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use devkit_common::tracker::fake::FakeTracker;
    use devkit_common::tracker::{StateKind, Tracker, TrackerKind};
    use std::collections::HashMap;

    fn done(name: &str) -> State {
        State {
            kind: StateKind::Completed,
            name: name.into(),
            color: None,
        }
    }

    fn tracker(kind: TrackerKind, ready: bool) -> TrackerInfo {
        TrackerInfo {
            kind,
            ready,
            link_base: None,
        }
    }

    /// One discovered row on `branch`, shaped the way `discover` emits it.
    fn discovered(id: &str, branch: &str) -> Discovered {
        let mut row = wt(id, "NO_PR", false, None);
        row.branch = branch.to_string();
        row.pr_number = None;
        Discovered::for_test(vec![row], "/main".into(), vec![id.to_string()])
    }

    #[test]
    fn a_merged_clean_worktree_with_a_completed_issue_is_finished() {
        let t = FakeTracker::with_states([("ENG-1", done("Done"))]);
        let report = assemble(
            discovered("ENG-1", "lev/eng-1-fix"),
            vec![false],
            Prs::for_test(vec![pr(10, "MERGED", "lev/eng-1-fix")]),
            t.states(&["ENG-1".into()]),
            tracker(TrackerKind::Linear, true),
        );
        assert_eq!(
            report.worktrees[0].state.as_ref().map(|s| s.kind),
            Some(StateKind::Completed)
        );
        assert!(report.worktrees[0].finished);
        assert_eq!(report.finished_count, 1);
    }

    #[test]
    fn an_open_issue_is_not_finished_and_says_why() {
        let mut st = done("In Progress");
        st.kind = StateKind::Started;
        let report = assemble(
            discovered("ENG-2", "lev/eng-2-wip"),
            vec![false],
            Prs::for_test(vec![pr(11, "MERGED", "lev/eng-2-wip")]),
            HashMap::from([("ENG-2".to_string(), st)]),
            tracker(TrackerKind::Linear, true),
        );
        assert!(!report.worktrees[0].finished);
        let why = report.worktrees[0].reason_not_finished.as_deref().unwrap();
        assert!(
            why.contains("In Progress"),
            "the reason names the state: {why}"
        );
    }

    #[test]
    fn with_no_tracker_a_merged_clean_worktree_is_finished_without_a_state() {
        let report = assemble(
            discovered("ENG-3", "lev/some-branch"),
            vec![false],
            Prs::for_test(vec![pr(12, "MERGED", "lev/some-branch")]),
            HashMap::new(),
            tracker(TrackerKind::None, false),
        );
        let row = &report.worktrees[0];
        assert!(row.state.is_none());
        assert!(
            row.finished,
            "a project with no tracker still finishes on PR merged + clean"
        );
    }

    #[test]
    fn with_a_tracker_and_no_key_a_merged_clean_worktree_is_not_finished() {
        let report = assemble(
            discovered("ENG-4", "lev/other-branch"),
            vec![false],
            Prs::for_test(vec![pr(13, "MERGED", "lev/other-branch")]),
            HashMap::new(),
            tracker(TrackerKind::Linear, false),
        );
        let row = &report.worktrees[0];
        assert!(!row.finished);
        assert_eq!(
            row.reason_not_finished.as_deref(),
            Some("no tracker key"),
            "a configured tracker that cannot answer holds the gate, so an unset \
             key never promotes a worktree to finished"
        );
    }

    // assemble zips dirty flags onto rows in order, attaches the best PR by
    // branch, applies tracker state, and computes the finished verdict — the
    // same result the old monolithic gather produced.
    #[test]
    fn assemble_attaches_pr_dirty_and_verdict() {
        let d = discovered("ENG-1", "lev/eng-1-foo");
        let prs = Prs::for_test(vec![pr(7, "MERGED", "lev/eng-1-foo")]);
        let states = HashMap::from([("ENG-1".to_string(), done("Done"))]);
        let info = TrackerInfo {
            kind: TrackerKind::Linear,
            ready: true,
            link_base: Some("https://linear.app/acme/issue/".into()),
        };
        let report = assemble(d, vec![false], prs, states, info);
        let row = &report.worktrees[0];
        assert_eq!(row.pr_number, Some(7));
        assert_eq!(row.pr_state, "MERGED");
        assert!(!row.dirty);
        assert!(row.finished);
        assert_eq!(report.finished_count, 1);
        assert_eq!(
            report.tracker.link_base.as_deref(),
            Some("https://linear.app/acme/issue/")
        );
    }

    #[test]
    fn assemble_marks_dirty_from_flags() {
        let report = assemble(
            discovered("ENG-2", "lev/eng-2-bar"),
            vec![true],
            Prs::for_test(vec![]),
            HashMap::new(),
            tracker(TrackerKind::None, false),
        );
        assert!(report.worktrees[0].dirty);
        assert!(!report.worktrees[0].finished);
    }

    impl Discovered {
        fn for_test(rows: Vec<IssueWorktree>, main_path: String, issue_ids: Vec<String>) -> Self {
            Discovered {
                rows,
                main_path,
                issue_ids,
            }
        }
    }
    impl Prs {
        fn for_test(prs: Vec<Pr>) -> Self {
            Prs(prs)
        }
    }

    fn pr(n: u64, state: &str, head: &str) -> Pr {
        Pr {
            number: n,
            state: state.into(),
            url: format!("https://x/{n}"),
            head_ref_name: head.into(),
        }
    }

    fn wt(issue_id: &str, pr_state: &str, dirty: bool, kind: Option<StateKind>) -> IssueWorktree {
        IssueWorktree {
            worktree: "/w".into(),
            branch: "b".into(),
            issue_id: issue_id.into(),
            dirty,
            pr_number: Some(1),
            pr_state: pr_state.into(),
            pr_url: None,
            state: kind.map(|kind| State {
                kind,
                name: "Done".into(),
                color: None,
            }),
            finished: false,
            reason_not_finished: None,
        }
    }

    #[test]
    fn best_pr_prefers_merged_over_open() {
        let prs = vec![
            pr(1, "OPEN", "feat"),
            pr(2, "MERGED", "feat"),
            pr(3, "CLOSED", "feat"),
        ];
        assert_eq!(best_pr(&prs, "feat").unwrap().number, 2);
    }

    #[test]
    fn best_pr_higher_number_within_same_state() {
        let prs = vec![pr(5, "OPEN", "feat"), pr(9, "OPEN", "feat")];
        assert_eq!(best_pr(&prs, "feat").unwrap().number, 9);
    }

    #[test]
    fn best_pr_none_for_unknown_head() {
        let prs = vec![pr(1, "MERGED", "feat")];
        assert!(best_pr(&prs, "other").is_none());
    }

    #[test]
    fn apply_best_overlays_pr_and_skips_detached() {
        let prs = Prs::for_test(vec![pr(7, "MERGED", "lev/eng-1-foo")]);
        let mut row = wt("ENG-1", "NO_PR", false, None);
        row.branch = "lev/eng-1-foo".into();
        row.pr_number = None;
        prs.apply_best(&mut row);
        assert_eq!(row.pr_number, Some(7));
        assert_eq!(row.pr_state, "MERGED");

        let mut detached = wt("UNKNOWN", "NO_PR", false, None);
        detached.branch = "DETACHED".into();
        detached.pr_number = None;
        prs.apply_best(&mut detached);
        assert_eq!(detached.pr_number, None);
        assert_eq!(detached.pr_state, "NO_PR");
    }

    #[test]
    fn finished_when_merged_done_clean() {
        assert!(
            reason_not_finished(
                &wt("ENG-1", "MERGED", false, Some(StateKind::Completed)),
                &tracker(TrackerKind::Linear, true),
                false
            )
            .is_none()
        );
    }

    #[test]
    fn not_finished_when_dirty() {
        assert_eq!(
            reason_not_finished(
                &wt("ENG-1", "MERGED", true, Some(StateKind::Completed)),
                &tracker(TrackerKind::Linear, true),
                false
            )
            .as_deref(),
            Some("dirty")
        );
    }

    #[test]
    fn pr_only_ignores_tracker_state() {
        // No state, no tracker, but pr_only drops the state gate.
        assert!(
            reason_not_finished(
                &wt("ENG-1", "MERGED", false, None),
                &tracker(TrackerKind::None, false),
                true
            )
            .is_none()
        );
    }

    #[test]
    fn pr_only_allows_unknown_issue_id() {
        // A repo without issue-id branch names has UNKNOWN issue ids; with
        // pr_only a merged + clean worktree is still finished.
        assert!(
            reason_not_finished(
                &wt("UNKNOWN", "MERGED", false, None),
                &tracker(TrackerKind::None, false),
                true
            )
            .is_none()
        );
    }

    #[test]
    fn pr_only_unknown_still_gated_on_pr() {
        assert_eq!(
            reason_not_finished(
                &wt("UNKNOWN", "NO_PR", false, None),
                &tracker(TrackerKind::None, false),
                true
            )
            .as_deref(),
            Some("no PR")
        );
    }

    #[test]
    fn verdict_combinations() {
        let linear = tracker(TrackerKind::Linear, true);
        // Unknown id is never an issue worktree.
        assert_eq!(
            reason_not_finished(
                &wt("UNKNOWN", "MERGED", false, Some(StateKind::Completed)),
                &linear,
                false
            )
            .as_deref(),
            Some("not an issue worktree")
        );
        // No PR + a tracker with no key, all reasons join with ", ".
        assert_eq!(
            reason_not_finished(
                &wt("ENG-2", "NO_PR", false, None),
                &tracker(TrackerKind::Linear, false),
                false
            )
            .as_deref(),
            Some("no PR, no tracker key")
        );
        // Open PR + started state + dirty; the reason names the tracker.
        assert_eq!(
            reason_not_finished(
                &wt("ENG-3", "OPEN", true, Some(StateKind::Started)),
                &linear,
                false
            )
            .as_deref(),
            Some("PR not merged, Linear Done, dirty")
        );
        // A ready tracker with no row for the issue.
        assert_eq!(
            reason_not_finished(&wt("ENG-4", "MERGED", false, None), &linear, false).as_deref(),
            Some("tracker state unknown")
        );
    }

    #[test]
    fn the_reason_names_whichever_tracker_produced_the_state() {
        let row = wt("ENG-5", "MERGED", false, Some(StateKind::Started));
        assert_eq!(
            reason_not_finished(&row, &tracker(TrackerKind::Github, true), false).as_deref(),
            Some("GitHub Done")
        );
    }

    #[test]
    fn prs_empty_leaves_row_untouched() {
        let mut r = wt("ENG-1", "NO_PR", false, None);
        r.pr_number = None;
        Prs::empty().apply_best(&mut r);
        assert_eq!(r.pr_number, None);
        assert_eq!(r.pr_state, "NO_PR");
    }

    // dirty_stream must report each index exactly once with the same result
    // dirty_many computes. Plain (non-git) dirs make dirty_of return false;
    // one dir is a git repo with an untracked file, so exactly one index is
    // true and the value comparison catches wrong-value or wrong-index bugs.
    #[test]
    fn dirty_stream_reports_every_index_once() {
        use std::sync::Mutex;
        let base = std::env::temp_dir().join(format!("devkit-dstream-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let paths: Vec<String> = (0..7)
            .map(|i| {
                let p = base.join(format!("d{i}"));
                std::fs::create_dir_all(&p).unwrap();
                p.to_string_lossy().into_owned()
            })
            .collect();
        // A repo with an untracked file: `git status --porcelain` is non-empty.
        git(&["init", "-q", "-b", "main"], &paths[3]).unwrap();
        std::fs::write(std::path::Path::new(&paths[3]).join("f"), "x").unwrap();

        let got: Mutex<Vec<Option<bool>>> = Mutex::new(vec![None; paths.len()]);
        dirty_stream(&paths, |i, d| {
            let mut g = got.lock().unwrap();
            assert!(g[i].is_none(), "index {i} reported twice");
            g[i] = Some(d);
        });
        let got = got.into_inner().unwrap();
        let want = dirty_many(&paths);
        assert_eq!(
            want,
            vec![false, false, false, true, false, false, false],
            "only the repo with the untracked file is dirty"
        );
        assert_eq!(
            got.into_iter()
                .map(|o| o.expect("index missing"))
                .collect::<Vec<_>>(),
            want
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
