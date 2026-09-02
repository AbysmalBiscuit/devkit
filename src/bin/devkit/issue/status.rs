use crate::issue::triage::{self, render};
use anyhow::Result;
use devkit_common::livetable::{Cell, LiveTable};
use devkit_common::tracker::{State, TrackerKind};
use devkit_common::ui;
use devkit_issue::status::{self as st, IssueWorktree, StatusReport, TrackerInfo};
use std::collections::HashMap;
use std::sync::mpsc;

pub(crate) const COL_TREE: usize = 2;
pub(crate) const COL_PR: usize = 3;
pub(crate) const COL_STATE: usize = 4;
pub(crate) const COL_VERDICT: usize = 5;

/// Discovery index → display row, matching `triage::render`'s sort (by
/// issue id, stable on ties).
fn display_order(rows: &[IssueWorktree]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..rows.len()).collect();
    idx.sort_by(|&a, &b| rows[a].issue_id.cmp(&rows[b].issue_id));
    let mut disp = vec![0usize; rows.len()];
    for (d, &i) in idx.iter().enumerate() {
        disp[i] = d;
    }
    disp
}

/// IO-free accumulator for the live gather. Each `apply_*` overlays one source
/// onto the rows and returns cell writes as `(discovery_index, column,
/// content)`; a row's VERDICT write is emitted only once all three sources
/// have reported for it. Keeps raw results so the caller can feed the same
/// `assemble` the silent gather uses — the stdout table stays byte-identical.
struct LiveState {
    /// Working copies for cell rendering; the final report is recomputed by
    /// `assemble` from the raw parts — never feed these rows to the report.
    rows: Vec<IssueWorktree>,
    dirty: Vec<bool>,
    dirty_seen: Vec<bool>,
    prs: Option<st::Prs>,
    states: Option<HashMap<String, State>>,
    tracker: TrackerInfo,
}

impl LiveState {
    fn new(rows: Vec<IssueWorktree>, tracker: TrackerInfo) -> LiveState {
        let n = rows.len();
        LiveState {
            rows,
            dirty: vec![false; n],
            dirty_seen: vec![false; n],
            prs: None,
            states: None,
            tracker,
        }
    }

    fn done(&self) -> bool {
        self.dirty_seen.iter().all(|&s| s) && self.prs.is_some() && self.states.is_some()
    }

    /// VERDICT write for row `i` if all its inputs are now present.
    fn verdict_write(&mut self, i: usize, out: &mut Vec<(usize, usize, String)>) {
        if !(self.dirty_seen[i] && self.prs.is_some() && self.states.is_some()) {
            return;
        }
        let reason = st::reason_not_finished(&self.rows[i], &self.tracker, false);
        self.rows[i].finished = reason.is_none();
        self.rows[i].reason_not_finished = reason;
        out.push((i, COL_VERDICT, triage::verdict_cell(&self.rows[i], false)));
    }

    fn apply_dirty(&mut self, i: usize, dirty: bool) -> Vec<(usize, usize, String)> {
        self.dirty[i] = dirty;
        self.dirty_seen[i] = true;
        self.rows[i].dirty = dirty;
        let mut out = vec![(i, COL_TREE, triage::tree_cell(dirty))];
        self.verdict_write(i, &mut out);
        out
    }

    fn apply_prs(&mut self, prs: st::Prs) -> Vec<(usize, usize, String)> {
        let mut out = Vec::new();
        for i in 0..self.rows.len() {
            prs.apply(&mut self.rows[i]);
            out.push((i, COL_PR, triage::pr_cell(&self.rows[i])));
        }
        self.prs = Some(prs);
        for i in 0..self.rows.len() {
            self.verdict_write(i, &mut out);
        }
        out
    }

    fn apply_states(
        &mut self,
        states: HashMap<String, State>,
        link_base: Option<String>,
    ) -> Vec<(usize, usize, String)> {
        let mut out = Vec::new();
        for i in 0..self.rows.len() {
            if let Some(s) = states.get(&self.rows[i].issue_id) {
                self.rows[i].state = Some(s.clone());
            }
            out.push((
                i,
                COL_STATE,
                triage::state_cell(&self.rows[i], self.tracker.ready),
            ));
        }
        self.states = Some(states);
        self.tracker.link_base = link_base;
        for i in 0..self.rows.len() {
            self.verdict_write(i, &mut out);
        }
        out
    }

    /// The raw collected results, for `st::assemble`.
    fn into_parts(self) -> (Vec<bool>, st::Prs, HashMap<String, State>, TrackerInfo) {
        (
            self.dirty,
            self.prs.expect("apply_prs ran"),
            self.states.expect("apply_states ran"),
            self.tracker,
        )
    }
}

enum Update {
    Dirty(usize, bool),
    Prs(Result<st::Prs>),
    States(HashMap<String, State>, Option<String>),
}

/// The single status line under the live table; sources drop out of the
/// message as they land. One line, not one per source: every status row the
/// block allocates scrolls the screen and stays scrolled after the row is
/// gone, so the block's footprint beyond the table is kept to the one line
/// the shell prompt will overwrite.
fn progress_msg(prs_done: bool, states_done: bool) -> String {
    let mut parts = vec!["Checking worktrees"];
    if !prs_done {
        parts.push("fetching PRs");
    }
    if !states_done {
        parts.push("fetching issue states");
    }
    parts.join(" · ")
}

/// Discover worktrees, then draw the triage table immediately — ISSUE/BRANCH
/// known, other cells as spinners — and fill it as each source lands. The
/// live block animates on stderr and is cleared; the returned report renders
/// to stdout exactly as the silent gather would.
pub fn gather_live(start: &str, ids: &[String], config: Option<&str>) -> Result<StatusReport> {
    let d = st::discover(start, ids)?;
    let (resolved, repos) = crate::issue::tracker::select(config, start, None);
    let info = TrackerInfo::of(&resolved);
    if d.is_empty() {
        // No worktrees means no PR fetch — the report is empty either way.
        return Ok(st::assemble(
            d,
            Vec::new(),
            st::Prs::empty(),
            HashMap::new(),
            info,
        ));
    }

    let repo = repos.prs()?;
    let m = d.len();
    let paths = d.worktree_paths();
    let ids_v: Vec<String> = d.issue_ids().to_vec();
    let disp = display_order(d.rows());

    let mut lt = LiveTable::new("ISSUE WORKTREES", &triage::HEADERS, m);
    for (i, row) in d.rows().iter().enumerate() {
        // The link base is unknown until the tracker responds; links appear in
        // the final stdout render.
        lt.set(disp[i], 0, Cell::Ready(triage::issue_cell(row, None)));
        lt.set(disp[i], 1, Cell::Ready(triage::branch_cell(&row.branch)));
    }
    lt.redraw();
    let progress = lt.bar(&progress_msg(false, false), m as u64);

    let mut state = LiveState::new(d.rows().to_vec(), info);

    let looped: Result<()> = std::thread::scope(|s| {
        let (tx, rx) = mpsc::channel::<Update>();
        {
            let tx = tx.clone();
            let paths = &paths;
            s.spawn(move || {
                st::dirty_stream(paths, move |i, dirty| {
                    let _ = tx.send(Update::Dirty(i, dirty));
                });
            });
        }
        {
            let tx = tx.clone();
            let d = &d;
            s.spawn(move || {
                let _ = tx.send(Update::Prs(st::fetch_prs(d, repo)));
            });
        }
        {
            let tx = tx.clone();
            let ids_v = &ids_v;
            let tracker = resolved.tracker.as_ref();
            s.spawn(move || {
                let (states, link_base) = std::thread::scope(|s2| {
                    let stt = s2.spawn(|| tracker.states(ids_v));
                    let lbt = s2.spawn(|| tracker.issue_url(""));
                    (
                        stt.join().expect("tracker states thread panicked"),
                        lbt.join().expect("tracker link-base thread panicked"),
                    )
                });
                let _ = tx.send(Update::States(states, link_base));
            });
        }
        drop(tx);

        let mut prs_done = false;
        let mut states_done = false;
        lt.drive(&rx, |lt, msg| {
            let writes = match msg {
                Update::Dirty(i, dirty) => {
                    progress.inc(1);
                    state.apply_dirty(i, dirty)
                }
                Update::Prs(res) => {
                    prs_done = true;
                    progress.set_message(progress_msg(prs_done, states_done));
                    state.apply_prs(res?)
                }
                Update::States(states, link_base) => {
                    states_done = true;
                    progress.set_message(progress_msg(prs_done, states_done));
                    state.apply_states(states, link_base)
                }
            };
            for (i, col, content) in writes {
                lt.set(disp[i], col, Cell::Ready(content));
            }
            Ok(state.done())
        })
    });
    // Clear the live block before any error renders, so the anyhow report is
    // not printed under a half-drawn region.
    lt.finish();
    looped?;

    let (dirty, prs, states, info) = state.into_parts();
    Ok(st::assemble(d, dirty, prs, states, info))
}

/// The note printed under the table for a tracker that could not answer, or
/// `None` when there is nothing worth saying.
///
/// A tracker that cannot answer holds every issue-state gate closed, which is
/// why nothing reaches FINISHED — so the hint explains the closed gate and how
/// to open it. The one project that genuinely skips the gate is the one that
/// declared it has no tracker, and it asked for that, so it gets no note.
///
/// A project whose named tracker could not be built is told what stopped it
/// instead of being pointed at the config key it already set.
fn tracker_hint(t: &TrackerInfo) -> Option<String> {
    if t.ready {
        return None;
    }
    if let Some(why) = devkit_common::tracker::unbuilt_reason(t.kind, t.declared, &t.reason) {
        return Some(format!(
            "{why} — issue state gates stay closed, so nothing reports finished"
        ));
    }
    let generic = match t.kind {
        TrackerKind::Linear => {
            "No LINEAR_API_KEY — Linear state gates stay closed, so nothing reports finished. \
             Create a key at https://linear.app/settings/api"
        }
        TrackerKind::Github => {
            "No GitHub token — GitHub state gates stay closed, so nothing reports finished. \
             Set GH_TOKEN/GITHUB_TOKEN or run `gh auth login`"
        }
        TrackerKind::None if t.declared => return None,
        TrackerKind::None => {
            "No issue tracker devkit can read — issue state gates stay closed, so nothing \
             reports finished. Set `[tracker] kind` to name this project's tracker"
        }
    };
    Some(generic.into())
}

pub fn run(start: &str, ids: &[String], config: Option<&str>) -> Result<()> {
    let report = gather_live(start, ids, config)?;
    let finished = render(&report, false);
    if finished > 0 {
        println!(
            "\n{} Run `issue end` to remove them.",
            ui::green(&format!("{finished} finished."))
        );
    }
    if let Some(hint) = tracker_hint(&report.tracker) {
        println!("\n{}", ui::dim(&hint));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use devkit_common::tracker::StateKind;
    use devkit_issue::status::{IssueWorktree, PrStatus};

    const LINK_BASE: &str = "https://linear.app/acme/issue/";

    fn tracker(ready: bool) -> TrackerInfo {
        TrackerInfo {
            kind: TrackerKind::Linear,
            ready,
            declared: true,
            reason: "[tracker] kind = \"linear\"".into(),
            link_base: None,
        }
    }

    fn info(kind: TrackerKind, ready: bool, declared: bool) -> TrackerInfo {
        with_reason(kind, ready, declared, "detected: something")
    }

    fn with_reason(kind: TrackerKind, ready: bool, declared: bool, reason: &str) -> TrackerInfo {
        TrackerInfo {
            kind,
            ready,
            declared,
            reason: reason.into(),
            link_base: None,
        }
    }

    fn row(id: &str) -> IssueWorktree {
        IssueWorktree {
            worktree: format!("/w/{id}"),
            branch: format!("lev/{}-x", id.to_lowercase()),
            issue_id: id.into(),
            dirty: false,
            pr: PrStatus::None,
            state: None,
            finished: false,
            reason_not_finished: None,
        }
    }

    /// A worktree whose only remaining gate is the tracker's: merged PR, clean
    /// tree, real issue id.
    fn merged_clean(id: &str) -> IssueWorktree {
        let mut r = row(id);
        r.pr = PrStatus::Unique {
            number: 1,
            state: "MERGED".into(),
            url: "".into(),
            is_draft: false,
        };
        r
    }

    /// Every tracker that cannot answer keeps a merged, clean worktree off
    /// FINISHED, so its hint has to say the gate is closed rather than skipped —
    /// the reader is looking for why nothing finished.
    #[test]
    fn a_tracker_that_holds_the_gate_says_so() {
        for (kind, declared) in [
            (TrackerKind::Linear, true),
            (TrackerKind::Github, false),
            (TrackerKind::None, false),
        ] {
            let t = info(kind, false, declared);
            assert!(
                st::reason_not_finished(&merged_clean("ENG-1"), &t, false).is_some(),
                "{kind:?} holds the gate"
            );
            let hint = tracker_hint(&t).unwrap_or_else(|| panic!("{kind:?} needs a hint"));
            assert!(hint.contains("stay closed"), "{kind:?}: {hint}");
            assert!(!hint.contains("skipped"), "{kind:?}: {hint}");
        }
    }

    /// Only a project that declared it has no tracker really skips the gate, and
    /// that is what it asked for — nothing to report.
    #[test]
    fn a_declared_absence_of_a_tracker_gets_no_hint() {
        let t = info(TrackerKind::None, false, true);
        assert!(st::reason_not_finished(&merged_clean("ENG-1"), &t, false).is_none());
        assert_eq!(tracker_hint(&t), None);
    }

    #[test]
    fn a_tracker_that_answered_gets_no_hint() {
        assert_eq!(tracker_hint(&info(TrackerKind::Linear, true, true)), None);
    }

    /// A GitHub project has already named its tracker, so the fix is the token,
    /// not the config key it already set.
    #[test]
    fn the_github_hint_points_at_the_token_not_the_config() {
        let hint = tracker_hint(&info(TrackerKind::Github, false, true)).unwrap();
        assert!(hint.contains("GH_TOKEN"), "{hint}");
        assert!(!hint.contains("[tracker] kind"), "{hint}");
    }

    /// A project whose named tracker could not be built lands on the same kind
    /// and `declared` pair as a project that named nothing, and only the reason
    /// tells them apart. Telling the first one to set the key it already set
    /// points away from the actual fault, so the reason has to carry the hint.
    #[test]
    fn a_named_tracker_that_did_not_resolve_reports_why() {
        let t = with_reason(
            TrackerKind::None,
            false,
            false,
            "github selected but no issues repository: no github.com `origin` remote",
        );
        let hint = tracker_hint(&t).expect("an unresolved tracker needs a hint");
        assert!(hint.contains("no issues repository"), "{hint}");
        assert!(!hint.contains("[tracker] kind"), "{hint}");
    }

    /// The Linear hint is the only one that can point at a fix the user makes
    /// outside the config, so it keeps the key's URL.
    #[test]
    fn the_linear_hint_points_at_the_api_key_page() {
        let hint = tracker_hint(&info(TrackerKind::Linear, false, true)).unwrap();
        assert!(hint.contains("https://linear.app/settings/api"), "{hint}");
    }

    #[test]
    fn progress_msg_drops_sources_as_they_land() {
        assert_eq!(
            progress_msg(false, false),
            "Checking worktrees · fetching PRs · fetching issue states"
        );
        assert_eq!(
            progress_msg(true, false),
            "Checking worktrees · fetching issue states"
        );
        assert_eq!(
            progress_msg(false, true),
            "Checking worktrees · fetching PRs"
        );
        assert_eq!(progress_msg(true, true), "Checking worktrees");
    }

    #[test]
    fn display_order_sorts_by_issue_id() {
        let rows = vec![row("ENG-2"), row("ENG-1"), row("ENG-3")];
        // discovery index -> display row
        assert_eq!(display_order(&rows), vec![1, 0, 2]);
    }

    #[test]
    fn display_order_is_stable_on_ties() {
        // Equal ids keep their discovery order, matching render's stable sort.
        let rows = vec![row("UNKNOWN"), row("ENG-1"), row("UNKNOWN")];
        assert_eq!(display_order(&rows), vec![1, 0, 2]);
    }

    // Verdict cells appear only once a row has all three inputs, regardless of
    // arrival order; earlier sources fill their own column immediately.
    #[test]
    fn verdicts_wait_for_all_sources() {
        let mut state = LiveState::new(vec![row("ENG-1"), row("ENG-2")], tracker(false));

        let w1 = state.apply_dirty(0, true);
        assert!(w1.iter().any(|(r, c, _)| (*r, *c) == (0, COL_TREE)));
        assert!(!w1.iter().any(|(_, c, _)| *c == COL_VERDICT));

        let w2 = state.apply_prs(st::Prs::empty());
        assert!(w2.iter().any(|(r, c, _)| (*r, *c) == (1, COL_PR)));
        assert!(!w2.iter().any(|(_, c, _)| *c == COL_VERDICT));

        let w3 = state.apply_states(std::collections::HashMap::new(), None);
        // The tracker was the last input for row 0 (dirty done); row 1's dirty
        // is still missing, so only row 0 gains a verdict.
        assert!(w3.iter().any(|(r, c, _)| (*r, *c) == (0, COL_VERDICT)));
        assert!(!w3.iter().any(|(r, c, _)| (*r, *c) == (1, COL_VERDICT)));
        assert!(!state.done());

        let w4 = state.apply_dirty(1, false);
        assert!(w4.iter().any(|(r, c, _)| (*r, *c) == (1, COL_VERDICT)));
        assert!(state.done());
    }

    // When PRs are the last source to land, their apply emits the verdict
    // flood for every row.
    #[test]
    fn prs_last_emits_all_verdicts() {
        let mut state = LiveState::new(vec![row("ENG-1"), row("ENG-2")], tracker(false));
        state.apply_dirty(0, false);
        state.apply_dirty(1, true);
        let w = state.apply_states(std::collections::HashMap::new(), None);
        assert!(!w.iter().any(|(_, c, _)| *c == COL_VERDICT));
        assert!(!state.done());

        let w = state.apply_prs(st::Prs::empty());
        assert!(w.iter().any(|(r, c, _)| (*r, *c) == (0, COL_VERDICT)));
        assert!(w.iter().any(|(r, c, _)| (*r, *c) == (1, COL_VERDICT)));
        assert!(state.done());
    }

    #[test]
    fn collected_parts_match_inputs() {
        let mut state = LiveState::new(vec![row("ENG-1")], tracker(true));
        state.apply_dirty(0, true);
        state.apply_prs(st::Prs::empty());
        state.apply_states(std::collections::HashMap::new(), Some(LINK_BASE.into()));
        let (dirty, _prs, _states, info) = state.into_parts();
        assert_eq!(dirty, vec![true]);
        assert_eq!(info.link_base.as_deref(), Some(LINK_BASE));
    }

    // LiveState's state overlay + verdict duplicate what `assemble` computes;
    // this pins them together so the live cells can never disagree with the
    // final report built from the same raw inputs. `assemble` keeps discovery
    // order, so the comparison is index-aligned.
    #[test]
    fn live_state_matches_assemble() {
        let rows = vec![row("ENG-1"), row("ENG-2"), row("ENG-3")];
        let dirty = vec![false, false, true];
        let mut states = std::collections::HashMap::new();
        states.insert(
            "ENG-1".to_string(),
            State {
                kind: StateKind::Completed,
                name: "Done".into(),
                color: None,
            },
        );
        states.insert(
            "ENG-3".to_string(),
            State {
                kind: StateKind::Started,
                name: "In Progress".into(),
                color: None,
            },
        );

        let mut state = LiveState::new(rows.clone(), tracker(true));
        for (i, &dt) in dirty.iter().enumerate() {
            state.apply_dirty(i, dt);
        }
        state.apply_prs(st::Prs::empty());
        state.apply_states(states.clone(), Some(LINK_BASE.into()));

        let ids = vec!["ENG-1".into(), "ENG-2".into(), "ENG-3".into()];
        let d = st::Discovered::from_parts(rows, ids);
        let report = st::assemble(
            d,
            dirty,
            st::Prs::empty(),
            states,
            TrackerInfo {
                link_base: Some(LINK_BASE.into()),
                ..tracker(true)
            },
        );

        assert_eq!(state.rows.len(), report.worktrees.len());
        for (live, assembled) in state.rows.iter().zip(&report.worktrees) {
            let id = &assembled.issue_id;
            assert_eq!(live.finished, assembled.finished, "finished for {id}");
            assert_eq!(
                live.reason_not_finished, assembled.reason_not_finished,
                "reason for {id}"
            );
            assert_eq!(live.state, assembled.state, "state for {id}");
        }
    }
}
