//! Live-updating stderr blocks. [`LiveLines`] is a rewritable block of lines
//! over one `MultiProgress`, with status spinners/bars appended below it;
//! [`LiveTable`] renders a grid of [`Cell`]s into that block. Both are hidden
//! when stderr is not a terminal — pipes, MCP, and tests see nothing — and the
//! final table is the caller's job (printed to stdout after `finish`).

use crate::progress::{add_bar, add_spinner, tty_multi};
use crate::ui;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle, WeakProgressBar};
use std::cell::RefCell;

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
    /// Weak refs to handed-out status bars, so `clear` can finish them —
    /// a steady-tick bar left unfinished would repaint after the clear.
    status: RefCell<Vec<WeakProgressBar>>,
}

impl LiveLines {
    pub fn new() -> LiveLines {
        Self::over(tty_multi())
    }

    /// A block that never draws, even on a TTY — for callers that run the same
    /// update loop but must not render (e.g. machine-readable output modes).
    pub fn hidden() -> LiveLines {
        Self::over(MultiProgress::with_draw_target(
            indicatif::ProgressDrawTarget::hidden(),
        ))
    }

    fn over(mp: MultiProgress) -> LiveLines {
        LiveLines {
            mp,
            lines: Vec::new(),
            status: RefCell::new(Vec::new()),
        }
    }

    pub fn is_hidden(&self) -> bool {
        self.mp.is_hidden()
    }

    /// Replace the block's content, growing or shrinking the line count to
    /// match. New lines are inserted above any status spinners/bars. Cheap
    /// no-op when hidden.
    pub fn set_lines(&mut self, content: &[String]) {
        if self.mp.is_hidden() {
            return;
        }
        while self.lines.len() > content.len() {
            let pb = self.lines.pop().expect("len checked");
            pb.finish_and_clear();
            self.mp.remove(&pb);
        }
        while self.lines.len() < content.len() {
            let pb = self.mp.insert(self.lines.len(), ProgressBar::new_spinner());
            pb.set_style(ProgressStyle::with_template("{wide_msg}").expect("valid template"));
            self.lines.push(pb);
        }
        for (pb, line) in self.lines.iter().zip(content) {
            pb.set_message(line.clone());
        }
    }

    /// An indeterminate status spinner below the block.
    pub fn spinner(&self, msg: &str) -> ProgressBar {
        let pb = add_spinner(&self.mp, msg);
        self.status.borrow_mut().push(pb.downgrade());
        pb
    }

    /// A determinate fill bar below the block.
    pub fn bar(&self, msg: &str, len: u64) -> ProgressBar {
        let pb = add_bar(&self.mp, msg, len);
        self.status.borrow_mut().push(pb.downgrade());
        pb
    }

    /// Erase the whole block (lines and status bars) from the terminal. Also
    /// finishes any status spinners/bars handed out, so nothing repaints after
    /// the block is erased — an unfinished steady-tick bar would otherwise
    /// redraw itself within its tick interval.
    pub fn clear(&self) {
        for weak in self.status.borrow_mut().drain(..) {
            if let Some(pb) = weak.upgrade() {
                pb.finish_and_clear();
            }
        }
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
///
/// The block draws on stderr, so its chrome (title, headers, width, stale
/// dim, spinner frame) styles for that stream; `Ready` content arrives
/// pre-styled by the caller for the final stdout render and is used as-is.
pub fn render_lines(
    title: &str,
    headers: &[String],
    rows: &[Vec<Cell>],
    frame: usize,
) -> Vec<String> {
    let paint = ui::Paint::on(ui::Stream::Stderr);
    let hdrs: Vec<&str> = headers.iter().map(|s| s.as_str()).collect();
    let mut t = ui::table_on(ui::Stream::Stderr, &hdrs);
    for row in rows {
        t.add_row(row.iter().map(|c| match c {
            Cell::Ready(s) => s.clone(),
            // dim_all, not dim: styled content carries its own SGR resets,
            // which would cancel a single leading dim mid-cell.
            Cell::Stale(s) => paint.dim_all(s),
            Cell::Pending => paint.cyan(FRAMES[frame % FRAMES.len()]),
        }));
    }
    let mut out = vec![paint.bold_cyan(title)];
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
        Self::over(LiveLines::new(), title, headers, nrows)
    }

    /// Like [`LiveTable::new`] but never draws, even on a TTY — the update
    /// loop still runs for callers that must not render (e.g. `--json`).
    pub fn hidden(title: &str, headers: &[&str], nrows: usize) -> LiveTable {
        Self::over(LiveLines::hidden(), title, headers, nrows)
    }

    fn over(lines: LiveLines, title: &str, headers: &[&str], nrows: usize) -> LiveTable {
        let cols = headers.len();
        LiveTable {
            lines,
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

    /// Drive the table from a channel of source messages. Calls `on_msg` for
    /// each message; `on_msg` returns `Ok(true)` once all sources have
    /// reported. A 100ms receive timeout advances the spinner frame, and a
    /// steady sub-100ms message stream cannot starve the animation: the frame
    /// also advances on the message path once 100ms have elapsed since the
    /// last advance. Returns when done, on channel disconnect, or on the
    /// first `on_msg` error (the caller clears the block via [`Self::finish`]
    /// before propagating).
    pub fn drive<M>(
        &mut self,
        rx: &std::sync::mpsc::Receiver<M>,
        mut on_msg: impl FnMut(&mut LiveTable, M) -> anyhow::Result<bool>,
    ) -> anyhow::Result<()> {
        use std::sync::mpsc::RecvTimeoutError;
        use std::time::{Duration, Instant};

        let mut last_tick = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(msg) => {
                    let done = on_msg(self, msg)?;
                    if last_tick.elapsed() >= Duration::from_millis(100) {
                        self.tick();
                        last_tick = Instant::now();
                    } else {
                        self.redraw();
                    }
                    if done {
                        return Ok(());
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.tick();
                    last_tick = Instant::now();
                }
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }
    }

    /// Erase the live block; the caller prints the final table to stdout.
    pub fn finish(self) {
        self.lines.clear();
    }
}

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
        let rows = vec![vec![Cell::Ready("one".into()), Cell::Pending]];
        let lines = render_lines("TITLE", &headers(), &rows, 2);
        assert_eq!(lines[0], "TITLE");
        let body = lines.join("\n");
        assert!(body.contains("one"), "{body}");
        assert!(body.contains(FRAMES[2]), "{body}");
    }

    #[test]
    fn render_lines_stale_keeps_content() {
        let rows = vec![vec![Cell::Stale("old".into()), Cell::Ready("new".into())]];
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

    #[test]
    fn drive_returns_on_done_and_on_disconnect() {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<u32>();
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        let mut lt = LiveTable::new("T", &["A"], 1);
        let mut seen = Vec::new();
        lt.drive(&rx, |_, m| {
            seen.push(m);
            Ok(m == 2)
        })
        .unwrap();
        assert_eq!(seen, [1, 2]);
        // A disconnected channel ends the loop cleanly instead of erroring.
        drop(tx);
        lt.drive(&rx, |_, _: u32| Ok(false)).unwrap();
    }

    #[test]
    fn drive_propagates_on_msg_error() {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<u32>();
        tx.send(1).unwrap();
        let mut lt = LiveTable::new("T", &["A"], 1);
        let err = lt.drive(&rx, |_, _| anyhow::bail!("boom")).unwrap_err();
        assert_eq!(err.to_string(), "boom");
    }

    // Tests never run under a TTY: the whole widget must be hidden so pipes /
    // MCP / CI see no live output — same invariant progress::Steps holds.
    #[test]
    fn hidden_off_tty() {
        let mut ll = LiveLines::new();
        assert!(ll.is_hidden());
        ll.set_lines(&["x".into(), "y".into()]); // no-op when hidden
        ll.set_lines(&["z".into()]); // shrink must not panic
        let sp = ll.spinner("s");
        let bar = ll.bar("b", 3);
        assert!(sp.is_hidden());
        assert!(bar.is_hidden());
        // clear() must finish handed-out status bars, or a steady-tick bar
        // would repaint after the screen wipe.
        ll.clear();
        assert!(sp.is_finished());
        assert!(bar.is_finished());

        let mut lt = LiveTable::new("T", &["A", "B"], 2);
        lt.set(0, 0, Cell::Ready("v".into()));
        lt.redraw();
        lt.tick();
        lt.finish();
    }
}
