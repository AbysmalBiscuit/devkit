pub mod barrier;
pub mod cache;
pub mod importers;
pub mod layout;
pub mod lockfiles;
pub mod locks;
pub mod lookup;
pub mod manifest;
pub mod names;
pub mod pins;
pub mod refs;
pub mod resolve;
pub mod tags;
pub mod upgrade;

use crate::manifest::{Ecosystem, LibEntry};
use anyhow::{Context, Result, bail};
use std::path::Path;

/// Which manifest a registration targets: the machine-owned global file, or a
/// repo-committed `devkit.toml`.
#[derive(Clone, Copy)]
pub enum ManifestTarget<'a> {
    Global(&'a Path),
    /// The `devkit.toml` a `--project` registration edits.
    Project(&'a Path),
}

impl<'a> ManifestTarget<'a> {
    pub fn path(&self) -> &'a Path {
        match self {
            Self::Global(path) | Self::Project(path) => path,
        }
    }

    /// What this target holds for `name` right now, in the form a rollback can
    /// put back unchanged.
    fn snapshot(&self, name: &str) -> Result<Snapshot<'a>> {
        Ok(match self {
            Self::Global(path) => Snapshot::Global(
                path,
                manifest::load_global(path)?
                    .libs
                    .into_iter()
                    .find(|lib| lib.name == name),
            ),
            Self::Project(path) => Snapshot::Project(path, manifest::project_entry(path, name)?),
        })
    }

    fn upsert(&self, entry: &LibEntry, cache_root: &Path) -> Result<()> {
        match self {
            Self::Global(path) => manifest::upsert_global(path, entry, cache_root),
            Self::Project(path) => manifest::upsert_project(path, entry, cache_root),
        }
    }

    fn remove(&self, name: &str, cache_root: &Path) -> Result<bool> {
        match self {
            Self::Global(path) => manifest::remove_global(path, name, cache_root),
            Self::Project(path) => manifest::remove_project(path, name, cache_root),
        }
    }
}

/// What a manifest held for one library before a registration touched it.
///
/// The project variant keeps the entry's raw table rather than a parsed
/// `LibEntry`: a `devkit.toml` is hand-maintained and repo-committed, so a
/// rollback that re-serialized the entry would reorder its keys and drop
/// comments inside it — a diff left behind by a command that failed.
enum Snapshot<'a> {
    Global(&'a Path, Option<LibEntry>),
    Project(&'a Path, Option<toml_edit::Table>),
}

impl Snapshot<'_> {
    fn restore(&self, name: &str, cache_root: &Path) -> Result<()> {
        match self {
            Self::Global(path, Some(entry)) => manifest::upsert_global(path, entry, cache_root),
            Self::Global(path, None) => manifest::remove_global(path, name, cache_root).map(|_| ()),
            Self::Project(path, Some(table)) => {
                manifest::put_project_entry(path, name, table, cache_root)
            }
            Self::Project(path, None) => {
                manifest::remove_project(path, name, cache_root).map(|_| ())
            }
        }
    }
}

const PROJECT_NEEDS_REF: &str = "\
--project needs an explicit --ref for a git URL entry

devkit.toml is shared policy — an inferred default branch would read as a
team decision. Take the ref from this project's dependency or release
policy; don't guess `main`. Then rerun:

    docm add <url> --project --ref <tag|branch|sha>";

pub struct Added {
    pub resolved: resolve::Resolved,
    /// The ref was derived from the repo's default branch rather than given.
    pub inferred_ref: bool,
}

/// Register `entry` and materialize its checkout as one transaction: a
/// registration that cannot be materialized is not a registration, so any
/// failure restores the manifest to what it held before.
///
/// The whole transaction runs under the library lock, which is also what keeps
/// a concurrent `rm` of the same library from reading the manifest before the
/// entry lands and writing back a copy without it. The manifest lock cannot be
/// held instead: materialization clones over the network.
pub fn add_library(
    target: ManifestTarget<'_>,
    cache_root: &Path,
    start: &Path,
    entry: &LibEntry,
    opts: &resolve::Options,
) -> Result<Added> {
    locks::with_lib(cache_root, &entry.name, || {
        let previous = target.snapshot(&entry.name)?;
        barrier::signal("ready")?;
        barrier::wait("go")?;
        let mut entry = entry.clone();
        let inferred_ref = pin_default_branch(target, cache_root, &mut entry)?;
        target.upsert(&entry, cache_root)?;
        match resolve::resolve_locked(&entry, start, cache_root, opts) {
            Ok(resolved) => Ok(Added {
                resolved,
                inferred_ref,
            }),
            Err(error) => Err(restore(&previous, cache_root, &entry.name, error)),
        }
    })
}

/// Remove `name` from `target`, reporting whether an entry was there.
///
/// Takes the library lock and no manifest lock of its own: the manifest
/// mutators take that one themselves, and `fd-lock` is not reentrant.
pub fn rm_library(target: ManifestTarget<'_>, cache_root: &Path, name: &str) -> Result<bool> {
    locks::with_lib(cache_root, name, || target.remove(name, cache_root))
}

/// Pin a ref-less git entry to the repo's current default branch, reporting
/// whether it did. Deriving the value from remote `HEAD` is what keeps a git
/// entry from ever sitting in the manifest unpinned.
fn pin_default_branch(
    target: ManifestTarget<'_>,
    cache_root: &Path,
    entry: &mut LibEntry,
) -> Result<bool> {
    if entry.ecosystem != Some(Ecosystem::Git) || entry.r#ref.is_some() {
        return Ok(false);
    }
    if let ManifestTarget::Project(_) = target {
        bail!(PROJECT_NEEDS_REF);
    }
    let repo = entry
        .repo
        .as_deref()
        .with_context(|| format!("lib `{}` has no repo url", entry.name))?;
    let lib = cache::LibCache::new(cache_root, &entry.name)?;
    let mut meta = cache::read_meta(&lib.dir)?;
    lib.ensure_clone(repo, &mut meta)?;
    entry.r#ref = Some(lib.default_branch()?);
    Ok(true)
}

/// Put the manifest back the way the failed registration found it. A failure
/// here outranks the original error in the report: an entry naming a library
/// that was never materialized is what every later command reads.
fn restore(
    previous: &Snapshot<'_>,
    cache_root: &Path,
    name: &str,
    error: anyhow::Error,
) -> anyhow::Error {
    let (Snapshot::Global(path, _) | Snapshot::Project(path, _)) = previous;
    match previous.restore(name, cache_root) {
        Ok(()) => error,
        Err(failure) => error.context(format!(
            "`{name}` is left registered in {} but was not materialized — \
             restoring it failed too: {failure:#}",
            path.display()
        )),
    }
}

pub struct DocsDoctor {
    pub libs: usize,
    pub bytes: u64,
    pub unreferenced: usize,
    /// One line per checkout that is dirty or is not at the commit `meta.toml`
    /// records for it.
    pub problems: Vec<String>,
}

/// Health summary for `devkit doctor`: lib count, cache size, version
/// worktrees no registry row references, and a sweep of every materialized
/// checkout for cleanliness and commit correctness. Resolution verifies the
/// one checkout it returns; this covers the ones no workspace resolves.
pub fn doctor_summary(cache_root: &Path) -> DocsDoctor {
    let mut out = DocsDoctor {
        libs: 0,
        bytes: cache::dir_size(cache_root),
        unreferenced: 0,
        problems: Vec::new(),
    };
    let data = refs::RefStore::at(cache_root).snapshot();
    let referenced: std::collections::BTreeSet<(String, String)> = data
        .rows
        .iter()
        .map(|r| (r.lib.clone(), r.version.clone()))
        .collect();
    let Ok(rd) = std::fs::read_dir(cache_root) else {
        return out;
    };
    for e in rd.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let dirname = e.file_name().to_string_lossy().into_owned();
        if locks::is_control(&dirname) {
            continue;
        }
        let name = names::decode(&dirname);
        out.libs += 1;
        let lib = cache::LibCache::from_dir(cache_root, &dirname);
        // `doctor` is the command run to diagnose a broken cache, so one
        // library's unreadable sidecar is a row in the report rather than the
        // end of it. Without the recorded commits, the checkouts below are
        // still swept for local modifications.
        let meta = match cache::read_meta(&lib.dir) {
            Ok(meta) => meta,
            Err(error) => {
                out.problems.push(format!("{dirname}: {error:#}"));
                cache::Meta::default()
            }
        };
        for (wt, path) in lib.version_worktrees() {
            if !referenced.contains(&(name.clone(), wt.clone())) {
                out.unreferenced += 1;
            }
            out.problems.extend(inspect(
                &format!("{dirname}/{wt}"),
                &path,
                meta.worktrees.get(&wt),
            ));
        }
    }
    out
}

/// What is wrong with one materialized checkout, if anything: source that
/// differs from the commit, or a HEAD that is not the recorded one. Reported
/// rather than repaired — `doctor` diagnoses, it does not mutate the cache.
///
/// The sweep takes no library lock, so it reads a checkout a concurrent
/// `docm` is still materializing. Blocking a diagnostic behind a network
/// clone costs more than a warning the reader can re-run, so the drift row
/// says it may be transient rather than claiming a settled mismatch.
fn inspect(label: &str, path: &Path, recorded: Option<&cache::WorktreeMeta>) -> Vec<String> {
    let mut problems = Vec::new();
    let dir = path.to_string_lossy().into_owned();
    match devkit_common::cmd::git(&["status", "--porcelain"], &dir) {
        Ok(status) if !status.trim().is_empty() => problems.push(format!(
            "{label} has local modifications:\n    {}",
            status.trim().replace('\n', "\n    ")
        )),
        Ok(_) => {}
        Err(error) => problems.push(format!("{label} cannot be inspected: {error:#}")),
    }
    let Some(recorded) = recorded else {
        return problems;
    };
    match devkit_common::cmd::git(&["rev-parse", "HEAD"], &dir) {
        Ok(head) if head.trim() != recorded.commit => problems.push(format!(
            "{label} is at {}, but {} resolved to {} (may be transient during a \
             concurrent `docm` run)",
            head.trim(),
            recorded.raw_ref,
            recorded.commit
        )),
        Ok(_) => {}
        Err(error) => problems.push(format!("{label} has no readable HEAD: {error:#}")),
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_summary_counts_libs_and_unreferenced_worktrees() {
        let root_dir = tempfile::tempdir().unwrap();
        let root = root_dir.path();
        // One lib with a referenced worktree, an unreferenced one, and default.
        for wt in ["1.0.0", "2.0.0", "default", "repo.git"] {
            std::fs::create_dir_all(root.join("tokio").join(wt)).unwrap();
        }
        std::fs::write(root.join("tokio/1.0.0/f"), "x").unwrap();
        refs::RefStore::at(root)
            .commit(|d| {
                d.record("/some/project", "tokio", "1.0.0", "v1.0.0", "aaa");
                Ok(())
            })
            .unwrap();
        let s = doctor_summary(root);
        assert_eq!(s.libs, 1);
        assert_eq!(s.unreferenced, 2); // 2.0.0 and default; repo.git is not a checkout
        assert!(s.bytes > 0);
    }

    #[test]
    fn doctor_summary_skips_registry_lock_directory() {
        let root_dir = tempfile::tempdir().unwrap();
        let root = root_dir.path();
        std::fs::create_dir_all(root.join("tokio/default")).unwrap();
        std::fs::create_dir_all(root.join("registry.locks")).unwrap();

        let summary = doctor_summary(root);

        assert_eq!(summary.libs, 1);
    }
}
