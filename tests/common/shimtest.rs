//! Run the built `devkit` under a shim name, the way an installed hardlink does.

use std::path::PathBuf;

/// Hardlink the built `devkit` as `name` inside a fresh temp dir. Returns the
/// guard and the link path; bind the guard for as long as the path is used, or
/// the directory is gone before the test runs anything.
pub fn linked(name: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let link = dir.path().join(exe);
    std::fs::hard_link(env!("CARGO_BIN_EXE_devkit"), &link)
        .unwrap_or_else(|e| panic!("hardlink devkit as {name}: {e}"));
    (dir, link)
}
