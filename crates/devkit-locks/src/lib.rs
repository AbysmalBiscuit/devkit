pub mod ident;
pub mod model;
pub mod store;

use anyhow::{Context, Result};
use model::{AcquireOutcome, Conflict, Data, LockEntry};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Nearest ancestor of `start` containing a `.git` entry; `start` itself if none.
pub fn find_root_from(start: &Path) -> PathBuf {
    let mut dir = start;
    loop {
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => return start.to_path_buf(),
        }
    }
}

fn find_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("getting current dir")?;
    Ok(find_root_from(&cwd))
}

/// Lexically clean `abs` and express it relative to `root` ('/'-separated; the root
/// itself becomes "."). Errors if `abs` is not under `root`.
pub fn normalize_under_root(abs: &Path, root: &Path) -> Result<String> {
    let rel = abs
        .strip_prefix(root)
        .ok()
        .context("path is outside the project root")?;
    let mut parts: Vec<String> = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str().context("non-utf8 path")?.to_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            _ => {}
        }
    }
    Ok(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}

/// Resolve a CLI path argument (absolute or cwd-relative) to a root-relative key.
fn normalize_arg(arg: &str, cwd: &Path, root: &Path) -> Result<String> {
    let p = Path::new(arg);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    normalize_under_root(&abs, root)
}

struct Ctx {
    root: String,
    holder: String,
    paths: Vec<String>,
}

fn ctx(paths_in: &[String], as_flag: Option<&str>) -> Result<Ctx> {
    let cwd = std::env::current_dir().context("getting current dir")?;
    let root = find_root_from(&cwd);
    let mut paths = Vec::with_capacity(paths_in.len());
    for a in paths_in {
        paths.push(normalize_arg(a, &cwd, &root)?);
    }
    Ok(Ctx {
        root: root.to_string_lossy().into_owned(),
        holder: ident::identity(as_flag),
        paths,
    })
}

pub fn acquire(
    paths_in: &[String],
    as_flag: Option<&str>,
    note: Option<&str>,
    ttl: u64,
) -> Result<AcquireOutcome> {
    let c = ctx(paths_in, as_flag)?;
    let pid = ident::anchor_pid();
    store::with_lock(|d| {
        d.prune_dead(now());
        Ok(d.try_acquire(&c.root, &c.paths, &c.holder, pid, note, ttl, now()))
    })
}

pub fn check(paths_in: &[String], as_flag: Option<&str>) -> Result<Vec<Conflict>> {
    let c = ctx(paths_in, as_flag)?;
    store::with_lock(|d| {
        d.prune_dead(now());
        Ok(d.check(&c.root, &c.paths, &c.holder, now()))
    })
}

pub fn release(
    paths_in: &[String],
    as_flag: Option<&str>,
    force: bool,
) -> Result<(Vec<String>, Vec<String>)> {
    let c = ctx(paths_in, as_flag)?;
    store::with_lock(|d| Ok(d.do_release(&c.root, &c.paths, &c.holder, force)))
}

pub fn release_all(as_flag: Option<&str>) -> Result<Vec<String>> {
    let c = ctx(&[], as_flag)?;
    store::with_lock(|d| Ok(d.release_all(&c.root, &c.holder)))
}

/// Live locks for the current project root, or every project when `all`.
pub fn status(all: bool) -> Result<Vec<LockEntry>> {
    let root = find_root()?.to_string_lossy().into_owned();
    store::with_lock(|d: &mut Data| {
        d.prune_dead(now());
        let mut out: Vec<LockEntry> = d
            .locks
            .values()
            .filter(|e| all || e.root == root)
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            (a.root.as_str(), a.path.as_str()).cmp(&(b.root.as_str(), b.path.as_str()))
        });
        Ok(out)
    })
}

pub fn prune() -> Result<usize> {
    store::with_lock(|d| Ok(d.prune_dead(now())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("devkit-lib-{}-{}", std::process::id(), tag));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn find_root_walks_up_to_git() {
        let root = scratch("git-root");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let deep = root.join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_root_from(&deep), root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn find_root_falls_back_to_start_without_git() {
        let start = scratch("no-git");
        assert_eq!(find_root_from(&start), start);
        let _ = std::fs::remove_dir_all(&start);
    }

    #[test]
    fn normalize_makes_root_relative() {
        let root = Path::new("/repo");
        assert_eq!(
            normalize_under_root(Path::new("/repo/scenes/x.tscn"), root).unwrap(),
            "scenes/x.tscn"
        );
        assert_eq!(
            normalize_under_root(Path::new("/repo/./scenes/"), root).unwrap(),
            "scenes"
        );
        assert_eq!(normalize_under_root(Path::new("/repo"), root).unwrap(), ".");
    }

    #[test]
    fn normalize_rejects_outside_root() {
        assert!(normalize_under_root(Path::new("/elsewhere/x"), Path::new("/repo")).is_err());
    }
}
