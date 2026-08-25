//! Scratch space for tests, removed when the test that made it ends.
//!
//! Every devkit test that needs something on disk gets it from here: [`dir`]
//! for a directory, [`path`] for one file inside one. Both guards deref to
//! [`Path`], so a binding reads like the [`PathBuf`] it replaced —
//! `d.join("x")`, `&d`, `d.to_str()` — while `Drop` removes the tree.
//!
//! Keep the binding alive for as long as the path is used. Dropping a guard
//! deletes its directory, so `dir("x").join("y")` is a use-after-free in path
//! form, and a helper that returns a path derived from a guard must hand back
//! the guard alongside it.
//!
//! [`PathBuf`]: std::path::PathBuf

use std::ffi::OsStr;
use std::path::Path;

/// A scratch directory that removes itself when dropped.
pub struct TmpDir(tempfile::TempDir);

impl TmpDir {
    /// The guard's path, for the rare caller that needs it spelled out.
    /// Prefer the [`Deref`] — `&d` and `d.join(..)` already reach it.
    ///
    /// [`Deref`]: std::ops::Deref
    pub fn path(&self) -> &Path {
        self.0.path()
    }
}

impl std::ops::Deref for TmpDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        self.0.path()
    }
}

impl AsRef<Path> for TmpDir {
    fn as_ref(&self) -> &Path {
        self.0.path()
    }
}

/// So a guard can be handed straight to `Command::env`, `Command::arg` and the
/// other `OsStr` sinks a test path reaches.
impl AsRef<OsStr> for TmpDir {
    fn as_ref(&self) -> &OsStr {
        self.0.path().as_os_str()
    }
}

impl std::fmt::Debug for TmpDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.0.path(), f)
    }
}

/// A fresh empty directory under the system temp directory, named
/// `<prefix>-<random>`, removed when the returned guard drops.
///
/// The random suffix is what makes it safe for two tests in one binary — or
/// two test binaries `cargo test` runs at once — to ask for the same prefix.
///
/// # Panics
///
/// If the directory cannot be created. A test that cannot get scratch space has
/// nothing to assert.
pub fn dir(prefix: &str) -> TmpDir {
    TmpDir(
        tempfile::Builder::new()
            .prefix(&format!("{prefix}-"))
            .tempdir()
            .unwrap_or_else(|e| panic!("temp dir for {prefix}: {e}")),
    )
}

/// A path inside a scratch directory, carrying the guard that keeps it alive.
///
/// For the tests that want one *file* rather than a directory: the binding
/// reads as the path it names, and the directory around it goes away with it.
pub struct TmpPath {
    path: std::path::PathBuf,
    _dir: TmpDir,
}

impl std::ops::Deref for TmpPath {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TmpPath {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<OsStr> for TmpPath {
    fn as_ref(&self) -> &OsStr {
        self.path.as_os_str()
    }
}

impl std::fmt::Debug for TmpPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.path, f)
    }
}

/// `<prefix>-<random>/<name>` — the named entry need not exist; the directory
/// holding it does, and is removed when the returned guard drops.
///
/// # Panics
///
/// If the directory cannot be created.
pub fn path(prefix: &str, name: &str) -> TmpPath {
    let dir = self::dir(prefix);
    TmpPath {
        path: dir.join(name),
        _dir: dir,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_outlives_its_guard_by_nothing() {
        let leaked = {
            let d = dir("testtmp-drop");
            std::fs::write(d.join("f"), "x").unwrap();
            assert!(d.join("f").exists());
            d.to_path_buf()
        };
        assert!(!leaked.exists(), "{} survived its guard", leaked.display());
    }

    #[test]
    fn one_prefix_serves_two_concurrent_callers() {
        let a = dir("testtmp-same");
        let b = dir("testtmp-same");
        assert_ne!(a.path(), b.path());
    }

    #[test]
    fn a_named_path_sits_inside_a_directory_that_exists() {
        let p = path("testtmp-file", "secrets.toml");
        assert!(!p.exists(), "the named entry is the caller's to create");
        assert!(p.parent().unwrap().is_dir());
    }
}
