//! The lookup facade: entry + CWD → version-correct checkout path.
//! Order: manual `ref` pin → lockfile version → tag probe → `default`
//! worktree fallback (with a warning). Every success records a reference row.

use crate::cache::{self, LibCache};
use crate::layout::{self, Layout};
use crate::lockfiles;
use crate::manifest::{Ecosystem, LibEntry};
use crate::refs::RefStore;
use crate::tags;
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
pub struct Resolved {
    pub name: String,
    /// Human-facing version: lockfile version, the pinned ref, or the branch
    /// name — or a commit, when the `default` worktree holds neither.
    pub version: String,
    /// Worktree dirname — also the version recorded in the reference registry.
    pub worktree: String,
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
    let repo = entry
        .repo
        .as_deref()
        .with_context(|| format!("lib `{}` has no repo url", entry.name))?;
    let lib = LibCache::new(cache_root, &entry.name);
    lib.ensure_clone(repo)?;
    let mut warnings = Vec::new();
    let mut meta = cache::read_meta(&lib.dir);

    let (worktree, version, path, project) = if let Some(pin) = entry.r#ref.as_deref() {
        let path = lib.ensure_worktree("default", pin)?;
        let version = default_version(&lib, &entry.name, pin, &mut warnings);
        ("default".to_string(), version, path, project_root(start))
    } else {
        let eco = entry
            .ecosystem
            .with_context(|| format!("lib `{}` has neither ecosystem nor ref", entry.name))?;
        let hit = if eco == Ecosystem::Git {
            None
        } else {
            lockfiles::find_version(start, eco, &entry.package_name())
        };
        match hit {
            Some((root, versions)) => {
                let v = lockfiles::highest(versions.clone()).expect("non-empty versions");
                if versions.len() > 1 {
                    warnings.push(format!(
                        "lockfile holds {} versions of {}; using {v}",
                        versions.len(),
                        entry.package_name()
                    ));
                }
                match locate_tag(&lib, &mut meta, &entry.package_name(), &v)? {
                    Some(tag) => {
                        let path = lib.ensure_worktree(&v, &tag)?;
                        (v.clone(), v, path, root)
                    }
                    None => {
                        warnings.push(format!(
                            "no git tag found for {} {v}; falling back to the default branch",
                            entry.name
                        ));
                        let (w, ver, p) = default_worktree(&lib, &entry.name, &mut warnings)?;
                        (w, ver, p, root)
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
                let (w, ver, p) = default_worktree(&lib, &entry.name, &mut warnings)?;
                (w, ver, p, project_root(start))
            }
        }
    };

    RefStore::at(cache_root).commit(|d| {
        d.record(&project.to_string_lossy(), &entry.name, &worktree);
        Ok(())
    })?;

    let detected = match meta.layouts.get(&worktree) {
        Some(l) => l.clone(),
        None => {
            let l = layout::detect(&path);
            meta.layouts.insert(worktree.clone(), l.clone());
            l
        }
    };
    cache::write_meta(&lib.dir, &meta)?;

    Ok(Resolved {
        name: entry.name.clone(),
        version,
        worktree,
        path,
        layout: layout::with_overrides(detected, entry),
        notes: entry.notes.clone(),
        warnings,
    })
}

fn default_worktree(
    lib: &LibCache,
    name: &str,
    warnings: &mut Vec<String>,
) -> Result<(String, String, PathBuf)> {
    let branch = lib.default_branch()?;
    let path = lib.ensure_worktree("default", &branch)?;
    let version = default_version(lib, name, &branch, warnings);
    Ok(("default".to_string(), version, path))
}

/// What the `default` worktree actually holds, named as `target` when the two
/// agree and as a commit otherwise.
///
/// Every ref-pinned and fallback lookup shares this one worktree, and only
/// `docm sync` re-points it — so an edited pin, or a branch that moved under a
/// fetch, leaves the previous commit checked out. Reporting `target` there
/// would name a version the returned path does not contain.
fn default_version(lib: &LibCache, name: &str, target: &str, warnings: &mut Vec<String>) -> String {
    let (Some(want), Some(have)) = (lib.rev(target), lib.worktree_head("default")) else {
        return target.to_string();
    };
    if want == have {
        return target.to_string();
    }
    let short: String = have.chars().take(12).collect();
    warnings.push(format!(
        "{name} points at {target}, but its `default` worktree is checked out at \
         {short}; run `docm sync {name}` to re-point it"
    ));
    short
}

/// Cached pattern first; then a probe; then one fetch (the version may be
/// newer than the last fetch) and a final probe.
fn locate_tag(
    lib: &LibCache,
    meta: &mut cache::Meta,
    package: &str,
    version: &str,
) -> Result<Option<String>> {
    let tags_now = lib.tags()?;
    if let Some(p) = meta.tag_pattern {
        let t = tags::apply(p, package, version);
        if tags_now.contains(&t) {
            return Ok(Some(t));
        }
    }
    if let Some((p, t)) = tags::find(&tags_now, package, version) {
        meta.tag_pattern = Some(p);
        return Ok(Some(t));
    }
    lib.fetch()?;
    if let Some((p, t)) = tags::find(&lib.tags()?, package, version) {
        meta.tag_pattern = Some(p);
        return Ok(Some(t));
    }
    Ok(None)
}
