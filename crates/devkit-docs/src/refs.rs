//! Reference registry: which project roots resolved which lib versions.
//! One flock-guarded JSON file at the cache root; concurrent agent sessions
//! race docm, so every read-modify-write goes through
//! `devkit_common::store::with_lock`. No devkitd gate — no daemon serves this
//! file. A holder is live iff its project root path still exists (the same
//! model as the ports registry).

use crate::cache;
use crate::lockfiles;
use crate::locks;
use crate::manifest;
use anyhow::{Context, Result};
use devkit_common::store::{self, Document};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefRow {
    /// The workspace directory resolution selected the version from.
    pub project: String,
    pub lib: String,
    /// Checkout dirname the resolver materialized.
    pub version: String,
    #[serde(default)]
    pub git_ref: String,
    /// Empty in a row written before checkouts carried an identity. Such a row
    /// keeps protecting its directory until a workspace-keyed row supersedes it.
    #[serde(default)]
    pub commit: String,
    pub resolved_at: u64,
    #[serde(default)]
    pub revision: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Data {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub rows: Vec<RefRow>,
}

impl Document for Data {
    fn stamp_version(&mut self) {
        self.version = SCHEMA;
    }
    fn salvage(_raw: &str) -> Option<Self> {
        None
    }
    fn label() -> &'static str {
        "docs registry"
    }
    fn len(&self) -> usize {
        self.rows.len()
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Data {
    /// Upsert keyed on (workspace, lib), recording the checkout `dirname` the
    /// resolver materialized and the identity it materialized there.
    pub fn record(
        &mut self,
        workspace: &str,
        lib: &str,
        dirname: &str,
        git_ref: &str,
        commit: &str,
    ) {
        self.record_at(workspace, lib, dirname, git_ref, commit, now());
    }

    /// A row in the shape written before checkouts carried an identity: keyed
    /// by the lockfile directory, with no ref and no commit.
    pub fn record_legacy(&mut self, project: &str, lib: &str, version: &str) {
        self.record_at(project, lib, version, "", "", now());
    }

    /// Drop every identity-less `lib` row whose key is `workspace` or one of
    /// its ancestors. Such a row is keyed by the lockfile directory while its
    /// replacement is keyed by a nested workspace, so an upsert never matches
    /// it and it would outlive the checkout it protects.
    pub fn retire_legacy(&mut self, workspace: &str, lib: &str) {
        let workspace = Path::new(workspace);
        self.rows.retain(|row| {
            !(row.lib == lib && row.commit.is_empty() && workspace.starts_with(&row.project))
        });
    }

    fn record_at(
        &mut self,
        workspace: &str,
        lib: &str,
        dirname: &str,
        git_ref: &str,
        commit: &str,
        timestamp: u64,
    ) {
        match self
            .rows
            .iter_mut()
            .find(|r| r.project == workspace && r.lib == lib)
        {
            Some(r) => {
                r.version = dirname.to_string();
                r.git_ref = git_ref.to_string();
                r.commit = commit.to_string();
                r.resolved_at = timestamp;
                r.revision = r.revision.wrapping_add(1);
            }
            None => self.rows.push(RefRow {
                project: workspace.to_string(),
                lib: lib.to_string(),
                version: dirname.to_string(),
                git_ref: git_ref.to_string(),
                commit: commit.to_string(),
                resolved_at: timestamp,
                revision: 0,
            }),
        }
    }
}

/// The checkout directory a row points at.
pub fn row_dirname(row: &RefRow) -> String {
    row.version.clone()
}

pub struct RefStore {
    lock_path: PathBuf,
    data_path: PathBuf,
}

impl RefStore {
    pub fn at(cache_root: &Path) -> Self {
        Self {
            lock_path: cache_root.join("registry.lock"),
            data_path: cache_root.join("registry.json"),
        }
    }

    /// Callers recording a row must hold the library's lock while calling
    /// this. Whole-library deletion decides from a registry read taken under
    /// that same lock; a row written without it can land after that read and
    /// be missed, reopening the deletion race it is meant to close.
    pub fn commit<T>(&self, f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> {
        store::with_lock(&self.lock_path, &self.data_path, f)
    }

    pub fn snapshot(&self) -> Data {
        store::load(&self.data_path)
    }
}

#[derive(Debug)]
pub struct PrunePlan {
    /// Rows that survive with their materialized checkout identity unchanged.
    pub keep: Vec<RefRow>,
    /// (lib, worktree dirname) pairs with zero remaining references.
    pub delete: Vec<(String, String)>,
    /// Libs absent from every manifest with zero references — deleted only
    /// after confirmation.
    pub removable_libs: Vec<String>,
}

/// What a cache root holds: library directories, and directories that are not
/// libraries. A control entry belongs to the registry and is neither.
#[derive(Debug, Default)]
pub struct CacheScan {
    pub libs: Vec<String>,
    pub skipped: Vec<String>,
}

/// Classify every cache-root entry. A stray directory makes prune do less
/// rather than fail, so it is reported instead of rejected.
pub fn scan_cache(cache_root: &Path) -> Result<CacheScan> {
    let mut scan = CacheScan::default();
    let entries = std::fs::read_dir(cache_root)
        .with_context(|| format!("reading {}", cache_root.display()))?;
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let dirname = entry.file_name().to_string_lossy().into_owned();
        if locks::is_control(&dirname) {
            continue;
        }
        if cache::LibCache::from_dir(cache_root, &dirname).cloned() {
            scan.libs.push(dirname);
        } else {
            scan.skipped.push(dirname);
        }
    }
    scan.libs.sort();
    scan.skipped.sort();
    Ok(scan)
}

/// Pure prune planner. `worktrees` maps lib → worktree dirnames on disk
/// (including `default`); `current(project, lib)` reports whether a live
/// project still references the library (`None` = no longer referenced).
/// A surviving row keeps its recorded dirname because only resolve can make a
/// different checkout authoritative. Liveness checks run on a snapshot outside
/// the registry lock.
pub fn plan(
    data: &Data,
    worktrees: &BTreeMap<String, Vec<String>>,
    manifest_libs: &BTreeSet<String>,
    current: impl Fn(&str, &str) -> Option<String>,
) -> PrunePlan {
    let mut keep = Vec::new();
    for r in &data.rows {
        if !Path::new(&r.project).exists() {
            continue; // project root gone → holder dead → row drops
        }
        if current(&r.project, &r.lib).is_some() {
            keep.push(r.clone());
        }
    }
    let referenced: BTreeSet<(String, String)> = keep
        .iter()
        .map(|r| (r.lib.clone(), row_dirname(r)))
        .collect();
    let mut delete = Vec::new();
    let mut removable_libs = Vec::new();
    for (lib, dirs) in worktrees {
        for d in dirs {
            if !referenced.contains(&(lib.clone(), d.clone())) {
                delete.push((lib.clone(), d.clone()));
            }
        }
        let lib_referenced = referenced.iter().any(|(l, _)| l == lib);
        if !manifest_libs.contains(lib) && !lib_referenced {
            removable_libs.push(lib.clone());
        }
    }
    PrunePlan {
        keep,
        delete,
        removable_libs,
    }
}

/// What a live project selects for `entry`: an explicit/git entry remains
/// referenced, while a package entry requires a matching lockfile version.
/// Prune uses only presence here; the registry row owns the materialized dirname.
pub fn current_version(entry: &manifest::LibEntry, project: &Path) -> Option<String> {
    use manifest::Ecosystem;
    if entry.r#ref.is_some() {
        return Some("default".into());
    }
    let eco = entry.ecosystem?;
    if eco == Ecosystem::Git {
        return Some("default".into());
    }
    let (_, versions) = lockfiles::find_version(project, eco, &entry.package_name())?;
    lockfiles::highest(versions)
}

fn checkouts(cache_root: &Path, dirname: &str) -> Vec<String> {
    cache::LibCache::from_dir(cache_root, dirname)
        .version_worktrees()
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// The checkout a live project still selects for `lib`, or `None` once it
/// stopped referencing the library.
fn live_reference(data: &Data, project: &str, lib: &str, global: Option<&Path>) -> Option<String> {
    let proj = Path::new(project);
    match manifest::discover(proj, global) {
        Ok(disc) => {
            let entry = disc.manifest.libs.iter().find(|l| l.name == lib)?;
            current_version(entry, proj)
        }
        Err(e) => {
            // A live project whose manifest can't be read is not evidence it
            // stopped referencing the lib. Keep the existing reference (so
            // its checkout is never silently reclaimed) and warn.
            eprintln!("docm: cannot read {project}'s manifest ({e}); keeping its {lib} checkout");
            data.rows
                .iter()
                .find(|r| r.project == project && r.lib == lib)
                .map(row_dirname)
        }
    }
}

/// What one prune run reclaimed, and what it left for the caller to confirm.
#[derive(Debug, Default)]
pub struct Pruned {
    /// `lib/checkout` for every directory removed.
    pub removed: Vec<String>,
    /// Libs absent from every manifest with zero references — deleted only
    /// after confirmation.
    pub removable_libs: Vec<String>,
    /// Cache-root directories left alone for holding no `repo.git`.
    pub skipped: Vec<String>,
}

/// Reclaim unreferenced checkouts, holding each library's lock across planning
/// *and* removal. Planning outside the hold is what lets prune delete a
/// checkout a concurrent resolve materialized but has not yet recorded: the
/// registry it planned from goes stale the instant that resolve commits.
pub fn prune_with_lock(
    cache_root: &Path,
    manifest_libs: &BTreeSet<String>,
    global: Option<&Path>,
) -> Result<Pruned> {
    let scan = scan_cache(cache_root)?;
    let mut pruned = Pruned {
        skipped: scan.skipped,
        ..Default::default()
    };
    for dirname in &scan.libs {
        let (removed, removable) = locks::with_lib_dir(cache_root, dirname, || {
            prune_library_locked(cache_root, dirname, manifest_libs, global)
        })?;
        pruned.removed.extend(removed);
        pruned.removable_libs.extend(removable);
    }
    RefStore::at(cache_root).commit(|data| {
        data.rows.retain(|row| Path::new(&row.project).exists());
        Ok(())
    })?;
    Ok(pruned)
}

/// Prune one library, from a registry read inside its lock. Only a resolve of
/// the same library writes its rows, and that resolve needs the same lock, so
/// the plan cannot go stale between here and the removal.
fn prune_library_locked(
    cache_root: &Path,
    dirname: &str,
    manifest_libs: &BTreeSet<String>,
    global: Option<&Path>,
) -> Result<(Vec<String>, Option<String>)> {
    let name = crate::names::decode(dirname);
    let store = RefStore::at(cache_root);
    let fresh = store.snapshot();
    let scoped = Data {
        version: fresh.version,
        rows: fresh.rows.into_iter().filter(|r| r.lib == name).collect(),
    };
    let worktrees = BTreeMap::from([(name.clone(), checkouts(cache_root, dirname))]);
    let plan = plan(&scoped, &worktrees, manifest_libs, |project, lib| {
        live_reference(&scoped, project, lib, global)
    });

    let lib = cache::LibCache::from_dir(cache_root, dirname);
    let mut removed = Vec::new();
    for (_, checkout) in &plan.delete {
        lib.remove_worktree_locked(checkout)?;
        removed.push(format!("{name}/{checkout}"));
    }
    store.commit(|data| {
        reconcile(data, &scoped, &plan.keep);
        Ok(())
    })?;
    Ok((removed, plan.removable_libs.into_iter().next()))
}

/// Apply a prune plan to the freshly-locked registry without clobbering rows a
/// concurrent resolve added or retargeted after `snapshot` was taken. Drops
/// unchanged snapshot rows absent from `keep` and leaves fresher rows untouched.
pub fn reconcile(current: &mut Data, snapshot: &Data, keep: &[RefRow]) {
    use std::collections::{HashMap, HashSet};
    let key = |r: &RefRow| (r.project.clone(), r.lib.clone());
    let snapshot_rows: HashMap<(String, String), &RefRow> =
        snapshot.rows.iter().map(|row| (key(row), row)).collect();
    let keep_keys: HashSet<(String, String)> = keep.iter().map(key).collect();
    current
        .rows
        .retain(|row| match snapshot_rows.get(&key(row)) {
            None => true,
            Some(snapshot_row) => {
                keep_keys.contains(&key(row)) || row.revision != snapshot_row.revision
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    fn unique_tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("devkit-docs-rf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn record_upserts_by_project_and_lib() {
        let mut d = Data::default();
        d.record("/p1", "tokio", "1.0.0", "v1.0.0", "aaa");
        d.record("/p1", "tokio", "1.1.0", "v1.1.0", "bbb"); // same key → update
        d.record("/p2", "tokio", "1.0.0", "v1.0.0", "aaa"); // new workspace → append
        assert_eq!(d.rows.len(), 2);
        assert_eq!(d.rows[0].version, "1.1.0");
    }

    #[test]
    fn legacy_rows_default_revision_to_zero() {
        let data: Data = serde_json::from_str(
            r#"{
                "version": 1,
                "rows": [{
                    "project": "/p1",
                    "lib": "tokio",
                    "version": "1.0.0",
                    "resolved_at": 42
                }]
            }"#,
        )
        .unwrap();

        assert_eq!(data.rows[0].revision, 0);
    }

    #[test]
    fn repeated_same_key_and_version_record_advances_revision() {
        let mut data = Data::default();
        data.record_at("/p1", "tokio", "1.0.0", "v1.0.0", "aaa", 42);

        data.record_at("/p1", "tokio", "1.0.0", "v1.0.0", "aaa", 42);

        assert_eq!(data.rows[0].resolved_at, 42);
        assert_eq!(data.rows[0].revision, 1);
    }

    #[test]
    fn revision_wraparound_remains_distinguishable_from_the_prior_row() {
        let mut data = Data::default();
        data.record_at("/p1", "tokio", "1.0.0", "v1.0.0", "aaa", 42);
        data.rows[0].revision = u64::MAX;
        let prior = data.rows[0].clone();

        data.record_at("/p1", "tokio", "1.0.0", "v1.0.0", "aaa", 42);

        assert_eq!(data.rows[0].revision, 0);
        assert_ne!(data.rows[0], prior);
    }

    #[test]
    fn store_commit_and_snapshot_round_trip() {
        let root = unique_tmp("store");
        let store = RefStore::at(&root);
        store
            .commit(|d| {
                d.record("/p", "tokio", "1.0.0", "v1.0.0", "aaa");
                Ok(())
            })
            .unwrap();
        let snap = store.snapshot();
        assert_eq!(snap.rows.len(), 1);
        assert_eq!(snap.version, SCHEMA);
        assert!(root.join("registry.json").is_file());
    }

    #[test]
    fn plan_drops_dead_projects_and_preserves_materialized_rows() {
        let live = unique_tmp("live"); // an existing dir = live project
        let mut data = Data::default();
        data.record(live.to_str().unwrap(), "tokio", "1.0.0", "v1.0.0", "aaa");
        data.record("/gone/nowhere", "tokio", "0.9.0", "v0.9.0", "bbb"); // dead project → drop
        data.record(live.to_str().unwrap(), "serde", "2.0.0", "v2.0.0", "ccc"); // no longer in lockfile → drop

        let mut worktrees = BTreeMap::new();
        worktrees.insert(
            "tokio".to_string(),
            vec![
                "1.0.0".into(),
                "0.9.0".into(),
                "1.1.0".into(),
                "default".into(),
            ],
        );
        worktrees.insert("legacy".to_string(), vec!["3.0.0".into(), "default".into()]);
        let manifest_libs: BTreeSet<String> =
            ["tokio", "serde"].iter().map(|s| s.to_string()).collect();

        let p = plan(&data, &worktrees, &manifest_libs, |_, lib| {
            (lib == "tokio").then(|| "1.1.0".to_string())
        });
        assert_eq!(p.keep.len(), 1);
        assert_eq!(p.keep[0].version, "1.0.0");
        let mut del = p.delete.clone();
        del.sort();
        assert_eq!(
            del,
            vec![
                ("legacy".to_string(), "3.0.0".to_string()),
                ("legacy".to_string(), "default".to_string()),
                ("tokio".to_string(), "0.9.0".to_string()),
                ("tokio".to_string(), "1.1.0".to_string()),
                ("tokio".to_string(), "default".to_string()),
            ]
        ); // an unreferenced checkout goes, `default` included; 1.0.0 is the recorded checkout
        assert_eq!(p.removable_libs, vec!["legacy".to_string()]);
    }

    #[test]
    fn reconcile_preserves_a_concurrently_added_row() {
        let mut snapshot = Data::default();
        snapshot.record("/A", "libX", "1.0.0", "v1.0.0", "aaa");
        let keep = snapshot.rows.clone(); // plan kept the only snapshot row

        // Freshly-locked data: the snapshot row plus one a concurrent
        // resolve recorded after the snapshot was taken.
        let mut current = Data::default();
        current.record("/A", "libX", "1.0.0", "v1.0.0", "aaa");
        current.record("/C", "libZ", "3.0.0", "v3.0.0", "ccc");

        reconcile(&mut current, &snapshot, &keep);

        assert!(
            current
                .rows
                .iter()
                .any(|r| r.project == "/C" && r.lib == "libZ" && r.version == "3.0.0"),
            "concurrently-added row was dropped: {:?}",
            current.rows
        );
        assert!(
            current
                .rows
                .iter()
                .any(|r| r.project == "/A" && r.lib == "libX" && r.version == "1.0.0")
        );
        assert_eq!(current.rows.len(), 2);
    }

    #[test]
    fn reconcile_preserves_a_concurrent_same_key_retarget() {
        let mut snapshot = Data::default();
        snapshot.record("/A", "libX", "1.0.0", "v1.0.0", "aaa");
        let keep = snapshot.rows.clone();

        let mut current = Data::default();
        current.record("/A", "libX", "2.0.0", "v2.0.0", "bbb");

        reconcile(&mut current, &snapshot, &keep);

        assert_eq!(current.rows.len(), 1);
        assert_eq!(current.rows[0].version, "2.0.0");
    }

    #[test]
    fn reconcile_preserves_a_concurrent_same_row_refresh() {
        let mut snapshot = Data::default();
        snapshot.record_at("/A", "libX", "1.0.0", "v1.0.0", "aaa", 42);
        let mut current = Data {
            version: snapshot.version,
            rows: snapshot.rows.clone(),
        };
        current.record_at("/A", "libX", "1.0.0", "v1.0.0", "aaa", 42);

        reconcile(&mut current, &snapshot, &[]);

        assert_eq!(current.rows.len(), 1);
        assert_eq!(current.rows[0].resolved_at, 42);
        assert_eq!(current.rows[0].revision, 1);
    }

    #[test]
    fn reconcile_drops_a_dead_row_but_keeps_live() {
        let mut snapshot = Data::default();
        snapshot.record("/A", "libX", "1.0.0", "v1.0.0", "aaa");
        snapshot.record("/DEAD", "libW", "9.0.0", "v9.0.0", "ddd");
        // plan only kept the live row
        let keep = vec![snapshot.rows[0].clone()];

        let mut current = Data {
            version: snapshot.version,
            rows: snapshot.rows.clone(),
        };

        reconcile(&mut current, &snapshot, &keep);

        assert!(
            !current.rows.iter().any(|r| r.project == "/DEAD"),
            "dead row survived reconcile: {:?}",
            current.rows
        );
        assert!(
            current
                .rows
                .iter()
                .any(|r| r.project == "/A" && r.lib == "libX" && r.version == "1.0.0")
        );
        assert_eq!(current.rows.len(), 1);
    }
}
