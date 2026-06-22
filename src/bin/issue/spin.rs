use indicatif::{ProgressBar, ProgressStyle};
use std::io::IsTerminal;
use std::time::Duration;

/// A transient spinner that animates on stderr, so a piped or redirected stdout
/// stays free of progress noise. Hidden when stderr is not a terminal — pipes,
/// redirects, and tests then produce no output at all.
///
/// Update the phase with [`ProgressBar::set_message`] as work proceeds, and call
/// [`ProgressBar::finish_and_clear`] before printing results so the line is gone.
pub fn spinner(msg: &str) -> ProgressBar {
    if !std::io::stderr().is_terminal() {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::new_spinner();
    // `with_template` only fails on a malformed template; this one is constant.
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}").expect("valid spinner template"),
    );
    pb.enable_steady_tick(Duration::from_millis(80));
    pb.set_message(msg.to_string());
    pb
}
