//! Advisory locks for library cache and manifest transactions.
//!
//! Library lock files live outside their library directories so a held lock
//! survives a directory rename. Lock files are persistent: unlinking one can
//! let separate processes lock different inodes for the same logical path.

use anyhow::{Context, Result, bail};
use fd_lock::RwLock;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

const DIR: &str = "registry.locks";
const SUFFIX: &str = ".lock";
const MAX_COMPONENT_BYTES: usize = 255;

pub fn is_control(component: &str) -> bool {
    component == "registry" || component.starts_with("registry.")
}

pub fn lock_path(cache_root: &Path, lib: &str) -> Result<PathBuf> {
    lock_path_for_dir(cache_root, &crate::names::lib_dir(lib)?, lib)
}

pub fn manifest_lock_path(cache_root: &Path) -> PathBuf {
    cache_root.join(DIR).join("manifest.lock")
}

fn lock_path_for_dir(cache_root: &Path, dirname: &str, display_name: &str) -> Result<PathBuf> {
    let component = format!("{dirname}{SUFFIX}");
    if component.len() > MAX_COMPONENT_BYTES {
        bail!("library name `{display_name}` is too long to form a lock file name");
    }
    Ok(cache_root.join(DIR).join(component))
}

fn hold<T>(path: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let parent = path.parent().context("lock path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let mut lock = RwLock::new(file);
    if std::env::var_os(crate::barrier::VAR).is_some() {
        // A race test must distinguish a blocked contender from a process
        // that has merely started but has not attempted the lock yet.
        match lock.try_write() {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                crate::barrier::signal("contended")?;
            }
            Err(error) => {
                return Err(error).with_context(|| format!("probing {}", path.display()));
            }
            Ok(guard) => drop(guard),
        }
    }
    let _held = lock
        .write()
        .with_context(|| format!("locking {}", path.display()))?;
    f()
}

pub fn with_lib<T>(cache_root: &Path, lib: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    hold(&lock_path(cache_root, lib)?, f)
}

pub fn with_lib_dir<T>(
    cache_root: &Path,
    dirname: &str,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let lib = crate::names::decode(dirname);
    let validated = crate::names::lib_dir(&lib)?;
    hold(&lock_path_for_dir(cache_root, &validated, &lib)?, f)
}

/// Manifest mutations share one lock because global and project layers are
/// read as a merged view and distinct library locks cannot prevent lost
/// updates to the same file.
pub fn with_manifest<T>(cache_root: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    hold(&manifest_lock_path(cache_root), f)
}
