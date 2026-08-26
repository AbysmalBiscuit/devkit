#[path = "common/shimtest.rs"]
mod shimtest;

use std::process::Command;

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
    let out = Command::new(&exe)
        .arg("install-links")
        .output()
        .expect("spawn install-links");
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
    let out = Command::new(&exe)
        .arg("install-links")
        .output()
        .expect("spawn install-links");
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
    let out = Command::new(&exe)
        .arg("install-links")
        .output()
        .expect("spawn install-links");
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
    let out = Command::new(&exe)
        .arg("install-links")
        .output()
        .expect("spawn install-links");
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

#[test]
fn force_takes_over_a_foreign_binary() {
    let (dir, exe) = staged();
    let foreign = shim_path(dir.path(), "issue");
    std::fs::write(&foreign, b"#!/bin/sh\necho not devkit\n").expect("write foreign issue");
    let out = Command::new(&exe)
        .args(["install-links", "--force"])
        .output()
        .expect("spawn install-links --force");
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
    let first = Command::new(&exe)
        .arg("install-links")
        .output()
        .expect("spawn first install-links");
    assert!(first.status.success());

    let second = Command::new(&exe)
        .arg("install-links")
        .output()
        .expect("spawn second install-links");
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
