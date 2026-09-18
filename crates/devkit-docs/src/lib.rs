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

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rayon::prelude::*;

use crate::manifest::{Ecosystem, LibEntry};

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

/// Re-measure every materialized checkout of `name` and record what it finds,
/// under the library lock. Returns how many were measured.
///
/// This is the answer to a recorded size that is no longer believable. A
/// checkout that grew files `git status` does not report, anything ignored by
/// the library's own `.gitignore`, has no other way of being noticed: the
/// cleanliness sweep does not see them either.
pub fn refresh_sizes(cache_root: &Path, name: &str) -> Result<usize> {
    locks::with_lib(cache_root, name, || {
        let lib = cache::LibCache::new(cache_root, name)?;
        let mut meta = cache::read_meta(&lib.dir)?;
        let mut measured = 0;
        for (worktree, path) in lib.version_worktrees() {
            let Some(record) = meta.worktrees.get_mut(&worktree) else {
                continue;
            };
            record.bytes = Some(devkit_common::disk::dir_size(&path));
            measured += 1;
        }
        if measured > 0 {
            cache::write_meta(&lib.dir, &meta)?;
        }
        Ok(measured)
    })
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
        bytes: 0,
        unreferenced: 0,
        problems: Vec::new(),
    };
    // Sizes recorded when each checkout was materialized, substituted into the
    // walk below. The cache is still walked whole, so the shared object stores,
    // sidecars, lock files and whatever a broken cache has left lying around
    // all still land in the total.
    let mut known: std::collections::HashMap<std::path::PathBuf, u64> =
        std::collections::HashMap::new();
    let data = refs::RefStore::at(cache_root).snapshot();
    let referenced: std::collections::BTreeSet<(String, String)> = data
        .rows
        .iter()
        .map(|r| (r.lib.clone(), r.version.clone()))
        .collect();
    let Ok(rd) = std::fs::read_dir(cache_root) else {
        out.bytes = devkit_common::disk::dir_size(cache_root);
        return out;
    };
    let mut checkouts: Vec<Checkout> = Vec::new();
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
            let recorded = meta.worktrees.get(&wt).cloned();
            if let Some(bytes) = recorded.as_ref().and_then(|r| r.bytes) {
                known.insert(path.clone(), bytes);
            }
            checkouts.push(Checkout {
                label: format!("{dirname}/{wt}"),
                path,
                recorded,
            });
        }
    }
    out.bytes = devkit_common::disk::dir_size_with_known(cache_root, &known);
    out.problems.extend(sweep(&checkouts));
    out
}

/// One materialized checkout, addressed the way the report names it.
struct Checkout {
    label: String,
    path: PathBuf,
    recorded: Option<cache::WorktreeMeta>,
}

/// Inspect every checkout on the shared pool. Each [`inspect`] is two git
/// subprocesses and a cache holding a few dozen libraries makes this the
/// slowest thing `devkit doctor` does, so the calls overlap. rayon's `collect`
/// is ordered, so the report stays in cache order rather than falling into
/// whichever order the git calls finished in.
fn sweep(checkouts: &[Checkout]) -> Vec<String> {
    devkit_common::pool::install(|| {
        checkouts
            .par_iter()
            .flat_map_iter(|c| inspect(&c.label, &c.path, c.recorded.as_ref()))
            .collect()
    })
}

/// What is wrong with one materialized checkout, if anything: source that
/// differs from the commit, or a HEAD that is not the recorded one. Reported
/// rather than repaired — `doctor` diagnoses, it does not mutate the cache.
///
/// The sweep takes no library lock, so it reads a checkout a concurrent
/// `docm` is still materializing. Blocking a diagnostic behind a network
/// clone costs more than a warning the reader can re-run, so the drift row
/// says it may be transient rather than claiming a settled mismatch.
/// Both answers come from one `git status`. The v2 format's `--branch` header
/// carries the full HEAD oid, so the commit comparison reads off the same
/// output as the cleanliness check instead of costing a second process per
/// checkout.
fn inspect(label: &str, path: &Path, recorded: Option<&cache::WorktreeMeta>) -> Vec<String> {
    let mut problems = Vec::new();
    let status = match devkit_common::git::Git::at(path)
        .args(["status", "--porcelain=v2", "--branch"])
        .output()
    {
        Ok(status) => status,
        Err(error) => {
            problems.push(format!("{label} cannot be inspected: {error:#}"));
            return problems;
        }
    };
    let entries: Vec<String> = status.lines().filter_map(entry_line).collect();
    if !entries.is_empty() {
        problems.push(format!(
            "{label} has local modifications:\n    {}",
            entries.join("\n    ")
        ));
    }
    let Some(recorded) = recorded else {
        return problems;
    };
    // `(initial)` where an unborn HEAD has no commit to name. A detached
    // checkout never has one, but comparing that word against a sha would
    // report drift rather than the absence of a commit.
    match head_oid(&status) {
        Some("(initial)") => problems.push(format!("{label} has no commit at HEAD")),
        Some(head) if head != recorded.commit => problems.push(format!(
            "{label} is at {head}, but {} resolved to {} (may be transient during a \
             concurrent `docm` run)",
            recorded.raw_ref, recorded.commit
        )),
        Some(_) => {}
        None => problems.push(format!("{label} has no readable HEAD")),
    }
    problems
}

/// The commit `# branch.oid` names, which `--branch` puts ahead of the entries.
fn head_oid(status: &str) -> Option<&str> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("# branch.oid "))
        .map(str::trim)
}

/// One porcelain v2 entry rendered the way v1 rendered it, as the two status
/// letters and the path. The v2 line also carries file modes, object ids and a
/// rename score, none of which a health report has any use for. Header lines
/// (`# ...`) and anything unrecognized yield nothing, so a format git grows a
/// new record type for cannot turn into a phantom modification.
fn entry_line(line: &str) -> Option<String> {
    let (kind, rest) = line.split_once(' ')?;
    let (xy, path) = match kind {
        // `1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>`
        "1" => (rest.split(' ').next()?, rest.splitn(8, ' ').nth(7)?),
        // `2` adds a rename score before the path, and joins the path to the
        // one it came from with a tab.
        "2" => (rest.split(' ').next()?, rest.splitn(9, ' ').nth(8)?),
        // `u` carries three stages of mode and object id instead of two.
        "u" => (rest.split(' ').next()?, rest.splitn(10, ' ').nth(9)?),
        "?" | "!" => (kind, rest),
        _ => return None,
    };
    Some(format!("{xy} {}", path.replace('\t', " <- ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every v2 record type, reduced to the pair a reader acts on. The header
    /// lines have to yield nothing: counting one as a modification would put
    /// every clean checkout in the report.
    #[test]
    fn a_v2_entry_reduces_to_its_status_letters_and_path() {
        assert_eq!(entry_line("# branch.oid abc123"), None);
        assert_eq!(entry_line("# branch.head (detached)"), None);
        assert_eq!(
            entry_line("1 .M N... 100644 100644 100644 abc def src/lib.rs").as_deref(),
            Some(".M src/lib.rs")
        );
        assert_eq!(
            entry_line("2 R. N... 100644 100644 100644 abc def R100 new.rs\told.rs").as_deref(),
            Some("R. new.rs <- old.rs")
        );
        assert_eq!(
            entry_line("u UU N... 100644 100644 100644 100644 a b c both.rs").as_deref(),
            Some("UU both.rs")
        );
        assert_eq!(
            entry_line("? untracked.rs").as_deref(),
            Some("? untracked.rs")
        );
        assert_eq!(entry_line("! ignored.rs").as_deref(), Some("! ignored.rs"));
        assert_eq!(entry_line("x something git grew later"), None);
    }

    /// A path with spaces. v2 leaves it unquoted and last on the line, so the
    /// field split has to stop at the path rather than through it.
    #[test]
    fn a_v2_entry_keeps_a_path_that_has_spaces_in_it() {
        assert_eq!(
            entry_line("1 .M N... 100644 100644 100644 abc def docs/a b.md").as_deref(),
            Some(".M docs/a b.md")
        );
    }

    #[test]
    fn the_head_commit_comes_from_the_branch_header() {
        let status = "# branch.oid 4eff1fd1ee373352cca53c93b82bcd60ed7a00dc\n\
                      # branch.head (detached)\n\
                      ? new.rs\n";
        assert_eq!(
            head_oid(status),
            Some("4eff1fd1ee373352cca53c93b82bcd60ed7a00dc")
        );
        assert_eq!(head_oid("? new.rs\n"), None);
    }

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

    /// Asserts the set and the grouping, not a sequence: `read_dir` is sorted
    /// on NTFS and hash-ordered on ext4. None of these dirs is a git repo,
    /// which is what makes every one a problem line.
    #[test]
    fn the_sweep_names_every_checkout_and_keeps_each_library_together() {
        let root_dir = tempfile::tempdir().unwrap();
        let root = root_dir.path();
        let libs = ["axum", "serde", "tokio"];
        let mut expected: Vec<String> = Vec::new();
        for lib in libs {
            std::fs::create_dir_all(root.join(lib).join("repo.git")).unwrap();
            for wt in ["1.0.0", "2.0.0"] {
                std::fs::create_dir_all(root.join(lib).join(wt)).unwrap();
                expected.push(format!("{lib}/{wt}"));
            }
        }
        expected.sort();

        let s = doctor_summary(root);

        let labels: Vec<String> = s
            .problems
            .iter()
            .map(|p| p.split_whitespace().next().unwrap_or_default().to_string())
            .collect();
        let mut named = labels.clone();
        named.sort();
        assert_eq!(named, expected, "{:#?}", s.problems);

        let runs = labels.iter().fold(Vec::new(), |mut acc: Vec<&str>, label| {
            let lib = label.split('/').next().unwrap_or_default();
            if acc.last() != Some(&lib) {
                acc.push(lib);
            }
            acc
        });
        assert_eq!(
            runs.len(),
            libs.len(),
            "a library's checkouts were split apart: {labels:?}"
        );
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
