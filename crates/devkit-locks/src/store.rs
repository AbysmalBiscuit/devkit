use crate::model::{AcquireOutcome, Conflict, Data, LockEntry, SCHEMA_VERSION};
use anyhow::Result;
use devkit_common::paths;
use devkit_common::store::{self, Document, salvage_map};
use fd_lock::RwLock;
use std::fs::OpenOptions;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

impl Document for Data {
    fn stamp_version(&mut self) {
        self.version = SCHEMA_VERSION;
    }
    /// Recover whatever locks still deserialize from a registry whose top-level
    /// schema has drifted; `None` only if there's no `locks` object. Keys are
    /// preserved verbatim (they already carry the `root\0path` composite).
    fn salvage(raw: &str) -> Option<Self> {
        Some(Data {
            version: 0,
            locks: salvage_map::<String, LockEntry>(raw, "locks", |k| Some(k.to_string()))?,
        })
    }
    fn label() -> &'static str {
        "lock registry"
    }
    fn len(&self) -> usize {
        self.locks.len()
    }
}

/// A driver for the lock-registry read-modify-write cycle. `FlockStore` backs the
/// direct path; the daemon's `MemoryStore` (added later) backs in-memory state.
pub trait Store {
    /// Current registry state — a cheap read, no mutation.
    fn snapshot(&self) -> Result<Data>;
    /// Exclusive read-modify-write: run `f`, persist, return its value.
    fn commit<T>(&self, f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T>;
}

/// Error marker: a live `devkitd` holds the registry write gate (`devkitd.lock`).
/// Carried via `anyhow` so callers can distinguish it (e.g. a best-effort prune).
#[derive(Debug)]
pub struct DaemonHoldsLock;

impl std::fmt::Display for DaemonHoldsLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "a devkitd daemon holds the registry lock; refusing to modify locks.json \
             behind it — stop the daemon or use a daemon-enabled binary",
        )
    }
}
impl std::error::Error for DaemonHoldsLock {}

/// Direct file driver. Reads load the file ungated. Writes take a shared,
/// non-blocking lock on `devkitd.lock` — the gate — and refuse if a daemon holds
/// it exclusive, then run the data-flock RMW.
pub struct FlockStore {
    gate_path: PathBuf,
    lock_path: PathBuf,
    data_path: PathBuf,
}

impl FlockStore {
    pub fn new() -> Self {
        Self {
            gate_path: paths::devkitd_lock(),
            lock_path: paths::locks_lock(),
            data_path: paths::locks_file(),
        }
    }
    #[cfg(test)]
    fn at(dir: &Path) -> Self {
        Self {
            gate_path: dir.join("devkitd.lock"),
            lock_path: dir.join("locks.lock"),
            data_path: dir.join("locks.json"),
        }
    }
}

impl Default for FlockStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Store for FlockStore {
    fn snapshot(&self) -> Result<Data> {
        Ok(store::load(&self.data_path))
    }
    fn commit<T>(&self, f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> {
        // Every direct writer holds the shared gate for its entire RMW. The daemon
        // holds devkitd.lock exclusive for its whole life (via MemoryStore, never
        // FlockStore), so a concurrent try_read failure here means a live daemon
        // owns the registry — surface the typed refusal rather than writing behind it.
        if let Some(parent) = self.gate_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&self.gate_path)?;
        let gate = RwLock::new(file);
        // `anyhow::Error::new` (not `anyhow!`) so the type survives for `downcast_ref`.
        let _shared = gate
            .try_read()
            .map_err(|_| anyhow::Error::new(DaemonHoldsLock))?;
        store::with_lock(&self.lock_path, &self.data_path, f)
    }
}

/// Acquire (explicit mutation): prune dead, then all-or-nothing acquire.
#[allow(clippy::too_many_arguments)]
pub fn acquire_with(
    s: &impl Store,
    root: &str,
    holder: &str,
    paths: &[String],
    pid: Option<u32>,
    note: Option<&str>,
    ttl: u64,
    now: u64,
) -> Result<AcquireOutcome> {
    s.commit(|d| {
        d.prune_dead(now);
        Ok(d.try_acquire(root, paths, holder, pid, note, ttl, now))
    })
}

/// Check (ungated read): conflicts that would block `holder`, with a best-effort
/// prune of dead rows. A blocked prune (a daemon owns the gate) is swallowed —
/// a read must never hard-fail because cleanup couldn't persist.
pub fn check_with(
    s: &impl Store,
    root: &str,
    holder: &str,
    paths: &[String],
    now: u64,
) -> Result<Vec<Conflict>> {
    let data = s.snapshot()?;
    let conflicts = data.check(root, paths, holder, now);
    if !data.dead_keys(now).is_empty() {
        let _ = s.commit(|d| {
            d.prune_dead(now);
            Ok(())
        });
    }
    Ok(conflicts)
}

/// Release named paths (explicit mutation). Returns (released, refused).
pub fn release_with(
    s: &impl Store,
    root: &str,
    holder: &str,
    paths: &[String],
    force: bool,
) -> Result<(Vec<String>, Vec<String>)> {
    s.commit(|d| Ok(d.do_release(root, paths, holder, force)))
}

/// Release every lock held by `holder` in `root` (explicit mutation).
pub fn release_all_with(s: &impl Store, root: &str, holder: &str) -> Result<Vec<String>> {
    s.commit(|d| Ok(d.release_all(root, holder)))
}

/// Live locks (ungated read), best-effort prune. `all` ignores the root filter.
pub fn status_with(
    s: &impl Store,
    root: &str,
    all: bool,
    now: u64,
) -> Result<Vec<crate::model::LockEntry>> {
    let data = s.snapshot()?;
    if !data.dead_keys(now).is_empty() {
        let _ = s.commit(|d| {
            d.prune_dead(now);
            Ok(())
        });
    }
    let mut out: Vec<crate::model::LockEntry> = data
        .locks
        .values()
        .filter(|e| !crate::model::entry_dead(e, now) && (all || e.root == root))
        .cloned()
        .collect();
    out.sort_by(|a, b| (a.root.as_str(), a.path.as_str()).cmp(&(b.root.as_str(), b.path.as_str())));
    Ok(out)
}

/// Drop dead locks (explicit mutation); returns how many were removed.
pub fn prune_with(s: &impl Store, now: u64) -> Result<usize> {
    let data = s.snapshot()?;
    if data.dead_keys(now).is_empty() {
        return Ok(0);
    }
    s.commit(|d| Ok(d.prune_dead(now)))
}

/// Run `f` while holding the exclusive lock-registry file lock; persists the result.
/// Liveness probes here are cheap, non-blocking syscalls (`kill(0)`) and TTL
/// arithmetic, so — unlike the port registry's TCP probes — pruning runs inside the
/// lock without risk of holding it across a blocking call.
#[allow(dead_code)] // gated direct RMW entry point retained for parity with the port registry
pub fn with_lock<T>(f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> {
    FlockStore::new().commit(f)
}

/// The daemon's authoritative in-memory lock registry. Reads serve from memory;
/// a mutation writes the file through (atomic rename) and updates memory only if
/// that write succeeded — the file is the commit point, so memory and file never
/// diverge.
pub struct MemoryStore {
    state: Arc<Mutex<Data>>,
    data_path: PathBuf,
}

impl MemoryStore {
    pub fn new(state: Arc<Mutex<Data>>, data_path: PathBuf) -> Self {
        Self { state, data_path }
    }
}

impl Store for MemoryStore {
    fn snapshot(&self) -> Result<Data> {
        Ok(self
            .state
            .lock()
            .expect("lock registry mutex poisoned")
            .clone())
    }
    fn commit<T>(&self, f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> {
        let mut guard = self.state.lock().expect("lock registry mutex poisoned");
        let mut next = guard.clone();
        let out = f(&mut next)?;
        next.stamp_version();
        store::save(&self.data_path, &next)?; // commit point: persist before memory
        *guard = next;
        Ok(out)
    }
}

/// Load the lock-registry file into a `Data` for an owner with its own exclusion
/// (the daemon, holding `devkitd.lock` exclusive, at startup).
pub fn load() -> Data {
    store::load(&paths::locks_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Data, LockEntry, key_for};

    #[test]
    fn serde_roundtrips_a_lock() {
        let mut d = Data::default();
        d.locks.insert(
            key_for("/repo", "scenes"),
            LockEntry {
                path: "scenes".into(),
                root: "/repo".into(),
                holder: "alice".into(),
                pid: None,
                note: None,
                ts: 7,
                ttl: 1800,
            },
        );
        let s = serde_json::to_string(&d).unwrap();
        let back: Data = serde_json::from_str(&s).unwrap();
        assert_eq!(back.locks[&key_for("/repo", "scenes")].holder, "alice");
    }

    #[test]
    fn salvage_recovers_locks_from_drifted_schema() {
        // "version" is a string rather than u32 to force a top-level Data
        // deserialization failure. The key uses the JSON \u0000 escape so that
        // after parsing the key contains a NUL byte, matching key_for's separator.
        let json = "{\"version\":\"oops\",\"locks\":{\"/repo\\u0000scenes\":{\"path\":\"scenes\",\"root\":\"/repo\",\"holder\":\"alice\",\"pid\":null,\"note\":null,\"ts\":7,\"ttl\":1800}}}";
        assert!(serde_json::from_str::<Data>(json).is_err());
        let d = Data::salvage(json).expect("locks object present");
        assert_eq!(d.locks[&key_for("/repo", "scenes")].holder, "alice");
        assert_eq!(d.version, 0);
    }

    #[test]
    fn salvage_gives_up_without_locks_object() {
        assert!(Data::salvage(r#"{"something":"else"}"#).is_none());
    }
}

#[cfg(test)]
mod seam_tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("devkit-lockseam-{}-{}", std::process::id(), tag));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn acquire_with_then_check_sees_conflict_for_other_holder() {
        let dir = tmp("acq");
        let s = FlockStore::at(&dir);
        let out = acquire_with(
            &s,
            "/repo",
            "alice",
            &["scenes".into()],
            None,
            None,
            1800,
            100,
        )
        .unwrap();
        assert_eq!(out.acquired.len(), 1);
        let conflicts = check_with(&s, "/repo", "bob", &["scenes/x".into()], 120).unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].held_by, "alice");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn release_with_frees_holders_path() {
        let dir = tmp("rel");
        let s = FlockStore::at(&dir);
        acquire_with(
            &s,
            "/repo",
            "alice",
            &["scenes".into()],
            None,
            None,
            1800,
            100,
        )
        .unwrap();
        let (released, refused) =
            release_with(&s, "/repo", "alice", &["scenes".into()], false).unwrap();
        assert_eq!(released, vec!["scenes".to_string()]);
        assert!(refused.is_empty());
        assert!(s.snapshot().unwrap().locks.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_with_drops_dead_and_is_a_hard_mutation() {
        let dir = tmp("prune");
        let s = FlockStore::at(&dir);
        // ttl=60, ts=0 → dead at now=1000
        acquire_with(&s, "/repo", "alice", &["scenes".into()], None, None, 60, 0).unwrap();
        let dropped = prune_with(&s, 1000).unwrap();
        assert_eq!(dropped, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_refused_while_gate_held_exclusive() {
        let dir = tmp("gate");
        let s = FlockStore::at(&dir);
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(dir.join("devkitd.lock"))
            .unwrap();
        let mut excl = fd_lock::RwLock::new(f);
        let _held = excl.try_write().expect("take exclusive gate");
        let err = acquire_with(
            &s,
            "/repo",
            "alice",
            &["scenes".into()],
            None,
            None,
            1800,
            100,
        )
        .unwrap_err();
        assert!(
            err.downcast_ref::<DaemonHoldsLock>().is_some(),
            "got: {err:#}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_is_ungated_under_held_gate() {
        let dir = tmp("checkgate");
        let s = FlockStore::at(&dir);
        acquire_with(
            &s,
            "/repo",
            "alice",
            &["scenes".into()],
            None,
            None,
            1800,
            100,
        )
        .unwrap();
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(dir.join("devkitd.lock"))
            .unwrap();
        let mut excl = fd_lock::RwLock::new(f);
        let _held = excl.try_write().unwrap();
        // ungated read must still succeed (and best-effort prune must not error out)
        let conflicts = check_with(&s, "/repo", "bob", &["scenes".into()], 120)
            .expect("read must not fail under held gate");
        assert_eq!(conflicts.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
