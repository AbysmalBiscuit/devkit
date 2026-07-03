use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::cell::Cell;
use std::io::IsTerminal;
use std::time::Duration;

/// A `MultiProgress` drawing to stderr, or fully hidden when stderr is not a
/// terminal — so pipes, redirects, MCP, and tests produce no live output.
pub(crate) fn tty_multi() -> MultiProgress {
    if std::io::stderr().is_terminal() {
        MultiProgress::new()
    } else {
        MultiProgress::with_draw_target(ProgressDrawTarget::hidden())
    }
}

/// Add an indeterminate spinner bar to `mp` with the shared house style.
pub(crate) fn add_spinner(mp: &MultiProgress, msg: &str) -> ProgressBar {
    let pb = mp.add(ProgressBar::new_spinner());
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {wide_msg}").expect("valid spinner template"),
    );
    pb.enable_steady_tick(Duration::from_millis(80));
    pb.set_message(msg.to_string());
    pb
}

/// Add a determinate fill bar of length `len` to `mp` with the shared house
/// style.
pub(crate) fn add_bar(mp: &MultiProgress, msg: &str, len: u64) -> ProgressBar {
    let pb = mp.add(ProgressBar::new(len));
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {wide_msg} [{bar:20.cyan/dim}] {pos}/{len}")
            .expect("valid bar template")
            .progress_chars("=>-"),
    );
    pb.enable_steady_tick(Duration::from_millis(80));
    pb.set_message(msg.to_string());
    pb
}

/// A group of progress bars sharing one [`MultiProgress`]. Each bar animates on
/// stderr; the whole group is hidden when stderr is not a terminal, so pipes,
/// redirects, MCP, and tests produce no progress output.
///
/// Two orthogonal display axes:
/// - Numbering: [`Steps::new`] is unnumbered — for concurrent displays where
///   several [`Steps::spinner`] bars animate at once, or for
///   branchy/prompt-interleaved flows where a fixed `[i/N]` count would be
///   misleading. [`Steps::with_total`] numbers each [`Steps::during`] step
///   `[i/total]`.
/// - Persistence: by default steps are transient — each step's spinner is
///   cleared when it settles, leaving no trace. [`Steps::persistent`] /
///   [`Steps::persistent_with_total`] instead leave each settled step on
///   screen as a numbered `✓`/`✗` line, so a multi-step command keeps a
///   scrollback record of what it did.
pub struct Steps {
    mp: MultiProgress,
    total: Option<usize>,
    n: Cell<usize>,
    persist: bool,
}

impl Steps {
    pub fn new() -> Steps {
        Steps {
            mp: Self::target(),
            total: None,
            n: Cell::new(0),
            persist: false,
        }
    }

    /// Numbered mode: every [`Steps::during`] message is prefixed `[i/total]`.
    pub fn with_total(total: usize) -> Steps {
        Steps {
            mp: Self::target(),
            total: Some(total),
            n: Cell::new(0),
            persist: false,
        }
    }

    /// Persistent step-log mode: each completed [`Steps::during`] step stays on
    /// screen as a numbered `✓ n. msg (elapsed)` line instead of clearing, so
    /// a multi-step command leaves a record of what it did.
    pub fn persistent() -> Steps {
        Steps {
            mp: Self::target(),
            total: None,
            n: Cell::new(0),
            persist: true,
        }
    }

    /// Persistent mode with `[i/total]` numbering.
    pub fn persistent_with_total(total: usize) -> Steps {
        Steps {
            mp: Self::target(),
            total: Some(total),
            n: Cell::new(0),
            persist: true,
        }
    }

    fn target() -> MultiProgress {
        tty_multi()
    }

    /// In numbered mode, prefix `[i/total] ` and advance the counter. In
    /// persistent unnumbered mode, prefix a plain `n. ` ordinal. Otherwise
    /// pass the message through unchanged.
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

    /// An indeterminate spinner bar for a single opaque/batched fetch. The
    /// message is used verbatim — embed any prefix yourself. Used directly for
    /// concurrent displays that show several bars at once.
    pub fn spinner(&self, msg: &str) -> ProgressBar {
        add_spinner(&self.mp, msg)
    }

    /// A determinate fill bar for a loop over a known count (`len`).
    pub fn bar(&self, msg: &str, len: u64) -> ProgressBar {
        add_bar(&self.mp, msg, len)
    }

    /// Run `f` under a spinner (auto-numbered in numbered mode). Transient
    /// mode clears the bar before returning — so the spinner never stays live
    /// across a `?`, a stdin prompt, or stdout output. Persistent mode prints
    /// the settled `✓` line into scrollback instead; the bar itself is still
    /// cleared, so no bar is ever active across a prompt in either mode.
    ///
    /// Every completion counts as success here — the persistent log line is
    /// always `✓`, regardless of what `f` returned. A closure returning
    /// `anyhow::Result` belongs in [`Steps::during_result`], which marks the
    /// step `✗` on error instead of logging a failure as succeeded.
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

    /// Clear every bar in the group (call once all work is done).
    pub fn clear(&self) {
        let _ = self.mp.clear();
    }
}

/// `312ms` under a second, `1.2s` under a minute, `1m 12s` from there up.
fn fmt_elapsed(d: Duration) -> String {
    if d < Duration::from_secs(1) {
        format!("{}ms", d.as_millis())
    } else if d < Duration::from_secs(60) {
        format!("{:.1}s", d.as_secs_f64())
    } else {
        format!("{}m {}s", d.as_secs() / 60, d.as_secs() % 60)
    }
}

/// The persistent line a settled step leaves behind. It prints on stderr, so
/// the mark's colour keys off that stream, not stdout.
fn finish_line(ok: bool, label: &str, elapsed: Duration) -> String {
    let paint = crate::ui::Paint::on(crate::ui::Stream::Stderr);
    let mark = if ok {
        paint.green("✓")
    } else {
        paint.red("✗")
    };
    format!("{mark} {label} ({})", fmt_elapsed(elapsed))
}

impl Default for Steps {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests never run under a TTY, so every bar the helper hands out must be
    // hidden — guaranteeing pipes / MCP / CI print no progress noise.
    #[test]
    fn steps_bars_hidden_off_tty() {
        let steps = Steps::new();
        assert!(steps.spinner("working…").is_hidden());
        assert!(steps.bar("counting…", 10).is_hidden());
        steps.clear();
    }

    #[test]
    fn during_returns_closure_value() {
        let steps = Steps::with_total(2);
        let out = steps.during("step one", || 41 + 1);
        assert_eq!(out, 42);
    }

    #[test]
    fn numbered_mode_advances_counter() {
        let steps = Steps::with_total(3);
        assert_eq!(steps.label("a"), "[1/3] a");
        assert_eq!(steps.label("b"), "[2/3] b");
        assert_eq!(steps.label("c"), "[3/3] c");
    }

    #[test]
    fn unnumbered_mode_passes_through() {
        let steps = Steps::new();
        assert_eq!(steps.label("a"), "a");
        assert_eq!(steps.label("b"), "b");
    }

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
        assert_eq!(fmt_elapsed(Duration::from_millis(999)), "999ms");
        assert_eq!(fmt_elapsed(Duration::from_millis(1000)), "1.0s");
        assert_eq!(fmt_elapsed(Duration::from_millis(1200)), "1.2s");
        assert_eq!(fmt_elapsed(Duration::from_millis(59_940)), "59.9s");
        assert_eq!(fmt_elapsed(Duration::from_millis(60_000)), "1m 0s");
        assert_eq!(fmt_elapsed(Duration::from_millis(72_500)), "1m 12s");
        assert_eq!(fmt_elapsed(Duration::from_secs(125)), "2m 5s");
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
}
