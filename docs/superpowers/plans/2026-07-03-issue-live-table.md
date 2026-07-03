# Issue Live-Table Rendering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `issue` status/info draw their table immediately and fill cells as data sources land (animated in-cell spinners); `issue prs` renders its last-run snapshot dim while refreshing; step-driven commands keep completed steps on screen as numbered `✓` lines.

**Architecture:** A `LiveLines` block (raw rewritable stderr lines over `indicatif::MultiProgress`) with a `LiveTable` cell layer on top, both TTY-gated like `progress::Steps`. Data gathering becomes event-driven: source threads report over one `mpsc` channel; a `recv_timeout(100ms)` loop applies updates and ticks the spinner frame. The final table always prints to stdout via the existing render path — live output is stderr-only and cleared, so pipes/MCP/tests are byte-identical to today.

**Tech Stack:** Rust (edition 2024), indicatif 0.17 (`MultiProgress`), comfy-table (existing `ui::table`), std `mpsc` + scoped threads. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-07-03-issue-live-table-design.md` (decisions D1–D4).

## File structure

| File | Change | Responsibility |
|---|---|---|
| `crates/devkit-common/src/livetable.rs` | create | `FRAMES`, `Cell`, `LiveLines` (rewritable stderr block + status bars), `LiveTable` (cells → rendered lines), pure `render_lines` |
| `crates/devkit-common/src/lib.rs` | modify | register `pub mod livetable;` |
| `crates/devkit-common/src/progress.rs` | modify | `Steps` persistent mode: `persistent()`, `persistent_with_total`, `during_result`, `✓/✗ n. msg (elapsed)` lines |
| `crates/devkit-issue/src/status.rs` | modify | `Prs::empty()`, `dirty_stream` (per-path streaming dirty checks) |
| `crates/devkit-issue/src/prs.rs` | modify | derive `Deserialize` on `MinePrView`/`ReviewPrView` |
| `src/bin/issue/triage.rs` | modify | extract pure cell formatters + `HEADERS` const shared by live and final renders |
| `src/bin/issue/status.rs` | modify | `LiveState` (pure update machine) + event-driven `gather_live` replacing `gather_with_bars` |
| `src/bin/issue/info.rs` | modify | parallelize PR/Linear/workspace, single-row live table |
| `src/bin/issue/prs.rs` | modify | `Snapshot` cache (full rows + diff), stale-while-revalidate render |
| `src/bin/issue/checkout.rs`, `setup.rs`, `end.rs`, `review/request.rs`, `review/finish.rs`, `review/mod.rs` | modify | adopt persistent `Steps` |

Run `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` before every commit; `cargo fmt --all` after edits.

---

### Task 1: Extract cell formatters from `triage::render`

The live table and the final render must produce identical cell text; sharing pure functions is what guarantees it.

**Files:**
- Modify: `src/bin/issue/triage.rs`

- [ ] **Step 1: Write the failing tests**

Append to `src/bin/issue/triage.rs` (new `#[cfg(test)]` module — the file has none):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use devkit_issue::status::IssueWorktree;

    fn row(pr_state: &str) -> IssueWorktree {
        IssueWorktree {
            worktree: "/w".into(),
            branch: "lev/eng-1-x".into(),
            issue_id: "ENG-1".into(),
            dirty: false,
            pr_number: Some(7),
            pr_state: pr_state.into(),
            pr_url: None,
            linear_kind: None,
            linear_name: None,
            finished: false,
            reason_not_finished: None,
        }
    }

    // Off-TTY the colour helpers pass text through, so assertions are plain.
    #[test]
    fn pr_cell_labels() {
        assert_eq!(pr_cell(&row("MERGED")), "MERGED #7");
        let mut r = row("NO_PR");
        r.pr_number = None;
        assert_eq!(pr_cell(&r), "no PR");
    }

    #[test]
    fn tree_cell_states() {
        assert_eq!(tree_cell(false), "clean");
        assert_eq!(tree_cell(true), "dirty");
    }

    #[test]
    fn linear_cell_no_key_vs_unknown() {
        assert_eq!(linear_cell(&row("OPEN"), false), "no key");
        assert_eq!(linear_cell(&row("OPEN"), true), "unknown");
        let mut r = row("OPEN");
        r.linear_kind = Some("completed".into());
        r.linear_name = Some("Done".into());
        assert_eq!(linear_cell(&r, true), "Done");
    }

    #[test]
    fn verdict_cell_variants() {
        assert_eq!(verdict_cell(&row("OPEN"), true), "—");
        let mut r = row("MERGED");
        r.finished = true;
        assert_eq!(verdict_cell(&r, false), "FINISHED");
        let mut r = row("OPEN");
        r.reason_not_finished = Some("PR not merged, dirty".into());
        assert_eq!(verdict_cell(&r, false), "PR not merged, dirty");
    }

    #[test]
    fn issue_cell_unknown_is_plain() {
        let mut r = row("OPEN");
        r.issue_id = "UNKNOWN".into();
        // No workspace / no linear state → bare id (colour is a passthrough here).
        assert_eq!(issue_cell(&r, None), "UNKNOWN");
        assert_eq!(issue_cell(&row("OPEN"), Some("acme")), "ENG-1");
    }

    #[test]
    fn branch_cell_truncates() {
        let long = "x".repeat(60);
        assert_eq!(branch_cell(&long).chars().count(), BRANCH_MAX);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit --bin issue triage:: 2>&1 | tail -20`
Expected: compile FAILURE — `pr_cell`, `tree_cell`, etc. not found.

- [ ] **Step 3: Extract the formatters**

In `src/bin/issue/triage.rs`, above `render`, add the shared header list and the pure functions (bodies moved verbatim from `render`'s inline closures — do not change any string or colour choice):

```rust
/// Column headers shared by the final render and the live table.
pub(crate) const HEADERS: [&str; 6] = ["ISSUE", "BRANCH", "TREE", "PR", "LINEAR", "VERDICT"];

pub(crate) fn issue_cell(row: &IssueWorktree, workspace: Option<&str>) -> String {
    let linked = match workspace {
        Some(k) if row.linear_kind.is_some() => ui::link(
            &row.issue_id,
            &format!("https://linear.app/{k}/issue/{}", row.issue_id),
        ),
        _ => row.issue_id.clone(),
    };
    if row.issue_id == "UNKNOWN" {
        ui::dim(&linked)
    } else {
        ui::cyan(&linked)
    }
}

pub(crate) fn branch_cell(branch: &str) -> String {
    ui::dim(&ui::truncate(branch, BRANCH_MAX))
}

pub(crate) fn tree_cell(dirty: bool) -> String {
    if dirty { ui::red("dirty") } else { ui::dim("clean") }
}

pub(crate) fn pr_cell(row: &IssueWorktree) -> String {
    let label = pr_label(row);
    let colored = match row.pr_state.as_str() {
        "MERGED" => ui::green(&label),
        "OPEN" => ui::yellow(&label),
        "CLOSED" => ui::red(&label),
        _ => ui::dim(&label), // NO_PR
    };
    match &row.pr_url {
        Some(u) => ui::link(&colored, u),
        None => colored,
    }
}

pub(crate) fn linear_cell(row: &IssueWorktree, has_key: bool) -> String {
    match row.linear_kind.as_deref() {
        None => ui::dim(if has_key { "unknown" } else { "no key" }),
        Some(kind) => {
            let name = row.linear_name.as_deref().unwrap_or("");
            match kind {
                "completed" => ui::green(name),
                "started" => ui::yellow(name),
                "canceled" => ui::red(name),
                _ => ui::dim(name),
            }
        }
    }
}

pub(crate) fn verdict_cell(row: &IssueWorktree, offline: bool) -> String {
    if offline {
        ui::dim("—")
    } else if row.finished {
        ui::bold_green("FINISHED")
    } else {
        // The only "ball in your court" reason is a dirty tree; flag it
        // yellow, leave the rest (waiting on PR/Linear) dim.
        match row.reason_not_finished.as_deref() {
            Some(r) if r.contains("dirty") => ui::yellow(r),
            Some(r) => ui::dim(r),
            None => ui::dim(""),
        }
    }
}
```

Then rewrite `render`'s row loop to call them, replacing the inline blocks (`linear_cell` takes `report.has_linear_key`; the `offline` LINEAR dash stays in `render`):

```rust
    let mut t = ui::table(&HEADERS);
    for row in sorted {
        let linear_disp = if offline {
            ui::dim("—")
        } else {
            linear_cell(row, report.has_linear_key)
        };
        t.add_row(vec![
            issue_cell(row, report.linear_workspace.as_deref()),
            branch_cell(&row.branch),
            tree_cell(row.dirty),
            pr_cell(row),
            linear_disp,
            verdict_cell(row, offline),
        ]);
    }
```

Note the original `verdict_disp`/`issue_disp`/`pr_disp`/`tree_disp` blocks and the old headers array are deleted.

- [ ] **Step 4: Run the test suite**

Run: `cargo test --workspace 2>&1 | tail -5`
Expected: all pass (327 existing + 6 new).

- [ ] **Step 5: Commit**

```bash
git add src/bin/issue/triage.rs
git commit -m "refactor(issue): extract triage cell formatters"
```

---

### Task 2: `Prs::empty()` and `dirty_stream` in `devkit-issue`

The live gather needs (a) a constructible empty `Prs` for tests of code that consumes one, and (b) dirty results streamed per path instead of returned as one batch.

**Files:**
- Modify: `crates/devkit-issue/src/status.rs`

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `crates/devkit-issue/src/status.rs`:

```rust
    #[test]
    fn prs_empty_leaves_row_untouched() {
        let mut r = wt("ENG-1", "NO_PR", false, None);
        r.pr_number = None;
        Prs::empty().apply_best(&mut r);
        assert_eq!(r.pr_number, None);
        assert_eq!(r.pr_state, "NO_PR");
    }

    // dirty_stream must report each index exactly once with the same result
    // dirty_many computes. Plain (non-git) dirs make dirty_of return false, so
    // no repo setup is needed to exercise the fan-out and index math.
    #[test]
    fn dirty_stream_reports_every_index_once() {
        use std::sync::Mutex;
        let base = std::env::temp_dir().join(format!("devkit-dstream-{}", std::process::id()));
        let paths: Vec<String> = (0..7)
            .map(|i| {
                let p = base.join(format!("d{i}"));
                std::fs::create_dir_all(&p).unwrap();
                p.to_string_lossy().into_owned()
            })
            .collect();

        let got: Mutex<Vec<Option<bool>>> = Mutex::new(vec![None; paths.len()]);
        dirty_stream(&paths, |i, d| {
            let mut g = got.lock().unwrap();
            assert!(g[i].is_none(), "index {i} reported twice");
            g[i] = Some(d);
        });
        let got = got.into_inner().unwrap();
        let want = dirty_many(&paths);
        assert_eq!(
            got.into_iter().map(|o| o.expect("index missing")).collect::<Vec<_>>(),
            want
        );
        let _ = std::fs::remove_dir_all(&base);
    }
```

Note the closure only needs `&got` — captured by reference, so `Fn + Send + Clone` is satisfied (`&Mutex` is `Copy`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-issue 2>&1 | tail -10`
Expected: compile FAILURE — `Prs::empty` / `dirty_stream` not found.

- [ ] **Step 3: Implement**

In `crates/devkit-issue/src/status.rs`, inside `impl Prs` add:

```rust
    /// An empty PR list, built without any network call. Used when there are
    /// no worktrees and by tests of code that consumes a `Prs`.
    pub fn empty() -> Prs {
        Prs(Vec::new())
    }
```

Change `fetch_prs`'s early return to use it (`return Ok(Prs::empty());`).

Below `dirty_many`, add:

```rust
/// `dirty_of` for many worktrees, reporting each result as soon as it is
/// known. `report(i, dirty)` is invoked exactly once per input index, from
/// worker threads. Same bounded pool and chunking as [`dirty_many`]; callers
/// that want the batch form should keep using `dirty_many`.
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
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p devkit-issue 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-issue/src/status.rs
git commit -m "feat(issue): add Prs::empty and streaming dirty checks"
```

---

### Task 3: `LiveTable` widget in `devkit-common`

**Files:**
- Create: `crates/devkit-common/src/livetable.rs`
- Modify: `crates/devkit-common/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/devkit-common/src/livetable.rs` with only a test module for now (the impl comes in Step 3, but writing tests first pins the API):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn headers() -> Vec<String> {
        ["A", "B"].iter().map(|s| s.to_string()).collect()
    }

    // render_lines is pure: title line + header + one line per row. Off-TTY
    // colour helpers pass through, so content asserts are plain strings.
    #[test]
    fn render_lines_shows_cells_and_frames() {
        let rows = vec![vec![
            Cell::Ready("one".into()),
            Cell::Pending,
        ]];
        let lines = render_lines("TITLE", &headers(), &rows, 2);
        assert_eq!(lines[0], "TITLE");
        let body = lines.join("\n");
        assert!(body.contains("one"), "{body}");
        assert!(body.contains(FRAMES[2]), "{body}");
    }

    #[test]
    fn render_lines_stale_keeps_content() {
        let rows = vec![vec![
            Cell::Stale("old".into()),
            Cell::Ready("new".into()),
        ]];
        let body = render_lines("T", &headers(), &rows, 0).join("\n");
        assert!(body.contains("old"), "{body}");
    }

    #[test]
    fn frame_wraps() {
        let rows = vec![vec![Cell::Pending, Cell::Pending]];
        let a = render_lines("T", &headers(), &rows, 0);
        let b = render_lines("T", &headers(), &rows, FRAMES.len());
        assert_eq!(a, b);
    }

    // Tests never run under a TTY: the whole widget must be hidden so pipes /
    // MCP / CI see no live output — same invariant progress::Steps holds.
    #[test]
    fn hidden_off_tty() {
        let mut ll = LiveLines::new();
        assert!(ll.is_hidden());
        ll.set_lines(&["x".into(), "y".into()]);
        ll.set_lines(&["z".into()]); // shrink must not panic
        assert!(ll.spinner("s").is_hidden());
        assert!(ll.bar("b", 3).is_hidden());
        ll.clear();

        let mut lt = LiveTable::new("T", &["A", "B"], 2);
        lt.set(0, 0, Cell::Ready("v".into()));
        lt.redraw();
        lt.tick();
        lt.finish();
    }
}
```

- [ ] **Step 2: Register the module and verify tests fail**

Add `pub mod livetable;` to `crates/devkit-common/src/lib.rs` (alphabetical: between `linear` and `paths`).

Run: `cargo test -p devkit-common livetable 2>&1 | tail -10`
Expected: compile FAILURE — types not defined.

- [ ] **Step 3: Implement the widget**

Prepend to `crates/devkit-common/src/livetable.rs` (above the test module):

```rust
//! Live-updating stderr blocks. [`LiveLines`] is a rewritable block of lines
//! over one `MultiProgress`, with status spinners/bars appended below it;
//! [`LiveTable`] renders a grid of [`Cell`]s into that block. Both are hidden
//! when stderr is not a terminal — pipes, MCP, and tests see nothing — and the
//! final table is the caller's job (printed to stdout after `finish`).

use crate::ui;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::io::IsTerminal;
use std::time::Duration;

/// Braille spinner frames for pending cells; index with `frame % FRAMES.len()`.
pub const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// One table cell of a [`LiveTable`].
pub enum Cell {
    /// Final content, rendered as-is.
    Ready(String),
    /// Last-run content awaiting refresh, rendered dim.
    Stale(String),
    /// Not yet known, rendered as the current spinner frame.
    Pending,
}

/// A block of stderr lines that can be rewritten wholesale, plus status
/// spinners/bars kept below the block.
pub struct LiveLines {
    mp: MultiProgress,
    lines: Vec<ProgressBar>,
}

impl LiveLines {
    pub fn new() -> LiveLines {
        let mp = if std::io::stderr().is_terminal() {
            MultiProgress::new()
        } else {
            MultiProgress::with_draw_target(ProgressDrawTarget::hidden())
        };
        LiveLines { mp, lines: Vec::new() }
    }

    pub fn is_hidden(&self) -> bool {
        self.mp.is_hidden()
    }

    /// Replace the block's content, growing or shrinking the line count to
    /// match. New lines are inserted above any status spinners/bars.
    pub fn set_lines(&mut self, content: &[String]) {
        while self.lines.len() > content.len() {
            let pb = self.lines.pop().expect("len checked");
            pb.finish_and_clear();
            self.mp.remove(&pb);
        }
        while self.lines.len() < content.len() {
            let pb = self
                .mp
                .insert(self.lines.len(), ProgressBar::new_spinner());
            pb.set_style(ProgressStyle::with_template("{wide_msg}").expect("valid template"));
            self.lines.push(pb);
        }
        for (pb, line) in self.lines.iter().zip(content) {
            pb.set_message(line.clone());
        }
    }

    /// An indeterminate status spinner below the block (style as `Steps`).
    pub fn spinner(&self, msg: &str) -> ProgressBar {
        let pb = self.mp.add(ProgressBar::new_spinner());
        pb.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {wide_msg}")
                .expect("valid spinner template"),
        );
        pb.enable_steady_tick(Duration::from_millis(80));
        pb.set_message(msg.to_string());
        pb
    }

    /// A determinate fill bar below the block (style as `Steps`).
    pub fn bar(&self, msg: &str, len: u64) -> ProgressBar {
        let pb = self.mp.add(ProgressBar::new(len));
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.cyan} {wide_msg} [{bar:20.cyan/dim}] {pos}/{len}",
            )
            .expect("valid bar template")
            .progress_chars("=>-"),
        );
        pb.enable_steady_tick(Duration::from_millis(80));
        pb.set_message(msg.to_string());
        pb
    }

    /// Erase the whole block (lines and status bars) from the terminal.
    pub fn clear(&self) {
        let _ = self.mp.clear();
    }
}

impl Default for LiveLines {
    fn default() -> Self {
        Self::new()
    }
}

/// Render a titled table block to plain lines. Pure — tests snapshot it
/// without a terminal. `frame` picks the glyph for `Pending` cells.
pub fn render_lines(
    title: &str,
    headers: &[String],
    rows: &[Vec<Cell>],
    frame: usize,
) -> Vec<String> {
    let hdrs: Vec<&str> = headers.iter().map(|s| s.as_str()).collect();
    let mut t = ui::table(&hdrs);
    for row in rows {
        t.add_row(row.iter().map(|c| match c {
            Cell::Ready(s) => s.clone(),
            Cell::Stale(s) => ui::dim(s),
            Cell::Pending => ui::cyan(FRAMES[frame % FRAMES.len()]),
        }));
    }
    let mut out = vec![ui::bold_cyan(title)];
    out.extend(t.to_string().lines().map(String::from));
    out
}

/// A live-updating table: a grid of [`Cell`]s rendered into a [`LiveLines`]
/// block on every `redraw`/`tick`.
pub struct LiveTable {
    lines: LiveLines,
    title: String,
    headers: Vec<String>,
    rows: Vec<Vec<Cell>>,
    frame: usize,
}

impl LiveTable {
    /// `nrows` rows of all-`Pending` cells under `headers`.
    pub fn new(title: &str, headers: &[&str], nrows: usize) -> LiveTable {
        let cols = headers.len();
        LiveTable {
            lines: LiveLines::new(),
            title: title.to_string(),
            headers: headers.iter().map(|s| s.to_string()).collect(),
            rows: (0..nrows)
                .map(|_| (0..cols).map(|_| Cell::Pending).collect())
                .collect(),
            frame: 0,
        }
    }

    pub fn set(&mut self, row: usize, col: usize, cell: Cell) {
        self.rows[row][col] = cell;
    }

    /// Re-render the grid into the live block. Cheap no-op off-TTY.
    pub fn redraw(&mut self) {
        if self.lines.is_hidden() {
            return;
        }
        let rendered = render_lines(&self.title, &self.headers, &self.rows, self.frame);
        self.lines.set_lines(&rendered);
    }

    /// Advance the spinner frame and redraw. Drive on a ~100ms cadence (e.g.
    /// from a `recv_timeout` loop) while any cell is `Pending`.
    pub fn tick(&mut self) {
        self.frame = (self.frame + 1) % FRAMES.len();
        self.redraw();
    }

    pub fn spinner(&self, msg: &str) -> ProgressBar {
        self.lines.spinner(msg)
    }

    pub fn bar(&self, msg: &str, len: u64) -> ProgressBar {
        self.lines.bar(msg, len)
    }

    /// Erase the live block; the caller prints the final table to stdout.
    pub fn finish(self) {
        self.lines.clear();
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p devkit-common livetable 2>&1 | tail -5`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-common/src/livetable.rs crates/devkit-common/src/lib.rs
git commit -m "feat(common): add LiveLines and LiveTable stderr widgets"
```

---

### Task 4: Event-driven live gather for `issue` (status)

Replace the join-then-render `gather_with_bars` with a channel loop that fills the live table per update. The pure update machine (`LiveState`) is separated from the render loop so arrival-order behavior is unit-testable.

**Files:**
- Modify: `src/bin/issue/status.rs` (full rewrite of the module body)

- [ ] **Step 1: Confirm `gather_with_bars` has no other callers**

Run: `rg -n "gather_with_bars" src/ crates/`
Expected: only `src/bin/issue/status.rs`. If anything else appears, it keeps working through `st::gather` — stop and reassess before deleting.

- [ ] **Step 2: Write the failing tests**

Append to `src/bin/issue/status.rs` (new test module):

```rust
#[cfg(test)]
mod tests {
    use super::*;
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
    fn display_order_sorts_by_issue_id() {
        let rows = vec![row("ENG-2"), row("ENG-1"), row("ENG-3")];
        // discovery index -> display row
        assert_eq!(display_order(&rows), vec![1, 0, 2]);
    }

    // Verdict cells appear only once a row has all three inputs, regardless of
    // arrival order; earlier sources fill their own column immediately.
    #[test]
    fn verdicts_wait_for_all_sources() {
        let mut st = LiveState::new(vec![row("ENG-1"), row("ENG-2")], false);

        let w1 = st.apply_dirty(0, true);
        assert!(w1.iter().any(|(r, c, _)| (*r, *c) == (0, COL_TREE)));
        assert!(!w1.iter().any(|(_, c, _)| *c == COL_VERDICT));

        let w2 = st.apply_prs(devkit_issue::status::Prs::empty());
        assert!(w2.iter().any(|(r, c, _)| (*r, *c) == (1, COL_PR)));
        assert!(!w2.iter().any(|(_, c, _)| *c == COL_VERDICT));

        let w3 = st.apply_linear(std::collections::HashMap::new(), None);
        // Linear was the last input for row 0 (dirty done); row 1's dirty is
        // still missing, so only row 0 gains a verdict.
        assert!(w3.iter().any(|(r, c, _)| (*r, *c) == (0, COL_VERDICT)));
        assert!(!w3.iter().any(|(r, c, _)| (*r, *c) == (1, COL_VERDICT)));
        assert!(!st.done());

        let w4 = st.apply_dirty(1, false);
        assert!(w4.iter().any(|(r, c, _)| (*r, *c) == (1, COL_VERDICT)));
        assert!(st.done());
    }

    #[test]
    fn collected_parts_match_inputs() {
        let mut st = LiveState::new(vec![row("ENG-1")], true);
        st.apply_dirty(0, true);
        st.apply_prs(devkit_issue::status::Prs::empty());
        st.apply_linear(std::collections::HashMap::new(), Some("acme".into()));
        let (dirty, _prs, _linear, ws) = st.into_parts();
        assert_eq!(dirty, vec![true]);
        assert_eq!(ws.as_deref(), Some("acme"));
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p devkit --bin issue status:: 2>&1 | tail -10`
Expected: compile FAILURE — `LiveState`, `display_order`, `COL_*` not defined.

- [ ] **Step 4: Implement `LiveState` and the live gather**

Replace the whole of `src/bin/issue/status.rs` above the new test module with:

```rust
use crate::triage::{self, render};
use anyhow::Result;
use devkit_common::livetable::{Cell, LiveTable};
use devkit_common::{linear, ui};
use devkit_issue::status::{self as st, IssueWorktree, StatusReport};
use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

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

/// Pure accumulator for the live gather. Each `apply_*` overlays one source
/// onto the rows and returns cell writes as `(discovery_index, column,
/// content)`; a row's VERDICT write is emitted only once all three sources
/// have reported for it. Keeps raw results so the caller can feed the same
/// `assemble` the silent gather uses — the stdout table stays byte-identical.
struct LiveState {
    rows: Vec<IssueWorktree>,
    dirty: Vec<bool>,
    dirty_seen: Vec<bool>,
    prs: Option<st::Prs>,
    linear: Option<HashMap<String, linear::LinearState>>,
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
        states: HashMap<String, linear::LinearState>,
        workspace: Option<String>,
    ) -> Vec<(usize, usize, String)> {
        let mut out = Vec::new();
        for i in 0..self.rows.len() {
            if let Some(s) = states.get(&self.rows[i].issue_id) {
                self.rows[i].linear_kind = Some(s.kind.clone());
                self.rows[i].linear_name = Some(s.name.clone());
            }
            out.push((i, COL_LINEAR, triage::linear_cell(&self.rows[i], self.has_key)));
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
    fn into_parts(
        self,
    ) -> (
        Vec<bool>,
        st::Prs,
        HashMap<String, linear::LinearState>,
        Option<String>,
    ) {
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
    Linear(HashMap<String, linear::LinearState>, Option<String>),
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
        let prs = st::fetch_prs(&d)?;
        return Ok(st::assemble(d, Vec::new(), prs, HashMap::new(), None, has_key));
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
    let dirty_bar = lt.bar(&format!("Checking {m} worktrees"), m as u64);
    let prs_spin = lt.spinner("Fetching PRs from GitHub…");
    let linear_spin = lt.spinner("Fetching Linear states…");

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

        while !state.done() {
            let writes = match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Update::Dirty(i, dirty)) => {
                    dirty_bar.inc(1);
                    state.apply_dirty(i, dirty)
                }
                Ok(Update::Prs(res)) => {
                    prs_spin.finish_and_clear();
                    state.apply_prs(res?)
                }
                Ok(Update::Linear(states, ws)) => {
                    linear_spin.finish_and_clear();
                    state.apply_linear(states, ws)
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    lt.tick();
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            for (i, col, content) in writes {
                lt.set(disp[i], col, Cell::Ready(content));
            }
            lt.redraw();
        }
        Ok(())
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
```

The old `gather_with_bars` is deleted. Note `Update::Prs(Err)` propagates via `res?` inside the scope — the scope joins its threads, then `lt.finish()` runs before `looped?` re-raises.

- [ ] **Step 5: Run tests + clippy**

Run: `cargo test --workspace 2>&1 | tail -5 && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3`
Expected: PASS, no warnings.

- [ ] **Step 6: Verify by eye**

Run in a real worktree-bearing repo (e.g. the monorepo): `cargo run --bin issue -- --timing`
Expected: table skeleton appears immediately with spinner cells; TREE cells pop in individually; PR/LINEAR columns fill; verdicts appear last; final table prints identically to before. Piped (`cargo run --bin issue | cat`): no animation, same output as before the change.

- [ ] **Step 7: Commit**

```bash
git add src/bin/issue/status.rs
git commit -m "feat(issue): live-fill the status triage table"
```

---

### Task 5: Parallel + live `issue info`

**Files:**
- Modify: `src/bin/issue/info.rs:65-121` (the fetch/orchestration section of `run`)

- [ ] **Step 1: Replace the serial live branch**

In `src/bin/issue/info.rs`, replace the `} else if discovered {` branch of `run` (currently three serial `steps.during` calls) with a concurrent fetch filling a one-row live table. Also delete the `let steps = Steps::new();` line and the `use devkit_common::progress::Steps;` import (the cache-only and main-clone branches get a local `Steps` where still needed — see below). New branch body:

```rust
    } else if discovered {
        // Live: one `gh pr list`, a single-id Linear lookup, and the workspace
        // key — all concurrent, filling a one-row live table as each lands.
        use devkit_common::livetable::{Cell, LiveTable};
        use std::sync::mpsc;
        use std::time::Duration;

        let mut lt = LiveTable::new("ISSUE WORKTREES", &crate::triage::HEADERS, 1);
        lt.set(0, 0, Cell::Ready(crate::triage::issue_cell(&row, None)));
        lt.set(0, 1, Cell::Ready(crate::triage::branch_cell(&row.branch)));
        lt.set(0, 2, Cell::Ready(crate::triage::tree_cell(row.dirty)));
        lt.redraw();

        enum Up {
            Prs(anyhow::Result<st::Prs>),
            Linear(std::collections::HashMap<String, devkit_common::linear::LinearState>),
            Workspace(Option<String>),
        }
        let want_linear = row.issue_id != "UNKNOWN";
        let looped: anyhow::Result<()> = std::thread::scope(|s| {
            let (tx, rx) = mpsc::channel::<Up>();
            {
                let tx = tx.clone();
                let d = &d;
                s.spawn(move || {
                    let _ = tx.send(Up::Prs(st::fetch_prs(d)));
                });
            }
            if want_linear {
                let tx = tx.clone();
                let id = row.issue_id.clone();
                s.spawn(move || {
                    let states = devkit_common::linear::states(
                        std::slice::from_ref(&id),
                        devkit_common::secrets::resolve("LINEAR_API_KEY").as_deref(),
                    );
                    let _ = tx.send(Up::Linear(states));
                });
            }
            {
                let tx = tx.clone();
                s.spawn(move || {
                    let _ = tx.send(Up::Workspace(devkit_common::linear::workspace_url_key()));
                });
            }
            drop(tx);

            let mut got_prs = false;
            let mut got_linear = !want_linear;
            let mut got_ws = false;
            while !(got_prs && got_linear && got_ws) {
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(Up::Prs(res)) => {
                        res?.apply_best(&mut row);
                        got_prs = true;
                        lt.set(0, 3, Cell::Ready(crate::triage::pr_cell(&row)));
                    }
                    Ok(Up::Linear(states)) => {
                        if let Some(s) = states.get(&row.issue_id) {
                            row.linear_kind = Some(s.kind.clone());
                            row.linear_name = Some(s.name.clone());
                        }
                        got_linear = true;
                        lt.set(0, 4, Cell::Ready(crate::triage::linear_cell(&row, has_key)));
                    }
                    Ok(Up::Workspace(ws)) => {
                        got_ws = true;
                        linear_workspace = ws;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        lt.tick();
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
                if got_prs && got_linear {
                    let reason = st::reason_not_finished(&row, has_key, false);
                    row.finished = reason.is_none();
                    row.reason_not_finished = reason;
                    lt.set(0, 5, Cell::Ready(crate::triage::verdict_cell(&row, false)));
                }
                lt.redraw();
            }
            Ok(())
        });
        lt.finish();
        looped?;

        if let (Some(number), Some(url)) = (row.pr_number, row.pr_url.clone()) {
            // pr_number and pr_url are set together, so both-Some is the normal
            // PR case; a PR-less row simply leaves the cache untouched.
            let _ = crate::info_cache::write(
                Path::new(&row.worktree),
                &crate::info_cache::CachedPr {
                    number,
                    state: row.pr_state.clone(),
                    url,
                },
            );
        }
    } else {
        // Live, but the target is the main clone (no associated PR/Linear): only
        // the workspace link is worth resolving for rendering.
        let steps = devkit_common::progress::Steps::new();
        linear_workspace = steps.during(
            "Resolving Linear workspace…",
            devkit_common::linear::workspace_url_key,
        );
    }
```

Note the linear-status fetch in the old code re-resolved the key per call; here `states` is called the same way, keeping behavior. The verdict block runs after every update once PR+Linear are in (idempotent — recomputes the same value).

- [ ] **Step 2: Run tests + clippy**

Run: `cargo test --workspace 2>&1 | tail -5 && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3`
Expected: PASS. (The `info` tests are all pure helpers — unaffected.)

- [ ] **Step 3: Verify by eye**

In an issue worktree: `cargo run --bin issue -- info --timing`
Expected: one-row table appears instantly with TREE filled; PR and LINEAR pop in; wall time drops ~1s vs. before (fetches overlap). `--json` and `--cache-only` unchanged.

- [ ] **Step 4: Commit**

```bash
git add src/bin/issue/info.rs
git commit -m "feat(issue): parallel fetches and live table for info"
```

---

### Task 6: `prs` snapshot cache (full rows + diff)

**Files:**
- Modify: `crates/devkit-issue/src/prs.rs:595-611` (view derives)
- Modify: `src/bin/issue/prs.rs` (cache section)

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `src/bin/issue/prs.rs`:

```rust
    #[test]
    fn snapshot_round_trips() {
        let snap = Snapshot {
            mine: vec![devkit_issue::prs::MinePrView {
                number: 1,
                url: "https://x/1".into(),
                issue_id: "ENG-1".into(),
                review_state: "approved".into(),
                check_state: "ok".into(),
                action: "MERGE".into(),
            }],
            reviews: vec![],
            diff: Snap::new(),
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.mine.len(), 1);
        assert_eq!(back.mine[0].action, "MERGE");
    }

    // The pre-snapshot cache format (top-level {"mine": {"12": {...}}} maps)
    // must read as an empty snapshot, not an error — first run after upgrade
    // simply has no stale table.
    #[test]
    fn old_format_reads_as_empty() {
        let old = r#"{"mine":{"12":{"review":"approved","check":"ok","action":"MERGE"}}}"#;
        let dir = std::env::temp_dir().join(format!("devkit-prs-snap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("repo.json");
        std::fs::write(&p, old).unwrap();
        let snap = load_snapshot(&p);
        assert!(snap.mine.is_empty() && snap.reviews.is_empty() && snap.diff.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit --bin issue prs:: 2>&1 | tail -10`
Expected: compile FAILURE — `Snapshot`/`load_snapshot` not defined (and `MinePrView` lacks `Deserialize`).

- [ ] **Step 3: Implement**

In `crates/devkit-issue/src/prs.rs`, change the two view derives (lines ~594 and ~604):

```rust
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct MinePrView {
```

```rust
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ReviewPrView {
```

In `src/bin/issue/prs.rs`, replace the `// diff cache` section (`Snap`/`cache_path`/`load_cache`/`save_cache`) with:

```rust
// snapshot cache -----------------------------------------------------------------
// One file per repo: the previous run's full rows (for the stale-while-
// revalidate render) plus the per-PR diff values (for `old → new` cells).

type Snap = BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>;

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Snapshot {
    #[serde(default)]
    mine: Vec<MinePrView>,
    #[serde(default)]
    reviews: Vec<ReviewPrView>,
    #[serde(default)]
    diff: Snap,
}

fn cache_path(repo: &str) -> PathBuf {
    paths::cache_dir()
        .join("pr-status")
        .join(format!("{}.json", repo.replace('/', "_")))
}

/// Read the snapshot; any parse failure (including the pre-snapshot cache
/// format) reads as an empty snapshot rather than an error.
fn load_snapshot(path: &Path) -> Snapshot {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_snapshot(path: &Path, snap: &Snapshot) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(snap)?)?;
    Ok(())
}
```

In `run`, adjust the cache plumbing (the full `run` rewrite lands in Task 7; for now keep the current structure compiling): replace `let mut cache: Snap = path.as_deref().map(load_cache).unwrap_or_default();` with

```rust
    let mut snap: Snapshot = path.as_deref().map(load_snapshot).unwrap_or_default();
    let mut cache: Snap = std::mem::take(&mut snap.diff);
```

and replace the trailing save with

```rust
    if let Some(p) = &path {
        save_snapshot(
            p,
            &Snapshot {
                mine: report.mine.clone(),
                reviews: report.reviews.clone(),
                diff: cache,
            },
        )?;
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test --workspace 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-issue/src/prs.rs src/bin/issue/prs.rs
git commit -m "feat(issue): store full pr rows in the prs snapshot cache"
```

---

### Task 7: `prs` stale-while-revalidate render

**Files:**
- Modify: `src/bin/issue/prs.rs` (table builders + `run`)

- [ ] **Step 1: Split the table builders from printing**

`mine_table`/`reviews_table` currently print. Split each into a builder returning the rendered string plus the diff map, and a printing wrapper — the stale path reuses the builders with an empty `prev`. Replace both functions with:

```rust
fn mine_table_build(
    prs: &[MinePrView],
    url_key: Option<&str>,
    prev: &BTreeMap<String, BTreeMap<String, String>>,
) -> (String, BTreeMap<String, BTreeMap<String, String>>) {
    let mut cur = BTreeMap::new();
    if prs.is_empty() {
        return (format!("  {}", ui::dim("(none)")), cur);
    }
    let mut t = ui::table(&["PR", "ISSUE", "REVIEW", "CHECK", "ACTION"]);
    for pr in prs {
        let review = pr.review_state.clone();
        let check = pr.check_state.clone();
        let action = pr.action.clone();
        let was = prev.get(&pr.number.to_string());
        let g = |k: &str| was.and_then(|m| m.get(k)).map(|s| s.as_str());
        t.add_row(vec![
            ui::link(&format!("#{}", pr.number), &pr.url),
            issue_cell(&pr.issue_id, url_key),
            diff_cell(g("review"), &review, |s| s.to_string()),
            diff_cell(g("check"), &check, |s| s.to_string()),
            diff_cell(g("action"), &action, |s| paint_action(&action, s)),
        ]);
        cur.insert(
            pr.number.to_string(),
            BTreeMap::from([
                ("review".to_string(), review),
                ("check".to_string(), check),
                ("action".to_string(), action),
            ]),
        );
    }
    (t.to_string(), cur)
}

fn mine_table(
    prs: &[MinePrView],
    url_key: Option<&str>,
    prev: &BTreeMap<String, BTreeMap<String, String>>,
) -> BTreeMap<String, BTreeMap<String, String>> {
    println!("{}", ui::bold_cyan("MY OPEN PRs"));
    let (body, cur) = mine_table_build(prs, url_key, prev);
    println!("{body}");
    cur
}
```

and the same shape for `reviews_table_build`/`reviews_table` (headers `["PR", "AUTHOR", "MY VOTE", "ACTION"]`, keys `vote`/`action`, title `PRs AWAITING MY REVIEW` printed with the leading `\n` as today).

- [ ] **Step 2: Add the stale-lines renderer**

```rust
/// The stale-while-revalidate block: last run's tables, every line dimmed,
/// under an animated "refreshing" banner. Pure — `frame` picks the glyph.
fn stale_lines(snap: &Snapshot, want_mine: bool, want_reviews: bool, frame: usize) -> Vec<String> {
    use devkit_common::livetable::FRAMES;
    let mut out = vec![format!(
        "{} {}",
        ui::cyan(FRAMES[frame % FRAMES.len()]),
        ui::dim("as of last run — refreshing…"),
    )];
    let empty = BTreeMap::new();
    if want_mine {
        out.push(ui::dim(&ui::bold_cyan("MY OPEN PRs")));
        let (body, _) = mine_table_build(&snap.mine, None, &empty);
        out.extend(body.lines().map(ui::dim));
    }
    if want_reviews {
        out.push(ui::dim(&ui::bold_cyan("PRs AWAITING MY REVIEW")));
        let (body, _) = reviews_table_build(&snap.reviews, &empty);
        out.extend(body.lines().map(ui::dim));
    }
    out
}
```

- [ ] **Step 3: Write tests for the builders**

Append to the `tests` module:

```rust
    fn mine_view(n: u64, action: &str) -> MinePrView {
        MinePrView {
            number: n,
            url: format!("https://x/{n}"),
            issue_id: "-".into(),
            review_state: "none".into(),
            check_state: "ok".into(),
            action: action.into(),
        }
    }

    #[test]
    fn mine_table_build_renders_and_collects() {
        let (body, cur) = mine_table_build(&[mine_view(12, "MERGE")], None, &BTreeMap::new());
        assert!(body.contains("#12"), "{body}");
        assert!(body.contains("MERGE"), "{body}");
        assert_eq!(cur["12"]["action"], "MERGE");
    }

    #[test]
    fn stale_lines_have_banner_and_rows() {
        let snap = Snapshot {
            mine: vec![mine_view(12, "MERGE")],
            reviews: vec![],
            diff: Snap::new(),
        };
        let lines = stale_lines(&snap, true, true, 0);
        assert!(lines[0].contains("as of last run"), "{:?}", lines[0]);
        assert!(lines.iter().any(|l| l.contains("#12")));
        assert!(lines.iter().any(|l| l.contains("(none)"))); // empty reviews
    }
```

Run: `cargo test -p devkit --bin issue prs:: 2>&1 | tail -5`
Expected: PASS (builders already implemented in Steps 1–2; if you prefer strict TDD, write this step's tests before Step 1 — the content is identical).

- [ ] **Step 4: Rewire `run` for SWR**

Replace the middle of `run` (from `let steps = devkit_common::progress::Steps::new();` through `steps.clear();` plus the cache-path lines) with:

```rust
    // Resolve the repo up front: the snapshot cache is keyed by it and the
    // stale table must render before the fetch starts.
    let resolved = devkit_issue::prs::resolve_repo(repo.as_deref(), ".")?;
    let repo_key = if no_cache { None } else { Some(resolved.clone()) };
    let path = repo_key.as_ref().map(|r| cache_path(r));
    let mut snap: Snapshot = path.as_deref().map(load_snapshot).unwrap_or_default();
    let cache_prev: Snap = std::mem::take(&mut snap.diff);

    // Stale-while-revalidate: last run's rows render immediately, dimmed under
    // a refreshing banner, and are cleared when fresh data lands. With no
    // usable snapshot, plain fetch spinners show instead.
    let mut live = devkit_common::livetable::LiveLines::new();
    let have_stale = !snap.mine.is_empty() || !snap.reviews.is_empty();
    let mut frame = 0usize;
    if have_stale {
        live.set_lines(&stale_lines(&snap, want_mine, want_reviews, frame));
    }
    let fetch_spin = (!have_stale).then(|| live.spinner("Fetching PRs from GitHub…"));

    enum Up {
        Fetched(Result<devkit_issue::prs::PrsReport>),
        Workspace(Option<String>),
    }
    let mut url_key: Option<String> = None;
    let mut report: Option<devkit_issue::prs::PrsReport> = None;
    let looped: Result<()> = std::thread::scope(|s| {
        use std::sync::mpsc;
        use std::time::Duration;
        let (tx, rx) = mpsc::channel::<Up>();
        {
            let tx = tx.clone();
            s.spawn(move || {
                let _ = tx.send(Up::Workspace(devkit_common::linear::workspace_url_key()));
            });
        }
        {
            let tx = tx.clone();
            let resolved = &resolved;
            let ignored_checks = &ignored_checks;
            s.spawn(move || {
                let _ = tx.send(Up::Fetched(devkit_issue::prs::gather(
                    ".",
                    mine,
                    reviews,
                    Some(resolved),
                    ignored_checks,
                )));
            });
        }
        drop(tx);
        while url_key.is_none() || report.is_none() {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Up::Fetched(res)) => report = Some(res?),
                Ok(Up::Workspace(ws)) => url_key = ws,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if have_stale {
                        frame = (frame + 1) % devkit_common::livetable::FRAMES.len();
                        live.set_lines(&stale_lines(&snap, want_mine, want_reviews, frame));
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        Ok(())
    });
    drop(fetch_spin);
    live.clear();
    looped?;
    let report = report.expect("fetch completed");
```

`url_key` loop-exit note: `workspace_url_key` returning `None` (no key configured) would loop forever with `url_key.is_none()` as the condition — use explicit `got_ws` flag instead:

```rust
        let mut got_ws = false;
        while !got_ws || report.is_none() {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Up::Fetched(res)) => report = Some(res?),
                Ok(Up::Workspace(ws)) => {
                    got_ws = true;
                    url_key = ws;
                }
                ...
```

(Use the `got_ws` version; the first snippet is shown to explain why.)

Then the existing rendering block follows unchanged (`if want_mine { … mine_table(&report.mine, url_key.as_deref(), &prev) … }`), with `prev` maps read from `cache_prev` instead of `cache` (`let prev = cache_prev.get("mine").cloned().unwrap_or_default();`), the fresh maps inserted into a new `let mut diff_cur: Snap = Snap::new();`, and the save from Task 6 writing `diff: diff_cur`.

- [ ] **Step 5: Run tests + clippy**

Run: `cargo test --workspace 2>&1 | tail -5 && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3`
Expected: PASS.

- [ ] **Step 6: Verify by eye**

In the monorepo: run `cargo run --bin issue -- prs` twice. First run: spinner only (old cache format reads as empty), fresh tables print, snapshot saved. Second run: dimmed previous tables + banner appear instantly, replaced ~5s later by fresh tables with `old → new` cells where things changed. `--no-cache`: no stale table. Piped: no animation.

- [ ] **Step 7: Commit**

```bash
git add src/bin/issue/prs.rs
git commit -m "feat(issue): stale-while-revalidate render for prs"
```

---

### Task 8: Persistent step-log mode on `Steps`

**Files:**
- Modify: `crates/devkit-common/src/progress.rs`

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `crates/devkit-common/src/progress.rs`:

```rust
    #[test]
    fn persistent_mode_numbers_steps() {
        let steps = Steps::persistent();
        assert_eq!(steps.label("a"), "1. a");
        assert_eq!(steps.label("b"), "2. b");
        let steps = Steps::persistent_with_total(2);
        assert_eq!(steps.label("a"), "[1/2] a");
    }

    #[test]
    fn finish_line_marks_ok_and_err() {
        // Off-TTY colours pass through, so the glyph and text are plain.
        let d = Duration::from_millis(312);
        assert_eq!(finish_line(true, "1. foo", d), "✓ 1. foo (312ms)");
        assert_eq!(finish_line(false, "2. bar", d), "✗ 2. bar (312ms)");
    }

    #[test]
    fn fmt_elapsed_units() {
        assert_eq!(fmt_elapsed(Duration::from_millis(312)), "312ms");
        assert_eq!(fmt_elapsed(Duration::from_millis(1200)), "1.2s");
    }

    #[test]
    fn during_result_passes_values_and_errors() {
        let steps = Steps::persistent();
        assert_eq!(steps.during_result("ok", || Ok(41 + 1)).unwrap(), 42);
        // Turbofish is unavailable next to impl-Trait params; annotate the
        // closure's return type instead.
        let failing = || -> anyhow::Result<()> { Err(anyhow::anyhow!("boom")) };
        assert!(steps.during_result("fail", failing).is_err());
    }

    #[test]
    fn persistent_bars_hidden_off_tty() {
        let steps = Steps::persistent();
        assert!(steps.spinner("working…").is_hidden());
        steps.during("quiet", || ());
        steps.clear();
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-common progress 2>&1 | tail -10`
Expected: compile FAILURE.

- [ ] **Step 3: Implement**

In `crates/devkit-common/src/progress.rs`:

Add the field and constructors (`new`/`with_total` set `persist: false`):

```rust
pub struct Steps {
    mp: MultiProgress,
    total: Option<usize>,
    n: Cell<usize>,
    persist: bool,
}
```

```rust
    /// Persistent step-log mode: each completed [`Steps::during`] step stays on
    /// screen as a numbered `✓ n. msg (elapsed)` line instead of clearing, so
    /// a multi-step command leaves a record of what it did.
    pub fn persistent() -> Steps {
        Steps { mp: Self::target(), total: None, n: Cell::new(0), persist: true }
    }

    /// Persistent mode with `[i/total]` numbering.
    pub fn persistent_with_total(total: usize) -> Steps {
        Steps { mp: Self::target(), total: Some(total), n: Cell::new(0), persist: true }
    }
```

Extend `label` — persistent unnumbered mode gets a plain ordinal:

```rust
    fn label(&self, msg: &str) -> String {
        let i = self.n.get() + 1;
        match self.total {
            Some(total) => {
                self.n.set(i);
                format!("[{i}/{total}] {msg}")
            }
            None if self.persist => {
                self.n.set(i);
                format!("{i}. {msg}")
            }
            None => msg.to_string(),
        }
    }
```

Add the pure formatting helpers (module level, above `impl Steps`):

```rust
/// `312ms` under a second, `1.2s` from there up.
fn fmt_elapsed(d: Duration) -> String {
    if d < Duration::from_secs(1) {
        format!("{}ms", d.as_millis())
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

/// The persistent line a settled step leaves behind.
fn finish_line(ok: bool, label: &str, elapsed: Duration) -> String {
    let mark = if ok {
        crate::ui::green("✓")
    } else {
        crate::ui::red("✗")
    };
    format!("{mark} {label} ({})", fmt_elapsed(elapsed))
}
```

Rework `during` and add `during_result` + the shared settle:

```rust
    /// Run `f` under a spinner (auto-numbered in numbered mode). Transient
    /// mode clears the bar before returning — so the spinner never stays live
    /// across a `?`, a stdin prompt, or stdout output. Persistent mode prints
    /// the settled `✓` line into scrollback instead; the bar itself is still
    /// cleared, so no bar is ever active across a prompt in either mode.
    pub fn during<T>(&self, msg: &str, f: impl FnOnce() -> T) -> T {
        let label = self.label(msg);
        let pb = self.spinner(&label);
        let out = f();
        self.settle(&pb, &label, true);
        out
    }

    /// [`Steps::during`] for fallible steps: in persistent mode the settled
    /// line is `✗` when the closure errors, so the failed step stays
    /// identifiable in the log.
    pub fn during_result<T>(
        &self,
        msg: &str,
        f: impl FnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let label = self.label(msg);
        let pb = self.spinner(&label);
        let out = f();
        self.settle(&pb, &label, out.is_ok());
        out
    }

    /// End a step's bar: persistent mode leaves a `✓/✗` line in scrollback
    /// (via the group's println, discarded when hidden), transient mode just
    /// clears. The bar is always cleared so prompts and stdout stay clean.
    fn settle(&self, pb: &ProgressBar, label: &str, ok: bool) {
        let line = self.persist.then(|| finish_line(ok, label, pb.elapsed()));
        pb.finish_and_clear();
        if let Some(line) = line {
            let _ = self.mp.println(line);
        }
    }
```

Add `use std::time::Duration;` is already imported. `anyhow` is already a `devkit-common` dependency (`cmd` uses it).

- [ ] **Step 4: Run tests + clippy**

Run: `cargo test -p devkit-common 2>&1 | tail -5 && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-common/src/progress.rs
git commit -m "feat(common): persistent step-log mode for Steps"
```

---

### Task 9: Adopt persistent steps in `checkout-pr`, `setup`, `review`, `end`

**Files:**
- Modify: `src/bin/issue/checkout.rs` (constructor at :310; `during` calls at :189, :203, :314, :345, :348, :367, :401)
- Modify: `src/bin/issue/setup.rs` (constructor at :159; `during` at :160, :171, :207)
- Modify: `src/bin/issue/end.rs` (constructor at :135; `during` at :141, :154, :186)
- Modify: `src/bin/issue/review/request.rs` (constructor at :181; `during` at :184, :222, :228, :237, :299)
- Modify: `src/bin/issue/review/finish.rs` (constructor at :107; `during` at :114, :126)
- Modify: `src/bin/issue/review/mod.rs` (`during` at :198)

(Line numbers are as of the plan date — re-locate with `rg -n "Steps::(new|with_total)|\.during\(" src/bin/issue/` if drifted.)

- [ ] **Step 1: Swap constructors**

- `Steps::new()` → `Steps::persistent()` in `checkout.rs:310`, `end.rs:135`, `review/request.rs:181`, `review/finish.rs:107`.
- `Steps::with_total(total)` → `Steps::persistent_with_total(total)` in `setup.rs:159`.

Do **not** touch `Steps::new()` in `info.rs` (its remaining use is a transient workspace spinner) or anywhere outside these step-driven flows.

- [ ] **Step 2: Route fallible steps through `during_result`**

For each `during` call site listed above, check the closure's return type: if it returns `anyhow::Result<_>` (the call is followed by `?`, `.context(…)?`, or is matched as a `Result`), change `steps.during(…)` to `steps.during_result(…)`. If the closure returns a plain value (e.g. `linear::states`, which returns a `HashMap`), leave it as `during`. The compiler does not force this change — check each site by hand; a `Result`-returning step left on `during` merely always settles `✓`, which would misreport failed steps.

Known-fallible from current code: `checkout.rs:314/:345/:348/:367/:401`, `end.rs:141/:154/:186` (the `:186` closure is matched as a `Result` — still `during_result`), `request.rs:184/:222/:228/:237/:299`, `finish.rs:114/:126`. Inspect `checkout.rs:189/:203` and `mod.rs:198` and convert only if their closures return `Result`.

- [ ] **Step 3: Run tests + clippy**

Run: `cargo test --workspace 2>&1 | tail -5 && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3`
Expected: PASS.

- [ ] **Step 4: Verify by eye**

Run `cargo run --bin issue -- setup <some-issue>` (or `checkout-pr` on a real PR) in a scratch repo.
Expected: completed steps accumulate as `✓ [1/3] Fetching from origin… (1.4s)` lines instead of one overwritten spinner; prompts and stdout messages interleave cleanly between steps; a failing step shows `✗`. Piped: silent as before.

- [ ] **Step 5: Commit**

```bash
git add src/bin/issue/checkout.rs src/bin/issue/setup.rs src/bin/issue/end.rs src/bin/issue/review/
git commit -m "feat(issue): persistent step log in step-driven commands"
```

---

### Task 10: Full gate, docs, and wrap-up

**Files:**
- Modify: `README.md` (issue section)

- [ ] **Step 1: Full verification**

```bash
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: clean. Then re-run the eye checks from Tasks 4/5/7/9 once each in the monorepo (`issue`, `issue info`, `issue prs` twice, `issue setup`), plus one piped run of each to confirm stdout is unchanged and silent of animation.

- [ ] **Step 2: Document the behavior**

In `README.md`'s `issue` section, add a short paragraph: status/info draw the table immediately and fill cells as GitHub/Linear/git data lands; `prs` shows the previous run's table dimmed while refreshing; step commands keep a numbered log of completed steps. All live output is stderr-only and TTY-gated — piped output is unchanged.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: describe live table and step-log rendering"
```

---

## Self-review notes

- **Spec coverage:** D1 → Tasks 4 (status), 5 (info), 6+7 (prs SWR, dim stale via `stale_lines`/`Cell::Stale`); D2 → Task 3 (`FRAMES`, `tick`) driven by the `recv_timeout` loops in Tasks 4/5/7; D3 → every live path clears then renders via the untouched stdout printers, checked by existing tests and the piped eye-checks; D4 → Tasks 8+9. Formatter extraction and `dirty_stream` are the spec's phases 1 and 3 prerequisites (Tasks 1–2).
- **Cell::Stale note:** the `prs` SWR path renders stale content through `stale_lines` + `LiveLines` (whole-block swap) rather than per-cell `Cell::Stale`; the variant still exists for `LiveTable` users and is tested, but no current caller mixes stale and pending cells in one grid.
- **Type consistency:** `LiveState.apply_*` all return `Vec<(usize, usize, String)>` (discovery index, column, content); `disp` maps to display rows only at the `lt.set` boundary. `triage::HEADERS` is the single header source for `render`, status, and info.
