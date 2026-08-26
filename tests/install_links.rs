#[path = "common/shimtest.rs"]
mod shimtest;

use std::process::{Command, Output, Stdio};
use std::time::Duration;

/// Retry `attempt` briefly on a transient `ExecutableFileBusy`. `staged()`
/// just finished writing the binary `attempt` executes moments earlier, and
/// `cargo test`'s default parallelism runs several of these tests at once,
/// each copying-then-executing its own freshly staged binary; occasionally
/// that races a kernel-level "busy" window on the just-closed write handle
/// (never a real conflict between two tests — each stages into its own
/// tempdir). The race clears within milliseconds. `attempt` is called again
/// from scratch on each retry so it can build a fresh `Command` (and fresh
/// `Stdio`s, which are not `Clone`) every time.
fn retry_on_busy<T>(mut attempt: impl FnMut() -> std::io::Result<T>) -> T {
    let mut attempts = 0;
    loop {
        match attempt() {
            Ok(v) => return v,
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 40 => {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => panic!("spawn: {e}"),
        }
    }
}

/// Run `install-links` (with `args` appended) against `exe`.
fn run(exe: &std::path::Path, args: &[&str]) -> Output {
    retry_on_busy(|| Command::new(exe).args(args).output())
}

/// Run `install-links` against a throwaway directory holding a copy of the
/// built binary, so the test never touches the real CARGO_HOME.
fn staged() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let exe = dir.path().join(if cfg!(windows) {
        "devkit.exe"
    } else {
        "devkit"
    });
    std::fs::copy(env!("CARGO_BIN_EXE_devkit"), &exe).expect("stage devkit");
    (dir, exe)
}

fn shim_path(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    dir.join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    })
}

#[test]
fn creates_every_shim() {
    let (dir, exe) = staged();
    let out = run(&exe, &["install-links"]);
    assert!(
        out.status.success(),
        "install-links failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    for name in ["issue", "devrun", "portm", "lockm", "docm", "devkit-mcp"] {
        assert!(
            shim_path(dir.path(), name).exists(),
            "install-links did not create {name}"
        );
    }
}

/// The upgrade path: a real devkit binary already sits at a shim name.
#[test]
fn replaces_an_existing_devkit_binary() {
    let (dir, exe) = staged();
    let stale = shim_path(dir.path(), "portm");
    std::fs::copy(env!("CARGO_BIN_EXE_devkit"), &stale).expect("stage a stale portm");
    let out = run(&exe, &["install-links"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.lines()
            .any(|l| l.contains("replaced") && l.contains("portm")),
        "should report portm replaced: {text}"
    );
    assert!(
        shimtest::same_inode(&exe, &stale),
        "portm should now be a hardlink to devkit"
    );
}

/// The upgrade path must work at every shim name, not just `portm`. This is
/// the test that would have caught `lockm`'s hidden-subcommand breakage:
/// `replaces_an_existing_devkit_binary` alone never touched `lockm`, whose
/// `hook` subcommand is `#[command(hide = true)]` and so never appears in
/// its own `--help` output.
#[test]
fn replaces_a_real_devkit_binary_at_every_shim_name() {
    let (dir, exe) = staged();
    let mut stale_paths = Vec::new();
    for name in ["issue", "devrun", "portm", "lockm", "docm", "devkit-mcp"] {
        let stale = shim_path(dir.path(), name);
        std::fs::copy(env!("CARGO_BIN_EXE_devkit"), &stale)
            .unwrap_or_else(|e| panic!("stage a stale {name}: {e}"));
        stale_paths.push((name, stale));
    }
    let out = run(&exe, &["install-links"]);
    assert!(
        out.status.success(),
        "install-links failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    for (name, stale) in &stale_paths {
        assert!(
            text.lines()
                .any(|l| l.contains("replaced") && l.contains(name)),
            "should report {name} replaced: {text}"
        );
        assert!(
            shimtest::same_inode(&exe, stale),
            "{name} should now be a hardlink to devkit"
        );
    }
}

/// Coverage for the hide-filter itself, not just the marker responder: a
/// real devkit copy always answers the marker probe and is `Accepted` before
/// the `--help` path ever runs, so `replaces_a_real_devkit_binary_at_every_shim_name`
/// alone never reaches the subcommand check `lockm`'s hidden `hook` broke.
/// This wrapper is a faithful pre-marker devkit: it forwards `--version` and
/// `--help` to the real staged binary (`exec -a` forces its argv[0] to
/// `lockm`, so the output is genuinely lockm's own — `hook` is a real
/// subcommand but never printed in `--help`) while refusing the probe flag
/// outright, forcing `is_devkit_binary` down the `--help`/subcommand path
/// this test exists to guard. Unix-only: `exec -a` is a bash extension with
/// no portable Windows equivalent.
#[test]
#[cfg(unix)]
fn replaces_a_faithful_pre_marker_lockm() {
    let (dir, exe) = staged();
    let wrapper = shim_path(dir.path(), "lockm");
    let script = format!(
        "#!/usr/bin/env bash\nif [ \"$1\" = \"--devkit-shim-probe\" ]; then\n  exit 1\nfi\nexec -a lockm \"{}\" \"$@\"\n",
        exe.display()
    );
    std::fs::write(&wrapper, script).expect("write pre-marker lockm wrapper");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))
            .expect("make wrapper executable");
    }
    let out = run(&exe, &["install-links"]);
    assert!(
        out.status.success(),
        "install-links failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.lines()
            .any(|l| l.contains("replaced") && l.contains("lockm")),
        "a faithful pre-marker lockm must be replaced via the subcommand path: {text}"
    );
    assert!(
        shimtest::same_inode(&exe, &wrapper),
        "lockm should now be a hardlink to devkit"
    );
}

/// A name held by something else is never destroyed. On Unix the script is
/// made executable so it actually runs and answers `--version` with output
/// that does not name devkit — reaching the content-comparison branch, not
/// just the "won't even execute" fallback. On Windows the same bytes are not
/// a valid PE, so there this test only re-exercises the can't-spawn branch;
/// the content-comparison coverage is Unix-only.
#[test]
fn leaves_a_foreign_binary_alone() {
    let (dir, exe) = staged();
    let foreign = shim_path(dir.path(), "issue");
    std::fs::write(&foreign, b"#!/bin/sh\necho not devkit\n").expect("write foreign issue");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o755))
            .expect("make foreign script executable");
    }
    let out = run(&exe, &["install-links"]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(&foreign).expect("foreign still readable"),
        b"#!/bin/sh\necho not devkit\n",
        "a foreign file at a shim name must not be replaced"
    );
    assert!(
        text.lines()
            .any(|l| l.contains("skipped") && l.contains("issue")),
        "should report the skip: {text}"
    );
}

/// A foreign binary that convincingly answers `--version` with `issue 1.2.3`
/// must still be rejected. Matching the version line alone is exactly what
/// the old, too-loose check accepted (a prefix match against the whole shim
/// set) — the fix requires `--help` to also name every real `issue`
/// subcommand, which this script's generic help text does not. On Unix the
/// script is made executable so it actually runs this scenario; on Windows
/// the bytes are not a valid executable, so there this test only re-exercises
/// the can't-spawn branch, the same as `leaves_a_foreign_binary_alone` — the
/// anchored-match/subcommand-probe rejection is Unix-only coverage.
#[test]
fn leaves_a_convincingly_named_foreign_binary_alone() {
    let (dir, exe) = staged();
    let foreign = shim_path(dir.path(), "issue");
    let script: &[u8] = b"#!/bin/sh\ncase \"$1\" in\n  --version) echo \"issue 1.2.3\" ;;\n  --help) echo \"issue 1.2.3\"; echo \"a foreign issue tracker, unrelated to devkit\" ;;\n  *) exit 1 ;;\nesac\n";
    std::fs::write(&foreign, script).expect("write convincing foreign issue");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o755))
            .expect("make foreign script executable");
    }
    let out = run(&exe, &["install-links"]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(&foreign).expect("foreign still readable"),
        script,
        "a foreign file at a shim name must not be replaced, even one echoing a matching version"
    );
    assert!(
        !shimtest::same_inode(&exe, &foreign),
        "a binary that only echoes a matching version string must not be linked over: {text}"
    );
    assert!(
        text.lines()
            .any(|l| l.contains("skipped") && l.contains("issue")),
        "should report the skip: {text}"
    );
}

/// The zero-subcommand exploit: `devkit-mcp` has no subcommands (`McpCli`
/// is a unit struct), so a foreign binary cannot be accepted by the
/// subcommand-set probe — there is nothing in it to check — and it does not
/// know to answer the identity-probe marker either. A bare anchored-version
/// check alone would have wrongly accepted this script.
#[test]
fn leaves_a_convincing_but_markerless_devkit_mcp_alone() {
    let (dir, exe) = staged();
    let foreign = shim_path(dir.path(), "devkit-mcp");
    let script: &[u8] = b"#!/bin/sh\ncase \"$1\" in\n  --version) echo \"devkit-mcp 1.2.3\" ;;\n  --help) echo \"devkit-mcp 1.2.3\"; echo \"a foreign mcp-like tool, unrelated to devkit\" ;;\n  *) exit 1 ;;\nesac\n";
    std::fs::write(&foreign, script).expect("write convincing foreign devkit-mcp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o755))
            .expect("make foreign script executable");
    }
    let out = run(&exe, &["install-links"]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(&foreign).expect("foreign still readable"),
        script,
        "a foreign file at devkit-mcp must not be replaced, even one echoing a matching version"
    );
    assert!(
        !shimtest::same_inode(&exe, &foreign),
        "a binary with no subcommands to verify against must not be linked over: {text}"
    );
    assert!(
        text.lines()
            .any(|l| l.contains("skipped") && l.contains("devkit-mcp")),
        "should report the skip: {text}"
    );
}

#[test]
fn force_takes_over_a_foreign_binary() {
    let (dir, exe) = staged();
    let foreign = shim_path(dir.path(), "issue");
    std::fs::write(&foreign, b"#!/bin/sh\necho not devkit\n").expect("write foreign issue");
    let out = run(&exe, &["install-links", "--force"]);
    assert!(out.status.success());
    assert!(
        shimtest::same_inode(&exe, &foreign),
        "--force should claim the name"
    );
}

/// Running install-links again once every shim is already the right hardlink
/// must be a no-op that reports it, not a remove-and-relink cycle.
#[test]
fn running_twice_is_idempotent() {
    let (dir, exe) = staged();
    let first = run(&exe, &["install-links"]);
    assert!(first.status.success());

    let second = run(&exe, &["install-links"]);
    assert!(second.status.success());
    let text = String::from_utf8_lossy(&second.stdout).to_string();
    assert!(
        text.contains("current"),
        "second run should report already-linked shims: {text}"
    );
    for name in ["issue", "devrun", "portm", "lockm", "docm", "devkit-mcp"] {
        assert!(
            shimtest::same_inode(&exe, &shim_path(dir.path(), name)),
            "{name} should still be a hardlink to devkit after the second run"
        );
    }
}

/// Task 9 calls `is_devkit_binary`'s probes on every shim invocation, and
/// `lockm hook pretooluse` reads its real payload from stdin — a foreign
/// binary sitting at a shim name while being probed must never be able to
/// consume that payload. Pipes a known sentinel into the outer
/// `install-links` process's own stdin and asserts a foreign script at a
/// shim name never received it: with the probe's stdin correctly nulled,
/// the script sees immediate EOF and writes nothing to the capture file.
#[test]
#[cfg(unix)]
fn probes_never_consume_the_callers_stdin() {
    use std::io::Write;

    let (dir, exe) = staged();
    let foreign = shim_path(dir.path(), "docm");
    let capture = dir.path().join("stdin-capture");
    let script = format!(
        "#!/bin/sh\ncat >> \"{}\" 2>/dev/null\nexit 1\n",
        capture.display()
    );
    std::fs::write(&foreign, script).expect("write stdin-capturing foreign docm");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o755))
            .expect("make foreign script executable");
    }

    let mut child = retry_on_busy(|| {
        Command::new(&exe)
            .arg("install-links")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    });
    child
        .stdin
        .take()
        .expect("install-links stdin")
        .write_all(b"SENTINEL-DATA-NOT-FOR-A-PROBE\n")
        .expect("write sentinel to install-links stdin");
    child.wait().expect("wait for install-links");

    let captured = std::fs::read(&capture).unwrap_or_default();
    assert!(
        captured.is_empty(),
        "a probe must not consume the caller's stdin: captured {:?}",
        String::from_utf8_lossy(&captured)
    );
}
