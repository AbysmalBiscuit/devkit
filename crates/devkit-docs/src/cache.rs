//! Per-library store: one bare (ideally blobless) clone plus detached
//! worktrees per resolved version, all under `~/.local/share/devkit/docs/<name>/`.

use crate::layout::Layout;
use crate::locks;
use crate::refs::{Data as RefData, RefStore};
use crate::tags::TagPattern;
use anyhow::{Context, Result, bail};
use devkit_common::cmd;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

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

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorktreeMeta {
    pub raw_ref: String,
    pub resolved_ref: String,
    pub commit: String,
}

/// Per-lib sidecar: repository identity and detected state per worktree.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_pattern: Option<TagPattern>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub layouts: BTreeMap<String, Layout>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub worktrees: BTreeMap<String, WorktreeMeta>,
}

pub fn read_meta(lib_dir: &Path) -> Meta {
    std::fs::read_to_string(lib_dir.join("meta.toml"))
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn write_meta(lib_dir: &Path, m: &Meta) -> Result<()> {
    ensure_dir_exact(lib_dir)?;
    let path = lib_dir.join("meta.toml");
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string_pretty(m)?).context("writing meta.toml")?;
    std::fs::rename(&tmp, &path).context("replacing meta.toml")?;
    Ok(())
}

pub fn create_dir_exact(parent: &Path, name: &str) -> Result<PathBuf> {
    let path = parent.join(name);
    std::fs::create_dir_all(&path).with_context(|| format!("creating {}", path.display()))?;
    assert_dir_exact(parent, name)?;
    Ok(path)
}

fn ensure_dir_exact(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().context("cache directory has no parent")?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("cache directory name is not valid UTF-8")?;
    create_dir_exact(parent, name)
}

fn assert_dir_exact(parent: &Path, name: &str) -> Result<()> {
    let entries = read_dir_entries(parent)?;
    if entries
        .iter()
        .any(|entry| entry.file_name() == std::ffi::OsStr::new(name))
    {
        return Ok(());
    }
    let existing: Vec<String> = entries
        .iter()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|entry| crate::names::fold_key(entry) == crate::names::fold_key(name))
        .collect();
    bail!(
        "this filesystem folds `{name}` onto {existing:?}; docm cannot keep them apart — \\
         rename the library or pin a ref whose name does not collide"
    );
}

fn read_dir_entries(parent: &Path) -> Result<Vec<std::fs::DirEntry>> {
    let entries =
        std::fs::read_dir(parent).with_context(|| format!("reading {}", parent.display()))?;
    collect_dir_entries(entries, parent)
}

fn collect_dir_entries(
    entries: impl Iterator<Item = std::io::Result<std::fs::DirEntry>>,
    parent: &Path,
) -> Result<Vec<std::fs::DirEntry>> {
    entries
        .map(|entry| entry.with_context(|| format!("reading entry in {}", parent.display())))
        .collect()
}

pub struct LibCache {
    pub dir: PathBuf,
}

impl LibCache {
    pub fn new(cache_root: &Path, name: &str) -> Result<Self> {
        Ok(Self {
            dir: cache_root.join(crate::names::lib_dir(name)?),
        })
    }

    pub fn from_dir(cache_root: &Path, dirname: &str) -> Self {
        Self {
            dir: cache_root.join(dirname),
        }
    }

    pub fn ensure_dir(&self) -> Result<PathBuf> {
        ensure_dir_exact(&self.dir)
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
    pub fn ensure_clone(&self, repo: &str, meta: &mut Meta) -> Result<()> {
        if self.cloned() {
            let actual = match meta.origin.clone() {
                Some(origin) => origin,
                None => cmd::git(&["config", "--get", "remote.origin.url"], &self.bare_str())
                    .with_context(|| format!("reading origin for {}", self.dir.display()))?
                    .trim()
                    .to_string(),
            };
            if actual != repo {
                bail!(
                    "{} was cloned from origin {actual}, but the manifest now asks for {repo}; \
                     remove the entry and re-add it to use a different repository",
                    self.dir.display()
                );
            }
            meta.origin = Some(actual);
            return Ok(());
        }
        self.ensure_dir()?;
        let dest = self.bare_str();
        let args = ["clone", "--bare", "--filter=blob:none", repo, dest.as_str()];
        let filtered = Command::new("git")
            .args(args)
            .output()
            .context("failed to spawn `git` for filtered bare clone")?;
        if !filtered.status.success() {
            match std::fs::remove_dir_all(self.bare()) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("removing incomplete clone at {dest}"));
                }
            }
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
        meta.origin = Some(repo.to_string());
        Ok(())
    }

    pub fn fetch(&self) -> Result<()> {
        cmd::git(
            &[
                "fetch",
                "--force",
                "--tags",
                "--prune",
                "--prune-tags",
                "origin",
            ],
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

    /// Resolve a ref to its canonical name and peeled commit.
    pub fn resolve_ref(&self, git_ref: &str) -> Result<(String, String)> {
        if let Some(found) = self.try_resolve_ref(git_ref)? {
            return Ok(found);
        }
        self.fetch()?;
        self.try_resolve_ref(git_ref)?.with_context(|| {
            format!("`{git_ref}` does not resolve to a commit, even after fetching")
        })
    }

    fn try_resolve_ref(&self, git_ref: &str) -> Result<Option<(String, String)>> {
        if git_ref.starts_with("refs/") {
            return Ok(self
                .peel_commit(git_ref)?
                .map(|commit| (git_ref.to_string(), commit)));
        }
        if git_ref.len() == 40
            && git_ref
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            return Ok(self
                .peel_commit(git_ref)?
                .map(|commit| (git_ref.to_string(), commit)));
        }
        let tag_ref = format!("refs/tags/{git_ref}");
        let head_ref = format!("refs/heads/{git_ref}");
        let tag = self.peel_commit(&tag_ref)?;
        let head = self.peel_commit(&head_ref)?;
        match (tag, head) {
            (Some(_), Some(_)) => {
                bail!("`{git_ref}` is both a tag and a branch; pin it as {tag_ref} or {head_ref}")
            }
            (Some(commit), None) => Ok(Some((tag_ref, commit))),
            (None, Some(commit)) => Ok(Some((head_ref, commit))),
            (None, None) => Ok(None),
        }
    }

    fn peel_commit(&self, git_ref: &str) -> Result<Option<String>> {
        let spec = format!("{git_ref}^{{commit}}");
        let bare = self.bare_str();
        let output = Command::new("git")
            .args([
                "-C",
                bare.as_str(),
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                spec.as_str(),
            ])
            .output()
            .with_context(|| format!("spawning git to resolve {git_ref} in {bare}"))?;
        if output.status.success() {
            let commit = String::from_utf8(output.stdout)
                .with_context(|| format!("reading commit for {git_ref} in {bare}"))?
                .trim()
                .to_string();
            return Ok(Some(commit));
        }
        if output.status.code() == Some(1) {
            return Ok(None);
        }
        bail!(
            "resolving {git_ref} in {bare} failed ({}):\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }

    /// Materialize at `commit`, re-pointing an existing worktree that drifted.
    pub fn ensure_at(&self, dirname: &str, commit: &str) -> Result<(PathBuf, bool)> {
        let path = self.worktree_path(dirname);
        if !path.is_dir() {
            let path_string = path.to_string_lossy().into_owned();
            cmd::git(
                &["worktree", "add", "--detach", path_string.as_str(), commit],
                &self.bare_str(),
            )
            .with_context(|| format!("materializing {dirname} at {commit}"))?;
            if let Err(exact_error) = assert_dir_exact(&self.dir, dirname) {
                cmd::git(
                    &["worktree", "remove", "--force", path_string.as_str()],
                    &self.bare_str(),
                )
                .with_context(|| {
                    format!("cleaning up inexact worktree {dirname} after: {exact_error}")
                })?;
                cmd::git(&["worktree", "prune"], &self.bare_str()).with_context(|| {
                    format!("pruning inexact worktree {dirname} after: {exact_error}")
                })?;
                return Err(exact_error);
            }
            return Ok((path, false));
        }
        assert_dir_exact(&self.dir, dirname)?;
        let path_string = path.to_string_lossy().into_owned();
        let head = cmd::git(&["rev-parse", "HEAD"], &path_string)
            .with_context(|| format!("reading HEAD for {dirname}"))?
            .trim()
            .to_string();
        if head == commit {
            return Ok((path, false));
        }
        cmd::git(&["checkout", "--detach", commit], path_string.as_str())
            .with_context(|| format!("re-pointing {dirname} from {head} to {commit}"))?;
        Ok((path, true))
    }

    /// Reject source that differs from the resolved commit.
    pub fn assert_clean(&self, path: &Path) -> Result<()> {
        let path_string = path.to_string_lossy().into_owned();
        let output = cmd::git(&["status", "--porcelain"], &path_string)
            .with_context(|| format!("checking {} for local modifications", path.display()))?;
        if !output.trim().is_empty() {
            bail!(
                "{} has local modifications:\n{}\nremove them, or `docm prune` and re-resolve",
                path.display(),
                output.trim()
            );
        }
        Ok(())
    }

    /// Worktree dirs currently on disk, excluding the bare repo itself.
    pub fn version_worktrees(&self) -> Vec<(String, PathBuf)> {
        if !self.bare().is_dir() {
            return Vec::new();
        }
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        rd.flatten()
            .filter(|e| e.path().is_dir())
            .filter(|e| e.file_name() != "repo.git")
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect()
    }

    /// Remove one checkout. The caller holds this library's lock, which is what
    /// keeps a concurrent resolve from materializing into the same directory.
    pub fn remove_worktree_locked(&self, dirname: &str) -> Result<()> {
        let p = self.worktree_path(dirname).to_string_lossy().into_owned();
        cmd::git(
            &["worktree", "remove", "--force", p.as_str()],
            &self.bare_str(),
        )
        .with_context(|| format!("removing worktree {dirname}"))?;
        Ok(())
    }

    pub fn remove_if_unreferenced(&self, snapshot: &RefData) -> Result<bool> {
        let cache_root = self
            .dir
            .parent()
            .context("library cache has no cache root")?
            .to_path_buf();
        let lib_dir = self
            .dir
            .file_name()
            .and_then(|name| name.to_str())
            .context("library cache name is not valid UTF-8")?
            .to_string();
        let lib = crate::names::decode(&lib_dir);
        locks::with_lib_dir(&cache_root, &lib_dir, || {
            let fresh = RefStore::at(&cache_root).snapshot();
            if fresh
                .rows
                .iter()
                .any(|row| row.lib == lib && !snapshot.rows.contains(row))
            {
                return Ok(false);
            }
            std::fs::remove_dir_all(&self.dir)
                .with_context(|| format!("deleting {}", self.dir.display()))?;
            Ok(true)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

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

    #[test]
    fn directory_entry_errors_keep_the_cache_path_context() {
        let parent = unique_tmp("read-dir-entry");
        let error = collect_dir_entries(
            std::iter::once(Err(io::Error::other("entry disappeared"))),
            &parent,
        )
        .unwrap_err();

        assert!(error.to_string().contains("reading entry in"));
        assert!(error.to_string().contains(&parent.display().to_string()));
    }
}
