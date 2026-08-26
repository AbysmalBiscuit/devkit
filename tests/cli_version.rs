//! Every binary reports its version. The plugin's bootstrap hook pins binaries
//! to a release matching its own version, so a user (or the hook) needs a way
//! to see which version is actually on PATH.

#[path = "common/shimtest.rs"]
mod shimtest;
use std::process::Command;

fn version_output(exe: &str) -> (bool, String) {
    let out = Command::new(exe)
        .arg("--version")
        .env("DEVKIT_SKIP_AUTOLINK", "1")
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
    assert_eq!(
        text.trim(),
        format!("{name} {}", env!("CARGO_PKG_VERSION")),
        "`{name} --version` should name itself and this version"
    );
}

/// A version line names the command the user typed. A subcommand's default is
/// clap's `<parent>-<name>`, which spells commands that do not exist —
/// `devkit-ports`, `devkit-locks` — and sends anyone who reads one looking for
/// a binary by that name.
fn assert_subcommand_reports_version(sub: &str) {
    let out = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .args([sub, "--version"])
        .output()
        .unwrap_or_else(|e| panic!("spawn devkit {sub} --version: {e}"));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "`devkit {sub} --version` should exit 0, got: {text}"
    );
    assert_eq!(
        text.trim(),
        format!("devkit {sub} {}", env!("CARGO_PKG_VERSION")),
        "`devkit {sub} --version` should name the command the user typed"
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
    assert_subcommand_reports_version("ports");
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
    assert_subcommand_reports_version("locks");
}

#[test]
fn issue_reports_version() {
    let (_dir, link) = shimtest::linked("issue");
    assert_reports_version("issue", link.to_str().expect("utf-8 link path"));
}

/// Parity requirement: an `issue` user must see the same behavior through
/// `devkit issue`, version output included.
#[test]
fn devkit_issue_reports_the_package_version() {
    assert_subcommand_reports_version("issue");
}

#[test]
fn devrun_reports_version() {
    let (_dir, link) = shimtest::linked("devrun");
    assert_reports_version("devrun", link.to_str().expect("utf-8 link path"));
}

/// Parity requirement: a `devrun` user must see the same behavior through
/// `devkit run`, version output included.
#[test]
fn devkit_run_reports_the_package_version() {
    assert_subcommand_reports_version("run");
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
    assert_subcommand_reports_version("docs");
}

#[test]
fn devkit_mcp_reports_version() {
    let (_dir, link) = shimtest::linked("devkit-mcp");
    assert_reports_version("devkit-mcp", link.to_str().expect("utf-8 link path"));
}

/// Parity requirement: a `devkit-mcp` user must see the same behavior through
/// `devkit mcp`, version output included.
#[test]
fn devkit_mcp_reports_the_package_version() {
    assert_subcommand_reports_version("mcp");
}

#[cfg(feature = "daemon")]
#[test]
fn devkitd_reports_version() {
    assert_reports_version("devkitd", env!("CARGO_BIN_EXE_devkitd"));
}
