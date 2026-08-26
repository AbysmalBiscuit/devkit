//! Run the built `devkit` under a shim name, the way an installed hardlink does.
//!
//! Compile-time unused helpers are expected: different test binaries include
//! this module via `#[path]` and use different subsets of it.
#![allow(dead_code)]

use std::path::PathBuf;

/// Hardlink the built `devkit` as `name` inside a fresh temp dir. Returns the
/// guard and the link path; bind the guard for as long as the path is used, or
/// the directory is gone before the test runs anything.
///
/// The temp dir is created beside the `devkit` binary rather than under the
/// system temp dir: a hardlink cannot cross a filesystem/volume boundary, and
/// the system temp dir and the build output are not guaranteed to share one
/// (e.g. GitHub's Windows runners check the workspace out on `D:` while
/// `%TEMP%` resolves under `C:\Users\...\AppData\Local\Temp`).
pub fn linked(name: &str) -> (tempfile::TempDir, PathBuf) {
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_devkit"));
    let dir = tempfile::Builder::new()
        .prefix("shim")
        .tempdir_in(exe.parent().expect("binary has a parent dir"))
        .expect("temp dir");
    let link_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let link = dir.path().join(link_name);
    std::fs::hard_link(&exe, &link).unwrap_or_else(|e| panic!("hardlink devkit as {name}: {e}"));
    (dir, link)
}

/// Whether two paths name the same file on disk — a hardlink shares an inode
/// with its target on Unix and a file index on Windows. Delegated to the
/// `same-file` crate (the direct analogue of `links::same_file`, which this
/// mirrors for tests) rather than hand-rolled per-platform metadata
/// comparison; an error resolving either path is "not the same file".
pub fn same_inode(a: &std::path::Path, b: &std::path::Path) -> bool {
    same_file::is_same_file(a, b).unwrap_or(false)
}
