//! `install_abort_hook` is why a panicking `devkitd` dies instead of living on
//! holding `devkitd.lock`. It cannot be tested in-process — it would abort the
//! test runner — so these drive the `panicker` example and read its exit status.

use std::path::Path;
use std::process::{Command, Output};

/// Run the `panicker` example under one hook. Examples build beside the binary,
/// which is how `CARGO_BIN_EXE_devkit` locates it without a path of its own.
fn panicking_run(hook: &str) -> Output {
    let name = format!("panicker{}", std::env::consts::EXE_SUFFIX);
    let bin = Path::new(env!("CARGO_BIN_EXE_devkit"))
        .parent()
        .expect("target dir")
        .join("examples")
        .join(&name);
    Command::new(&bin)
        .arg(hook)
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", bin.display()))
}

/// Aborting must not cost the crash report: the diagnostics are the reason to
/// install a hook at all, and they have to be out before the process dies.
#[test]
fn each_hook_reports_the_panic_before_it_acts() {
    for hook in ["abort", "plain"] {
        let err = String::from_utf8_lossy(&panicking_run(hook).stderr).into_owned();
        assert!(err.contains("devkit `panicker` panicked"), "{hook}: {err}");
        assert!(err.contains("deliberate panic"), "{hook}: {err}");
    }
}

/// The abort hook kills the process outright. The plain hook lets the unwind
/// run its course, which for a panicking `main` is the ordinary 101 exit — the
/// same 101 that proves the release profile still unwinds.
#[test]
fn only_the_abort_hook_kills_the_process() {
    let plain = panicking_run("plain").status;
    assert_eq!(plain.code(), Some(101), "plain hook: {plain:?}");

    let aborted = panicking_run("abort").status;
    assert!(!aborted.success(), "abort hook: {aborted:?}");
    assert_ne!(aborted.code(), Some(101), "abort hook unwound: {aborted:?}");

    // Windows has no signals; an abort surfaces there as an exit code, which the
    // assertions above already cover.
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        // SIGABRT.
        assert_eq!(aborted.signal(), Some(6), "abort hook: {aborted:?}");
    }
}
