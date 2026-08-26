//! Every binary reports its version. The plugin's bootstrap hook pins binaries
//! to a release matching its own version, so a user (or the hook) needs a way
//! to see which version is actually on PATH.

#[path = "common/shimtest.rs"]
mod shimtest;
use std::process::Command;

fn version_output(exe: &str) -> (bool, String) {
    let out = Command::new(exe)
        .arg("--version")
        .output()
        .unwrap_or_else(|e| panic!("spawn {exe}: {e}"));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

fn assert_reports_version(name: &str, exe: &str) {
    let (ok, text) = version_output(exe);
    assert!(ok, "`{name} --version` should exit 0, got: {text}");
    assert!(
        text.contains(env!("CARGO_PKG_VERSION")),
        "`{name} --version` should print {}, got: {text}",
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
fn portm_reports_version() {
    let (_dir, link) = shimtest::linked("portm");
    assert_reports_version("portm", link.to_str().expect("utf-8 link path"));
}

/// Parity requirement: a `portm` user must see the same behavior through
/// `devkit ports`, version output included.
#[test]
fn devkit_ports_reports_the_package_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .args(["ports", "--version"])
        .output()
        .expect("spawn devkit ports --version");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "`devkit ports --version` should exit 0, got: {text}"
    );
    assert!(
        text.contains(env!("CARGO_PKG_VERSION")),
        "`devkit ports --version` should print {}, got: {text}",
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
fn lockm_reports_version() {
    let (_dir, link) = shimtest::linked("lockm");
    assert_reports_version("lockm", link.to_str().expect("utf-8 link path"));
}

/// Parity requirement: a `lockm` user must see the same behavior through
/// `devkit locks`, version output included.
#[test]
fn devkit_locks_reports_the_package_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .args(["locks", "--version"])
        .output()
        .expect("spawn devkit locks --version");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "`devkit locks --version` should exit 0, got: {text}"
    );
    assert!(
        text.contains(env!("CARGO_PKG_VERSION")),
        "`devkit locks --version` should print {}, got: {text}",
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
fn issue_reports_version() {
    assert_reports_version("issue", env!("CARGO_BIN_EXE_issue"));
}

#[test]
fn devrun_reports_version() {
    assert_reports_version("devrun", env!("CARGO_BIN_EXE_devrun"));
}

#[test]
fn devkit_reports_version() {
    assert_reports_version("devkit", env!("CARGO_BIN_EXE_devkit"));
}

#[test]
fn docm_reports_version() {
    let (_dir, link) = shimtest::linked("docm");
    assert_reports_version("docm", link.to_str().expect("utf-8 link path"));
}

/// Parity requirement: a `docm` user must see the same behavior through
/// `devkit docs`, version output included.
#[test]
fn devkit_docs_reports_the_package_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .args(["docs", "--version"])
        .output()
        .expect("spawn devkit docs --version");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "`devkit docs --version` should exit 0, got: {text}"
    );
    assert!(
        text.contains(env!("CARGO_PKG_VERSION")),
        "`devkit docs --version` should print {}, got: {text}",
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
fn devkit_mcp_reports_version() {
    assert_reports_version("devkit-mcp", env!("CARGO_BIN_EXE_devkit-mcp"));
}

#[cfg(feature = "daemon")]
#[test]
fn devkitd_reports_version() {
    assert_reports_version("devkitd", env!("CARGO_BIN_EXE_devkitd"));
}
