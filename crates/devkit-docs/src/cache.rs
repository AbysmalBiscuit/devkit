//! Per-library store: one bare (ideally blobless) clone plus detached
//! worktrees per resolved version, all under `~/.local/share/devkit/docs/<name>/`.

use crate::layout::Layout;
use crate::tags::TagPattern;
use anyhow::{Context, Result};
use devkit_common::cmd;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `~/.local/share/devkit/docs` (or `$XDG_DATA_HOME/devkit/docs`).
///
/// The store lives under the XDG *data* home, not the cache home: coding
/// agents commonly run with a blanket read-deny on `~/.cache`, and the whole
/// point of these checkouts is that an agent can search them.
pub fn docs_root() -> PathBuf {
    let root = devkit_common::paths::data_dir().join("docs");
    migrate_legacy_root(&root, &devkit_common::paths::cache_dir().join("docs"));
    root
}

/// Move a store left behind at the pre-data-home location (`~/.cache/devkit/
/// docs`) to `root` with one rename. Best-effort: an established root always
/// wins, and on any failure the store is simply refetched on demand.
fn migrate_legacy_root(root: &Path, legacy: &Path) {
    if root.exists() || !legacy.exists() {
        return;
    }
    if let Some(parent) = root.parent()
        && std::fs::create_dir_all(parent).is_ok()
        && let Err(e) = std::fs::rename(legacy, root)
    {
        eprintln!(
            "docm: could not move {} to {}: {e}",
            legacy.display(),
            root.display()
        );
    }
}

/// Recursive byte count; used by the doctor row.
pub fn dir_size(p: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else {
        return 0;
    };
    rd.flatten()
        .map(|e| {
            let path = e.path();
            if path.is_dir() {
                dir_size(&path)
            } else {
                path.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}

/// Per-lib sidecar: the cached tag pattern and detected layout per worktree.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_pattern: Option<TagPattern>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub layouts: BTreeMap<String, Layout>,
}

pub fn read_meta(lib_dir: &Path) -> Meta {
    std::fs::read_to_string(lib_dir.join("meta.toml"))
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn write_meta(lib_dir: &Path, m: &Meta) -> Result<()> {
    std::fs::create_dir_all(lib_dir)?;
    std::fs::write(lib_dir.join("meta.toml"), toml::to_string_pretty(m)?)
        .context("writing meta.toml")?;
    Ok(())
}

pub struct LibCache {
    pub dir: PathBuf,
}

impl LibCache {
    pub fn new(cache_root: &Path, name: &str) -> Self {
        Self {
            dir: cache_root.join(name),
        }
    }

    pub fn bare(&self) -> PathBuf {
        self.dir.join("repo.git")
    }

    fn bare_str(&self) -> String {
        self.bare().to_string_lossy().into_owned()
    }

    pub fn cloned(&self) -> bool {
        self.bare().is_dir()
    }

    /// Bare clone, blobless when the transport supports it. Filter support is
    /// best-effort: any failure retries as a plain bare clone.
    pub fn ensure_clone(&self, repo: &str) -> Result<()> {
        if self.cloned() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir)?;
        let dest = self.bare_str();
        if cmd::capture(
            "git",
            &["clone", "--bare", "--filter=blob:none", repo, dest.as_str()],
            None,
        )
        .is_err()
        {
            let _ = std::fs::remove_dir_all(self.bare());
            cmd::capture("git", &["clone", "--bare", repo, dest.as_str()], None)
                .with_context(|| format!("cloning {repo}"))?;
        }
        // Bare clones get no fetch refspec; heads must track the remote so
        // sync can fast-forward the default worktree.
        cmd::git(
            &[
                "config",
                "remote.origin.fetch",
                "+refs/heads/*:refs/heads/*",
            ],
            &dest,
        )?;
        Ok(())
    }

    pub fn fetch(&self) -> Result<()> {
        cmd::git(
            &["fetch", "--force", "--tags", "--prune", "origin"],
            &self.bare_str(),
        )
        .map(|_| ())
    }

    pub fn tags(&self) -> Result<Vec<String>> {
        Ok(cmd::git(&["tag", "--list"], &self.bare_str())?
            .lines()
            .map(str::to_string)
            .collect())
    }

    pub fn default_branch(&self) -> Result<String> {
        Ok(
            cmd::git(&["symbolic-ref", "--short", "HEAD"], &self.bare_str())?
                .trim()
                .to_string(),
        )
    }

    pub fn worktree_path(&self, dirname: &str) -> PathBuf {
        self.dir.join(dirname)
    }

    /// Commit a commitish names in the bare clone, or `None` when it names
    /// nothing (an unfetched tag, a deleted branch).
    pub fn rev(&self, commitish: &str) -> Option<String> {
        let spec = format!("{commitish}^{{commit}}");
        cmd::git(
            &["rev-parse", "--verify", "--quiet", &spec],
            &self.bare_str(),
        )
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    }

    /// Commit an existing worktree is checked out at.
    pub fn worktree_head(&self, dirname: &str) -> Option<String> {
        let path = self.worktree_path(dirname);
        if !path.is_dir() {
            return None;
        }
        cmd::git(&["rev-parse", "HEAD"], &path.to_string_lossy())
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Materialize a detached worktree at `commitish` if missing.
    pub fn ensure_worktree(&self, dirname: &str, commitish: &str) -> Result<PathBuf> {
        let path = self.worktree_path(dirname);
        if path.is_dir() {
            return Ok(path);
        }
        let p = path.to_string_lossy().into_owned();
        cmd::git(
            &["worktree", "add", "--detach", p.as_str(), commitish],
            &self.bare_str(),
        )
        .with_context(|| format!("materializing {dirname} at {commitish}"))?;
        Ok(path)
    }

    /// Ensure the `default` worktree exists and sits at `pin` (or the default
    /// branch tip). Fetch first when the tip should move.
    pub fn sync_default(&self, pin: Option<&str>) -> Result<PathBuf> {
        let target = match pin {
            Some(r) => r.to_string(),
            None => self.default_branch()?,
        };
        let path = self.worktree_path("default");
        if !path.is_dir() {
            return self.ensure_worktree("default", &target);
        }
        cmd::git(
            &["checkout", "--detach", target.as_str()],
            &path.to_string_lossy(),
        )?;
        Ok(path)
    }

    /// Worktree dirs currently on disk (including `default`, excluding the
    /// bare repo itself).
    pub fn version_worktrees(&self) -> Vec<(String, PathBuf)> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        rd.flatten()
            .filter(|e| e.path().is_dir())
            .filter(|e| e.file_name() != "repo.git")
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect()
    }

    pub fn remove_worktree(&self, dirname: &str) -> Result<()> {
        let p = self.worktree_path(dirname).to_string_lossy().into_owned();
        cmd::git(
            &["worktree", "remove", "--force", p.as_str()],
            &self.bare_str(),
        )
        .with_context(|| format!("removing worktree {dirname}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("devkit-docs-root-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn legacy_store_moves_to_new_root_once() {
        let base = unique_tmp("migrate");
        let legacy = base.join("cache/devkit/docs");
        let root = base.join("share/devkit/docs");
        std::fs::create_dir_all(legacy.join("kysely")).unwrap();
        std::fs::write(legacy.join("kysely/meta.json"), "{}").unwrap();

        migrate_legacy_root(&root, &legacy);
        assert!(root.join("kysely/meta.json").exists());
        assert!(!legacy.exists());

        // Established root wins even if a legacy dir reappears.
        std::fs::create_dir_all(legacy.join("other")).unwrap();
        migrate_legacy_root(&root, &legacy);
        assert!(root.join("kysely/meta.json").exists());
        assert!(!root.join("other").exists());
    }

    #[test]
    fn migration_is_a_noop_without_a_legacy_store() {
        let base = unique_tmp("noop");
        let root = base.join("share/devkit/docs");
        migrate_legacy_root(&root, &base.join("cache/devkit/docs"));
        assert!(!root.exists());
    }
}
