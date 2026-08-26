mod common;

use common::shimtest;
use std::process::Command;

#[test]
fn portm_shim_parses_portm_arguments() {
    let (_dir, link) = shimtest::linked("portm");
    let out = Command::new(&link)
        .args(["--help"])
        .output()
        .expect("spawn portm shim");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "portm --help exited non-zero: {text}");
    assert!(
        text.contains("Port registry"),
        "shim should show portm's own about text: {text}"
    );
    assert!(
        !text.contains("Configure and diagnose"),
        "shim must not show devkit's about text: {text}"
    );
}

/// `portm` with no subcommand shows status, which needs no project to exit 0
/// against an empty registry.
#[test]
fn portm_shim_defaults_to_status() {
    let (_dir, link) = shimtest::linked("portm");
    let state = tempfile::tempdir().expect("state dir");
    let out = Command::new(&link)
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .output()
        .expect("spawn bare portm shim");
    assert!(
        out.status.success(),
        "bare portm should run status: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn portm_shim_reports_the_package_version() {
    let (_dir, link) = shimtest::linked("portm");
    let out = Command::new(&link)
        .arg("--version")
        .output()
        .expect("spawn portm --version");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains(env!("CARGO_PKG_VERSION")),
        "portm --version should print {}, got: {text}",
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
fn devkit_ports_reaches_the_same_command() {
    let out = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .args(["ports", "--help"])
        .output()
        .expect("spawn devkit ports --help");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains("Port registry"),
        "devkit ports should show portm's about text: {text}"
    );
}
