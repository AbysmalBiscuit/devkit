use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::io::IsTerminal;
use std::time::Duration;

/// A group of progress bars sharing one [`MultiProgress`]. Each bar animates on
/// stderr; the whole group is hidden when stderr is not a terminal, so pipes,
/// redirects, MCP, and tests produce no progress output.
///
/// Numbering is the caller's job: embed any `[2/4]`-style prefix in the message.
/// Call [`Steps::clear`] once all work is done, before printing results.
pub struct Steps {
    mp: MultiProgress,
}

impl Steps {
    pub fn new() -> Steps {
        let mp = if std::io::stderr().is_terminal() {
            MultiProgress::new()
        } else {
            MultiProgress::with_draw_target(ProgressDrawTarget::hidden())
        };
        Steps { mp }
    }

    /// An indeterminate spinner bar for a single opaque/batched fetch.
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

    /// Clear every bar in the group (call once all work is done).
    pub fn clear(&self) {
        let _ = self.mp.clear();
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
}
