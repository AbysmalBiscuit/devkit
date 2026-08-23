use crate::triage::{self, render};
use anyhow::Result;
use devkit_common::livetable::{Cell, LiveTable};
use devkit_common::tracker::{State, linear};
use devkit_common::ui;
use devkit_issue::status::{self as st, IssueWorktree, StatusReport};
use std::collections::HashMap;
use std::sync::mpsc;

pub(crate) const COL_TREE: usize = 2;
pub(crate) const COL_PR: usize = 3;
pub(crate) const COL_LINEAR: usize = 4;
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
    linear: Option<HashMap<String, State>>,
    workspace: Option<String>,
    has_key: bool,
}

impl LiveState {
    fn new(rows: Vec<IssueWorktree>, has_key: bool) -> LiveState {
        let n = rows.len();
        LiveState {
            rows,
            dirty: vec![false; n],
            dirty_seen: vec![false; n],
            prs: None,
            linear: None,
            workspace: None,
            has_key,
        }
    }

    fn done(&self) -> bool {
        self.dirty_seen.iter().all(|&s| s) && self.prs.is_some() && self.linear.is_some()
    }

    /// VERDICT write for row `i` if all its inputs are now present.
    fn verdict_write(&mut self, i: usize, out: &mut Vec<(usize, usize, String)>) {
        if !(self.dirty_seen[i] && self.prs.is_some() && self.linear.is_some()) {
            return;
        }
        let reason = st::reason_not_finished(&self.rows[i], self.has_key, false);
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
            prs.apply_best(&mut self.rows[i]);
            out.push((i, COL_PR, triage::pr_cell(&self.rows[i])));
        }
        self.prs = Some(prs);
        for i in 0..self.rows.len() {
            self.verdict_write(i, &mut out);
        }
        out
    }

    fn apply_linear(
        &mut self,
        states: HashMap<String, State>,
        workspace: Option<String>,
    ) -> Vec<(usize, usize, String)> {
        let mut out = Vec::new();
        for i in 0..self.rows.len() {
            if let Some(s) = states.get(&self.rows[i].issue_id) {
                self.rows[i].linear_kind = Some(s.kind.to_string());
                self.rows[i].linear_name = Some(s.name.clone());
            }
            out.push((
                i,
                COL_LINEAR,
                triage::linear_cell(&self.rows[i], self.has_key),
            ));
        }
        self.linear = Some(states);
        self.workspace = workspace;
        for i in 0..self.rows.len() {
            self.verdict_write(i, &mut out);
        }
        out
    }

    /// The raw collected results, for `st::assemble`.
    #[allow(clippy::type_complexity)]
    fn into_parts(self) -> (Vec<bool>, st::Prs, HashMap<String, State>, Option<String>) {
        (
            self.dirty,
            self.prs.expect("apply_prs ran"),
            self.linear.expect("apply_linear ran"),
            self.workspace,
        )
    }
}

enum Update {
    Dirty(usize, bool),
    Prs(Result<st::Prs>),
    Linear(HashMap<String, State>, Option<String>),
}

/// The single status line under the live table; sources drop out of the
/// message as they land. One line, not one per source: every status row the
/// block allocates scrolls the screen and stays scrolled after the row is
/// gone, so the block's footprint beyond the table is kept to the one line
/// the shell prompt will overwrite.
fn progress_msg(prs_done: bool, linear_done: bool) -> String {
    let mut parts = vec!["Checking worktrees"];
    if !prs_done {
        parts.push("fetching PRs");
    }
    if !linear_done {
        parts.push("fetching Linear");
    }
    parts.join(" · ")
}

/// Discover worktrees, then draw the triage table immediately — ISSUE/BRANCH
/// known, other cells as spinners — and fill it as each source lands. The
/// live block animates on stderr and is cleared; the returned report renders
/// to stdout exactly as the silent gather would.
pub fn gather_live(start: &str, ids: &[String]) -> Result<StatusReport> {
    let d = st::discover(start, ids)?;
    let key = devkit_common::secrets::resolve("LINEAR_API_KEY");
    let has_key = key.is_some();
    if d.is_empty() {
        // No worktrees means no PR fetch — the report is empty either way.
        return Ok(st::assemble(
            d,
            Vec::new(),
            st::Prs::empty(),
            HashMap::new(),
            None,
            has_key,
        ));
    }

    let m = d.len();
    let paths = d.worktree_paths();
    let ids_v: Vec<String> = d.issue_ids().to_vec();
    let disp = display_order(d.rows());

    let mut lt = LiveTable::new("ISSUE WORKTREES", &triage::HEADERS, m);
    for (i, row) in d.rows().iter().enumerate() {
        // Workspace key is unknown until Linear responds; links appear in the
        // final stdout render.
        lt.set(disp[i], 0, Cell::Ready(triage::issue_cell(row, None)));
        lt.set(disp[i], 1, Cell::Ready(triage::branch_cell(&row.branch)));
    }
    lt.redraw();
    let progress = lt.bar(&progress_msg(false, false), m as u64);

    let mut state = LiveState::new(d.rows().to_vec(), has_key);

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
                let _ = tx.send(Update::Prs(st::fetch_prs(d)));
            });
        }
        {
            let tx = tx.clone();
            let ids_v = &ids_v;
            let key = key.clone();
            s.spawn(move || {
                let (states, ws) = std::thread::scope(|s2| {
                    let stt = s2.spawn(|| linear::states(ids_v, key.as_deref()));
                    let wst = s2.spawn(linear::workspace_url_key);
                    (
                        stt.join().expect("linear states thread panicked"),
                        wst.join().expect("linear url-key thread panicked"),
                    )
                });
                let _ = tx.send(Update::Linear(states, ws));
            });
        }
        drop(tx);

        let mut prs_done = false;
        let mut linear_done = false;
        lt.drive(&rx, |lt, msg| {
            let writes = match msg {
                Update::Dirty(i, dirty) => {
                    progress.inc(1);
                    state.apply_dirty(i, dirty)
                }
                Update::Prs(res) => {
                    prs_done = true;
                    progress.set_message(progress_msg(prs_done, linear_done));
                    state.apply_prs(res?)
                }
                Update::Linear(states, ws) => {
                    linear_done = true;
                    progress.set_message(progress_msg(prs_done, linear_done));
                    state.apply_linear(states, ws)
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

    let (dirty, prs, linear_states, ws) = state.into_parts();
    Ok(st::assemble(d, dirty, prs, linear_states, ws, has_key))
}

pub fn run(start: &str, ids: &[String]) -> Result<()> {
    let report = gather_live(start, ids)?;
    let finished = render(&report, false);
    if finished > 0 {
        println!(
            "\n{} Run `issue end` to remove them.",
            ui::green(&format!("{finished} finished."))
        );
    }
    if !report.has_linear_key {
        println!(
            "\n{}",
            ui::dim(
                "LINEAR_API_KEY unset — Linear gate skipped. Create a key at https://linear.app/settings/api"
            )
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use devkit_common::tracker::StateKind;
    use devkit_issue::status::IssueWorktree;

    fn row(id: &str) -> IssueWorktree {
        IssueWorktree {
            worktree: format!("/w/{id}"),
            branch: format!("lev/{}-x", id.to_lowercase()),
            issue_id: id.into(),
            dirty: false,
            pr_number: None,
            pr_state: "NO_PR".into(),
            pr_url: None,
            linear_kind: None,
            linear_name: None,
            finished: false,
            reason_not_finished: None,
        }
    }

    #[test]
    fn progress_msg_drops_sources_as_they_land() {
        assert_eq!(
            progress_msg(false, false),
            "Checking worktrees · fetching PRs · fetching Linear"
        );
        assert_eq!(
            progress_msg(true, false),
            "Checking worktrees · fetching Linear"
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
        let mut state = LiveState::new(vec![row("ENG-1"), row("ENG-2")], false);

        let w1 = state.apply_dirty(0, true);
        assert!(w1.iter().any(|(r, c, _)| (*r, *c) == (0, COL_TREE)));
        assert!(!w1.iter().any(|(_, c, _)| *c == COL_VERDICT));

        let w2 = state.apply_prs(st::Prs::empty());
        assert!(w2.iter().any(|(r, c, _)| (*r, *c) == (1, COL_PR)));
        assert!(!w2.iter().any(|(_, c, _)| *c == COL_VERDICT));

        let w3 = state.apply_linear(std::collections::HashMap::new(), None);
        // Linear was the last input for row 0 (dirty done); row 1's dirty is
        // still missing, so only row 0 gains a verdict.
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
        let mut state = LiveState::new(vec![row("ENG-1"), row("ENG-2")], false);
        state.apply_dirty(0, false);
        state.apply_dirty(1, true);
        let w = state.apply_linear(std::collections::HashMap::new(), None);
        assert!(!w.iter().any(|(_, c, _)| *c == COL_VERDICT));
        assert!(!state.done());

        let w = state.apply_prs(st::Prs::empty());
        assert!(w.iter().any(|(r, c, _)| (*r, *c) == (0, COL_VERDICT)));
        assert!(w.iter().any(|(r, c, _)| (*r, *c) == (1, COL_VERDICT)));
        assert!(state.done());
    }

    #[test]
    fn collected_parts_match_inputs() {
        let mut state = LiveState::new(vec![row("ENG-1")], true);
        state.apply_dirty(0, true);
        state.apply_prs(st::Prs::empty());
        state.apply_linear(std::collections::HashMap::new(), Some("acme".into()));
        let (dirty, _prs, _linear, ws) = state.into_parts();
        assert_eq!(dirty, vec![true]);
        assert_eq!(ws.as_deref(), Some("acme"));
    }

    // LiveState's Linear overlay + verdict duplicate what `assemble` computes;
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

        let mut state = LiveState::new(rows.clone(), true);
        for (i, &dt) in dirty.iter().enumerate() {
            state.apply_dirty(i, dt);
        }
        state.apply_prs(st::Prs::empty());
        state.apply_linear(states.clone(), Some("acme".into()));

        let ids = vec!["ENG-1".into(), "ENG-2".into(), "ENG-3".into()];
        let d = st::Discovered::from_parts(rows, "/main".into(), ids);
        let report = st::assemble(
            d,
            dirty,
            st::Prs::empty(),
            states,
            Some("acme".into()),
            true,
        );

        assert_eq!(state.rows.len(), report.worktrees.len());
        for (live, assembled) in state.rows.iter().zip(&report.worktrees) {
            let id = &assembled.issue_id;
            assert_eq!(live.finished, assembled.finished, "finished for {id}");
            assert_eq!(
                live.reason_not_finished, assembled.reason_not_finished,
                "reason for {id}"
            );
            assert_eq!(live.linear_kind, assembled.linear_kind, "kind for {id}");
            assert_eq!(live.linear_name, assembled.linear_name, "name for {id}");
        }
    }
}
