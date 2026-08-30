use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::io::IsTerminal;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// A `MultiProgress` drawing to stderr, or fully hidden when stderr is not a
/// terminal — so pipes, redirects, MCP, and tests produce no live output.
pub(crate) fn tty_multi() -> MultiProgress {
    // indicatif's template colours ({spinner:.cyan}) render through console's
    // process-wide colour flag, which auto-detects on *stdout*. Every bar
    // here draws on stderr, so align the flag with the stderr decision:
    // `cmd > file` keeps a coloured spinner, and NO_COLOR still disables it.
    console::set_colors_enabled(crate::ui::color_enabled_on(crate::ui::Stream::Stderr));
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
    n: AtomicUsize,
    persist: bool,
}

// `Steps` must stay `Send + Sync` so scoped worker threads can share one
// `&Steps` — `issue end` dispatches concurrent removals, each drawing its own
// bar through the shared `MultiProgress`.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Steps>();
};

/// A running step that draws sub-progress. Its transient line is rewritten in
/// place and never persists; finished sub-steps persist as their own indented
/// lines. Every method takes `&self` so a shared callback can drive them.
pub struct Step<'a> {
    steps: &'a Steps,
    activity: Mutex<Option<ProgressBar>>,
    detail: Mutex<Option<String>>,
}

impl Step<'_> {
    /// Rewrite the transient line beneath this step, creating it on first call.
    pub fn activity(&self, msg: &str) {
        let mut slot = self
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match slot.as_ref() {
            Some(pb) => pb.set_message(format!("  {msg}")),
            None => *slot = Some(add_spinner(&self.steps.mp, &format!("  {msg}"))),
        }
    }

    /// Persist a finished sub-step line beneath this step and clear the
    /// transient line, so the next sub-step starts from an empty slot.
    ///
    /// `MultiProgress` prints above its live bars and this step's bar is still
    /// live, so sub-steps land above the step's own settled line.
    pub fn substep(&self, msg: &str) {
        self.clear_activity();
        let paint = crate::ui::Paint::on(crate::ui::Stream::Stderr);
        let _ = self
            .steps
            .mp
            .println(format!("     {} {msg}", paint.green("✓")));
    }

    /// Text folded into the settled line's parens, ahead of the elapsed time.
    pub fn detail(&self, d: &str) {
        *self
            .detail
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(d.to_string());
    }

    /// Clear the transient line, leaving no trace of it. A finished step calls
    /// this itself before settling; a caller that already knows sub-progress
    /// is done can clear it early to keep an activity line from lingering
    /// beneath later output.
    pub fn clear_activity(&self) {
        if let Some(pb) = self
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            pb.finish_and_clear();
        }
    }
}

impl Steps {
    pub fn new() -> Steps {
        Steps {
            mp: Self::target(),
            total: None,
            n: AtomicUsize::new(0),
            persist: false,
        }
    }

    /// Numbered mode: every [`Steps::during`] message is prefixed `[i/total]`.
    pub fn with_total(total: usize) -> Steps {
        Steps {
            mp: Self::target(),
            total: Some(total),
            n: AtomicUsize::new(0),
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
            n: AtomicUsize::new(0),
            persist: true,
        }
    }

    /// Persistent mode with `[i/total]` numbering.
    pub fn persistent_with_total(total: usize) -> Steps {
        Steps {
            mp: Self::target(),
            total: Some(total),
            n: AtomicUsize::new(0),
            persist: true,
        }
    }

    fn target() -> MultiProgress {
        tty_multi()
    }

    /// In numbered mode, prefix `[i/total] ` and advance the counter. In
    /// persistent unnumbered mode, prefix a plain `n. ` ordinal. Otherwise
    /// pass the message through unchanged. Every mode mints an ordinal, even
    /// the unnumbered/transient one that never shows it, so [`Steps::started`]
    /// reports true step coverage regardless of display mode.
    fn label(&self, msg: &str) -> String {
        let i = self.n.fetch_add(1, Ordering::Relaxed) + 1;
        match self.total {
            Some(total) => format!("[{i}/{total}] {msg}"),
            None if self.persist => format!("{i}. {msg}"),
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
    /// mode clears the bar before returning, so the spinner never stays live
    /// across a `?`, a stdin prompt, or stdout output. Persistent mode prints
    /// the settled line into scrollback instead; the bar itself is still
    /// cleared, so no bar is ever active across a prompt in either mode.
    ///
    /// Every completion counts as success here: the persistent log line is
    /// always `✓`, regardless of what `f` returned. A closure returning
    /// `anyhow::Result` belongs in [`Steps::during_result`], and one that
    /// reports failure some other way in [`Steps::during_ok`]; both mark the
    /// step `✗` instead of logging a failure as succeeded.
    pub fn during<T>(&self, msg: &str, f: impl FnOnce() -> T) -> T {
        self.during_step(msg, |_| f())
    }

    /// [`Steps::during`] for a step that judges its own success without
    /// returning a `Result`: the closure hands back its value paired with
    /// whether the step succeeded, and the settled line is `✗` when that
    /// reads false.
    pub fn during_ok<T>(&self, msg: &str, f: impl FnOnce() -> (T, bool)) -> T {
        self.run_step(msg, |_| f())
    }

    /// [`Steps::during`] for fallible steps: in persistent mode the settled
    /// line is `✗` when the closure errors, so the failed step stays
    /// identifiable in the log.
    pub fn during_result<T>(
        &self,
        msg: &str,
        f: impl FnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        self.during_step_result(msg, |_| f())
    }

    /// [`Steps::during`] for a step that draws its own sub-progress. The
    /// closure gets a [`Step`] handle for a transient line, persisted
    /// sub-steps, and a detail folded into the settled line.
    pub fn during_step<T>(&self, msg: &str, f: impl FnOnce(&Step<'_>) -> T) -> T {
        self.run_step(msg, |step| (f(step), true))
    }

    /// [`Steps::during_step`] for fallible steps.
    pub fn during_step_result<T>(
        &self,
        msg: &str,
        f: impl FnOnce(&Step<'_>) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        self.run_step(msg, |step| {
            let out = f(step);
            let ok = out.is_ok();
            (out, ok)
        })
    }

    /// Mint the ordinal, draw the step's spinner, run `f`, then settle. `f`
    /// returns its value and whether the step succeeded.
    fn run_step<T>(&self, msg: &str, f: impl FnOnce(&Step<'_>) -> (T, bool)) -> T {
        let label = self.label(msg);
        let pb = self.spinner(&label);
        let step = Step {
            steps: self,
            activity: Mutex::new(None),
            detail: Mutex::new(None),
        };
        let (out, ok) = f(&step);
        step.clear_activity();
        let detail = step
            .detail
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let line = self
            .persist
            .then(|| finish_line(ok, &label, pb.elapsed(), detail.as_deref()));
        pb.finish_and_clear();
        if let Some(line) = line {
            let _ = self.mp.println(line);
        }
        out
    }

    /// Clear every bar in the group (call once all work is done).
    pub fn clear(&self) {
        let _ = self.mp.clear();
    }

    /// How many steps have begun. A step counts from the moment its label is
    /// minted, so a step still running is included. Callers outside this
    /// module assert step coverage with it; `label` is private.
    pub fn started(&self) -> usize {
        self.n.load(Ordering::Relaxed)
    }

    /// Run `f` with every bar in the group hidden, then redraw them. Use around
    /// a stdin prompt or any stdout write that would otherwise be torn by a live
    /// bar redrawing on stderr.
    pub fn suspend<T>(&self, f: impl FnOnce() -> T) -> T {
        self.mp.suspend(f)
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
/// the mark's colour keys off that stream, not stdout. `detail` rides inside
/// the same parens as the elapsed time rather than adding a separator.
fn finish_line(ok: bool, label: &str, elapsed: Duration, detail: Option<&str>) -> String {
    let paint = crate::ui::Paint::on(crate::ui::Stream::Stderr);
    let mark = if ok {
        paint.green("✓")
    } else {
        paint.red("✗")
    };
    match detail {
        Some(d) => format!("{mark} {label} ({d}, {})", fmt_elapsed(elapsed)),
        None => format!("{mark} {label} ({})", fmt_elapsed(elapsed)),
    }
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
        // The mark keys off stderr, so the expectation paints from that stream
        // too and holds whether or not colour is enabled.
        let paint = crate::ui::Paint::on(crate::ui::Stream::Stderr);
        let d = Duration::from_millis(312);
        assert_eq!(
            finish_line(true, "1. foo", d, None),
            format!("{} 1. foo (312ms)", paint.green("✓"))
        );
        assert_eq!(
            finish_line(false, "2. bar", d, None),
            format!("{} 2. bar (312ms)", paint.red("✗"))
        );
        assert_eq!(
            finish_line(true, "1. foo", d, Some("2 things")),
            format!("{} 1. foo (2 things, 312ms)", paint.green("✓"))
        );
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

    #[test]
    fn during_result_runs_concurrently_across_threads() {
        let steps = Steps::persistent();
        let done = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|s| {
            for i in 0..8 {
                let steps = &steps;
                let done = &done;
                s.spawn(move || {
                    let out: anyhow::Result<usize> =
                        steps.during_result(&format!("task {i}"), || Ok(i));
                    assert_eq!(out.unwrap(), i);
                    done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                });
            }
        });
        assert_eq!(done.load(std::sync::atomic::Ordering::Relaxed), 8);
        // The ordinal advanced exactly once per step regardless of interleaving:
        // 8 steps ran, so the next label is the 9th.
        assert_eq!(steps.label("next"), "9. next");
        steps.clear();
    }

    #[test]
    fn started_counts_every_step_that_began() {
        let steps = Steps::persistent_with_total(2);
        steps.during("first", || ());
        steps.during_result("second", || anyhow::Ok(())).unwrap();
        assert_eq!(steps.started(), 2);
        steps.clear();
    }

    #[test]
    fn started_counts_steps_in_unnumbered_transient_mode() {
        let steps = Steps::new();
        steps.during("first", || ());
        steps.during_result("second", || anyhow::Ok(())).unwrap();
        assert_eq!(steps.started(), 2);
        steps.clear();
    }

    /// A step that draws sub-progress still consumes exactly one ordinal, so a
    /// numbered run does not end short of its total.
    #[test]
    fn a_step_with_sub_progress_consumes_one_ordinal() {
        let steps = Steps::persistent_with_total(2);
        steps.during_step("first", |step| {
            step.activity("working");
            step.substep("1/2 one");
            step.substep("2/2 two");
            step.detail("2 things");
        });
        steps.during("second", || {});
        assert_eq!(steps.started(), 2);
    }

    /// The handle's methods take &self so a Fn callback can drive them, which is
    /// what a parallel producer needs.
    #[test]
    fn a_step_handle_is_usable_from_a_shared_reference() {
        fn assert_sync<T: Sync>(_: &T) {}
        let steps = Steps::persistent();
        steps.during_step("first", |step| {
            assert_sync(step);
            let emit: &(dyn Fn(&str) + Sync) = &|m| step.activity(m);
            emit("from a shared reference");
        });
        assert_eq!(steps.started(), 1);
    }
}
