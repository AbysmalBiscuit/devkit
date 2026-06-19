use std::backtrace::{Backtrace, BacktraceStatus};

/// Install a panic hook that prints a clear bug report — binary name, panic
/// location and message, and a backtrace when one is available — so an
/// unexpected crash is diagnosable instead of an opaque `thread 'main' panicked`.
///
/// Runs before the process aborts even under `panic = "abort"`, so release builds
/// still emit the report. Recoverable failures should return `anyhow::Error`
/// instead, whose `{:?}` rendering (printed by `main`'s `Result`) shows the full
/// `.context()` chain plus a backtrace when `RUST_BACKTRACE=1`.
pub fn install_panic_hook(bin: &'static str) {
    std::panic::set_hook(Box::new(move |info| {
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown location".to_string());
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("<non-string panic payload>");

        eprintln!("\ndevkit `{bin}` panicked — this is a bug.");
        eprintln!("  at {loc}: {msg}");

        let bt = Backtrace::capture();
        match bt.status() {
            BacktraceStatus::Captured => eprintln!("\nbacktrace:\n{bt}"),
            _ => eprintln!("  (set RUST_BACKTRACE=1 for a backtrace)"),
        }
    }));
}
