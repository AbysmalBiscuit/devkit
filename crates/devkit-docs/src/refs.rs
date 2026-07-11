//! Reference registry: which project roots resolved which lib versions.
//! One flock-guarded JSON file at the cache root; concurrent agent sessions
//! race docm, so every read-modify-write goes through
//! `devkit_common::store::with_lock`. No devkitd gate — no daemon serves this
//! file. A holder is live iff its project root path still exists (the same
//! model as the ports registry).

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
        match self
            .rows
            .iter_mut()
            .find(|r| r.project == project && r.lib == lib)
        {
            Some(r) => {
                r.version = version.to_string();
                r.resolved_at = now();
            }
            None => self.rows.push(RefRow {
                project: project.to_string(),
                lib: lib.to_string(),
                version: version.to_string(),
                resolved_at: now(),
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
    /// Rows that survive (already retargeted to the current version).
    pub keep: Vec<RefRow>,
    /// (lib, worktree dirname) pairs with zero remaining references.
    pub delete: Vec<(String, String)>,
    /// Libs absent from every manifest with zero references — deleted only
    /// after confirmation.
    pub removable_libs: Vec<String>,
}

/// Pure prune planner. `worktrees` maps lib → worktree dirnames on disk
/// (including `default`); `current(project, lib)` re-resolves what a live
/// project pins right now (`None` = no longer referenced). Liveness checks
/// run on a snapshot outside the registry lock.
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
        if let Some(v) = current(&r.project, &r.lib) {
            keep.push(RefRow {
                version: v,
                ..r.clone()
            });
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
    fn plan_drops_dead_projects_retargets_bumps_and_deletes_orphans() {
        let live = unique_tmp("live"); // an existing dir = live project
        let mut data = Data::default();
        data.record(live.to_str().unwrap(), "tokio", "1.0.0"); // will retarget to 1.1.0
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
        assert_eq!(p.keep[0].version, "1.1.0"); // retargeted
        let mut del = p.delete.clone();
        del.sort();
        assert_eq!(
            del,
            vec![
                ("legacy".to_string(), "3.0.0".to_string()),
                ("tokio".to_string(), "0.9.0".to_string()),
                ("tokio".to_string(), "1.0.0".to_string()),
            ]
        ); // "default" never deleted; 1.1.0 referenced
        assert_eq!(p.removable_libs, vec!["legacy".to_string()]);
    }
}
