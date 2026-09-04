use std::backtrace::{Backtrace, BacktraceStatus};

/// Install a panic hook that prints a clear bug report — binary name, panic
/// location and message, and a backtrace when one is available — so an
/// unexpected crash is diagnosable instead of an opaque `thread 'main' panicked`.
///
/// The hook prints and returns, so the panic then unwinds (or aborts, under a
/// profile that says so) as it otherwise would. Recoverable failures should
/// return `anyhow::Error` instead, whose `{:?}` rendering (printed by `main`'s
/// `Result`) shows the full `.context()` chain plus a backtrace when
/// `RUST_BACKTRACE=1`.
pub fn install_panic_hook(bin: &'static str) {
    std::panic::set_hook(Box::new(move |info| {
        report(bin, info);
    }));
}

/// [`install_panic_hook`], plus an unconditional `abort` once the report is out.
///
/// For a process whose state is unsafe to keep running after any panic. `devkitd`
/// is the case: it holds `devkitd.lock` exclusive for its whole life and serves
/// both registries from memory, so a thread that unwinds mid-mutation leaves a
/// live daemon answering from a half-written registry while every client fails
/// `DaemonHoldsLock` rather than falling back to the direct path. Dying is the
/// recoverable outcome: the OS releases the lock and the next caller takes over.
///
/// Aborting from the hook rather than relying on `panic = "abort"` makes the
/// policy the same in every profile, and independent of a workspace-wide setting
/// other binaries need the other way.
pub fn install_abort_hook(bin: &'static str) {
    std::panic::set_hook(Box::new(move |info| {
        report(bin, info);
        std::process::abort();
    }));
}

/// The report both hooks print.
fn report(bin: &str, info: &std::panic::PanicHookInfo<'_>) {
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
}
