//! Reference registry: which project roots resolved which lib versions.
//! One flock-guarded JSON file at the cache root; concurrent agent sessions
//! race docm, so every read-modify-write goes through
//! `devkit_common::store::with_lock`. No devkitd gate — no daemon serves this
//! file. A holder is live iff its project root path still exists (the same
//! model as the ports registry).

use crate::cache;
use crate::lockfiles;
use crate::manifest;
use anyhow::Result;
use devkit_common::store::{self, Document};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefRow {
    pub project: String,
    pub lib: String,
    /// Worktree dirname: a lockfile version like `1.38.0`, or `default`.
    pub version: String,
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
    /// Upsert keyed on (project, lib).
    pub fn record(&mut self, project: &str, lib: &str, version: &str) {
        self.record_at(project, lib, version, now());
    }

    fn record_at(&mut self, project: &str, lib: &str, version: &str, timestamp: u64) {
        match self
            .rows
            .iter_mut()
            .find(|r| r.project == project && r.lib == lib)
        {
            Some(r) => {
                r.version = version.to_string();
                r.resolved_at = timestamp;
                r.revision = r.revision.wrapping_add(1);
            }
            None => self.rows.push(RefRow {
                project: project.to_string(),
                lib: lib.to_string(),
                version: version.to_string(),
                resolved_at: timestamp,
                revision: 0,
            }),
        }
    }
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
        .map(|r| (r.lib.clone(), r.version.clone()))
        .collect();
    let mut delete = Vec::new();
    let mut removable_libs = Vec::new();
    for (lib, dirs) in worktrees {
        for d in dirs {
            if d != "default" && !referenced.contains(&(lib.clone(), d.clone())) {
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

/// Build a prune plan for a whole cache root, re-discovering each referenced
/// project's own manifest so a lib registered only in another project's
/// `[docs]` overlay is never treated as unreferenced. `manifest_libs` is the
/// invoking CWD's lib set (used only for whole-lib removability). `global` is
/// the global-manifest path override (None = the default `~/.config/devkit/docs.toml`).
pub fn plan_for_cache(
    cache_root: &Path,
    snapshot: &Data,
    manifest_libs: &BTreeSet<String>,
    global: Option<&Path>,
) -> anyhow::Result<PrunePlan> {
    let mut worktrees: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in std::fs::read_dir(cache_root)?.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let dirname = e.file_name().to_string_lossy().into_owned();
        let name = crate::names::decode(&dirname);
        let lib = cache::LibCache::from_dir(cache_root, &dirname);
        if !lib.cloned() {
            continue;
        }
        let dirs = lib
            .version_worktrees()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        worktrees.insert(name, dirs);
    }
    Ok(plan(snapshot, &worktrees, manifest_libs, |project, lib| {
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
                eprintln!(
                    "docm: cannot read {project}'s manifest ({e}); keeping its {lib} checkout"
                );
                snapshot
                    .rows
                    .iter()
                    .find(|r| r.project == project && r.lib == lib)
                    .map(|r| r.version.clone())
            }
        }
    }))
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
        d.record("/p1", "tokio", "1.0.0");
        d.record("/p1", "tokio", "1.1.0"); // same key → update
        d.record("/p2", "tokio", "1.0.0"); // new project → append
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
        data.record_at("/p1", "tokio", "1.0.0", 42);

        data.record_at("/p1", "tokio", "1.0.0", 42);

        assert_eq!(data.rows[0].resolved_at, 42);
        assert_eq!(data.rows[0].revision, 1);
    }

    #[test]
    fn revision_wraparound_remains_distinguishable_from_the_prior_row() {
        let mut data = Data::default();
        data.record_at("/p1", "tokio", "1.0.0", 42);
        data.rows[0].revision = u64::MAX;
        let prior = data.rows[0].clone();

        data.record_at("/p1", "tokio", "1.0.0", 42);

        assert_eq!(data.rows[0].revision, 0);
        assert_ne!(data.rows[0], prior);
    }

    #[test]
    fn store_commit_and_snapshot_round_trip() {
        let root = unique_tmp("store");
        let store = RefStore::at(&root);
        store
            .commit(|d| {
                d.record("/p", "tokio", "1.0.0");
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
        data.record(live.to_str().unwrap(), "tokio", "1.0.0");
        data.record("/gone/nowhere", "tokio", "0.9.0"); // dead project → drop
        data.record(live.to_str().unwrap(), "serde", "2.0.0"); // no longer in lockfile → drop

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
                ("tokio".to_string(), "0.9.0".to_string()),
                ("tokio".to_string(), "1.1.0".to_string()),
            ]
        ); // "default" never deleted; 1.0.0 is the recorded checkout
        assert_eq!(p.removable_libs, vec!["legacy".to_string()]);
    }

    #[test]
    fn reconcile_preserves_a_concurrently_added_row() {
        let mut snapshot = Data::default();
        snapshot.record("/A", "libX", "1.0.0");
        let keep = snapshot.rows.clone(); // plan kept the only snapshot row

        // Freshly-locked data: the snapshot row plus one a concurrent
        // resolve recorded after the snapshot was taken.
        let mut current = Data::default();
        current.record("/A", "libX", "1.0.0");
        current.record("/C", "libZ", "3.0.0");

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
        snapshot.record("/A", "libX", "1.0.0");
        let keep = snapshot.rows.clone();

        let mut current = Data::default();
        current.record("/A", "libX", "2.0.0");

        reconcile(&mut current, &snapshot, &keep);

        assert_eq!(current.rows.len(), 1);
        assert_eq!(current.rows[0].version, "2.0.0");
    }

    #[test]
    fn reconcile_preserves_a_concurrent_same_row_refresh() {
        let mut snapshot = Data::default();
        snapshot.record_at("/A", "libX", "1.0.0", 42);
        let mut current = Data {
            version: snapshot.version,
            rows: snapshot.rows.clone(),
        };
        current.record_at("/A", "libX", "1.0.0", 42);

        reconcile(&mut current, &snapshot, &[]);

        assert_eq!(current.rows.len(), 1);
        assert_eq!(current.rows[0].resolved_at, 42);
        assert_eq!(current.rows[0].revision, 1);
    }

    #[test]
    fn reconcile_drops_a_dead_row_but_keeps_live() {
        let mut snapshot = Data::default();
        snapshot.record("/A", "libX", "1.0.0");
        snapshot.record("/DEAD", "libW", "9.0.0");
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
