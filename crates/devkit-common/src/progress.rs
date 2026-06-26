use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::cell::Cell;
use std::io::IsTerminal;
use std::time::Duration;

/// A group of progress bars sharing one [`MultiProgress`]. Each bar animates on
/// stderr; the whole group is hidden when stderr is not a terminal, so pipes,
/// redirects, MCP, and tests produce no progress output.
///
/// Two display modes:
/// - [`Steps::new`] is unnumbered — for concurrent displays where several
///   [`Steps::spinner`] bars animate at once, or for branchy/prompt-interleaved
///   flows where a fixed `[i/N]` count would be misleading.
/// - [`Steps::with_total`] numbers each [`Steps::during`] step `[i/total]`.
pub struct Steps {
    mp: MultiProgress,
    total: Option<usize>,
    n: Cell<usize>,
}

impl Steps {
    pub fn new() -> Steps {
        Steps {
            mp: Self::target(),
            total: None,
            n: Cell::new(0),
        }
    }

    /// Numbered mode: every [`Steps::during`] message is prefixed `[i/total]`.
    pub fn with_total(total: usize) -> Steps {
        Steps {
            mp: Self::target(),
            total: Some(total),
            n: Cell::new(0),
        }
    }

    fn target() -> MultiProgress {
        if std::io::stderr().is_terminal() {
            MultiProgress::new()
        } else {
            MultiProgress::with_draw_target(ProgressDrawTarget::hidden())
        }
    }

    /// In numbered mode, prefix `[i/total] ` and advance the counter; otherwise
    /// pass the message through unchanged.
    fn label(&self, msg: &str) -> String {
        match self.total {
            Some(total) => {
                let i = self.n.get() + 1;
                self.n.set(i);
                format!("[{i}/{total}] {msg}")
            }
            None => msg.to_string(),
        }
    }

    /// An indeterminate spinner bar for a single opaque/batched fetch. The
    /// message is used verbatim — embed any prefix yourself. Used directly for
    /// concurrent displays that show several bars at once.
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

    /// A determinate fill bar for a loop over a known count (`len`).
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

    /// Run `f` under a spinner (auto-numbered in numbered mode), clearing the
    /// bar before returning — so the spinner never stays live across a `?`, a
    /// stdin prompt, or stdout output. The closure's return value (often a
    /// `Result`) is returned unchanged so callers can `?` it after the clear.
    pub fn during<T>(&self, msg: &str, f: impl FnOnce() -> T) -> T {
        let pb = self.spinner(&self.label(msg));
        let out = f();
        pb.finish_and_clear();
        out
    }

    /// Clear every bar in the group (call once all work is done).
    pub fn clear(&self) {
        let _ = self.mp.clear();
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
}
