//! The lookup facade: entry + CWD → version-correct checkout path.
//! Order: manual `ref` pin → lockfile version → tag probe → default-branch
//! fallback (with a warning). Every success records a reference row.

use crate::cache::{self, LibCache};
use crate::importers;
use crate::layout::{self, Layout};
use crate::locks;
use crate::manifest::{Ecosystem, LibEntry};
use crate::names;
use crate::refs::RefStore;
use crate::tags;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Repaired,
}

#[derive(Debug, Serialize)]
pub struct Resolved {
    pub name: String,
    /// Human-facing version: lockfile version, the pinned ref, or the branch name.
    pub version: String,
    /// Worktree dirname — also the version recorded in the reference registry.
    pub worktree: String,
    pub git_ref: String,
    pub commit: String,
    pub status: Status,
    pub origin: String,
    pub path: PathBuf,
    pub layout: Layout,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Nearest dir containing `devkit.toml` walking up, else `start` itself —
/// the project identity recorded in the reference registry when no lockfile
/// pins the version.
pub fn project_root(start: &Path) -> PathBuf {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join("devkit.toml").is_file() {
            return d.to_path_buf();
        }
        dir = d.parent();
    }
    start.to_path_buf()
}

pub fn resolve(entry: &LibEntry, start: &Path, cache_root: &Path) -> Result<Resolved> {
    locks::with_lib(cache_root, &entry.name, || {
        resolve_locked(entry, start, cache_root)
    })
}

pub fn resolve_locked(entry: &LibEntry, start: &Path, cache_root: &Path) -> Result<Resolved> {
    let repo = entry
        .repo
        .as_deref()
        .with_context(|| format!("lib `{}` has no repo url", entry.name))?;
    let lib = LibCache::new(cache_root, &entry.name)?;
    let mut warnings = Vec::new();
    let mut meta = cache::read_meta(&lib.dir);
    lib.ensure_clone(repo, &mut meta)?;
    let origin = meta
        .origin
        .clone()
        .context("clone origin was not recorded")?;

    let (git_ref, version, project) = if let Some(pin) = entry.r#ref.as_deref() {
        (pin.to_string(), pin.to_string(), project_root(start))
    } else {
        let eco = entry
            .ecosystem
            .with_context(|| format!("lib `{}` has neither ecosystem nor ref", entry.name))?;
        let selection = if eco == Ecosystem::Git || !has_importer_manifest(start, eco) {
            None
        } else {
            Some(importers::select(start, eco, &entry.package_name())?)
        };
        match selection {
            Some(selection) => {
                let v = selection.version;
                warnings.push(selection.source);
                match locate_tag(&lib, &mut meta, &entry.package_name(), &v)? {
                    Some(tag) => (tag, v, selection.workspace),
                    None => {
                        warnings.push(format!(
                            "no git tag found for {} {v}; falling back to the default branch",
                            entry.name
                        ));
                        let branch = lib.default_branch()?;
                        (branch.clone(), branch, selection.workspace)
                    }
                }
            }
            None => {
                warnings.push(if eco == Ecosystem::Git {
                    format!("no ref pinned for {}; using the default branch", entry.name)
                } else {
                    format!(
                        "no lockfile pins {}; using the default branch",
                        entry.package_name()
                    )
                });
                let branch = lib.default_branch()?;
                (branch.clone(), branch, project_root(start))
            }
        }
    };

    let worktree = names::checkout_dir(&git_ref)?;
    let (canonical, commit) = lib.resolve_ref(&git_ref)?;
    let previous = meta.worktrees.get(&worktree).cloned();
    if let Some(previous) = &previous
        && previous.resolved_ref != canonical
    {
        bail!(
            "`{git_ref}` previously resolved to {} and now resolves to {canonical}; \
             the pin changed kind upstream — re-pin it explicitly",
            previous.resolved_ref
        );
    }
    let moved_tag_from = previous
        .as_ref()
        .filter(|previous| previous.commit != commit && canonical.starts_with("refs/tags/"))
        .map(|previous| previous.commit.clone());
    let (path, repaired) = lib.ensure_at(&worktree, &commit)?;
    lib.assert_clean(&path)?;
    if repaired && let Some(previous_commit) = moved_tag_from {
        warnings.push(format!(
            "tag {git_ref} moved {previous_commit} → {commit} upstream; {worktree} re-pointed"
        ));
    }
    meta.worktrees.insert(
        worktree.clone(),
        cache::WorktreeMeta {
            raw_ref: git_ref.clone(),
            resolved_ref: canonical,
            commit: commit.clone(),
        },
    );
    let status = if repaired {
        Status::Repaired
    } else {
        Status::Ok
    };

    let detected = match meta.layouts.get(&worktree) {
        Some(l) => l.clone(),
        None => {
            let l = layout::detect(&path);
            meta.layouts.insert(worktree.clone(), l.clone());
            l
        }
    };
    cache::write_meta(&lib.dir, &meta)?;
    crate::barrier::signal("materialized")?;
    crate::barrier::wait("commit")?;
    RefStore::at(cache_root).commit(|data| {
        let workspace = project.to_string_lossy();
        data.record(&workspace, &entry.name, &worktree, &git_ref, &commit);
        data.retire_legacy(&workspace, &entry.name);
        Ok(())
    })?;

    Ok(Resolved {
        name: entry.name.clone(),
        version,
        worktree,
        git_ref,
        commit,
        status,
        origin,
        path,
        layout: layout::with_overrides(detected, entry),
        notes: entry.notes.clone(),
        warnings,
    })
}

fn has_importer_manifest(start: &Path, ecosystem: Ecosystem) -> bool {
    let manifest = match ecosystem {
        Ecosystem::Rust => "Cargo.toml",
        Ecosystem::Js => "package.json",
        Ecosystem::Python => "pyproject.toml",
        Ecosystem::Git => return false,
    };
    start
        .ancestors()
        .any(|directory| directory.join(manifest).is_file())
}

/// Probe tag patterns in priority order, using the cached pattern at its
/// priority position; then fetch once and probe again.
fn locate_tag(
    lib: &LibCache,
    meta: &mut cache::Meta,
    package: &str,
    version: &str,
) -> Result<Option<String>> {
    let tags_now = lib.tags()?;
    if let Some((p, t)) = tags::find_with_hint(&tags_now, package, version, meta.tag_pattern) {
        meta.tag_pattern = Some(p);
        return Ok(Some(t));
    }
    lib.fetch()?;
    if let Some((p, t)) = tags::find_with_hint(&lib.tags()?, package, version, meta.tag_pattern) {
        meta.tag_pattern = Some(p);
        return Ok(Some(t));
    }
    Ok(None)
}
