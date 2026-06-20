# Authoritative In-Memory devkit-portd (Port Registry) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `devkit-portd` hold the port registry in memory and serve reads from memory without taking the flock or re-reading the file, writing through to the file on mutations, with the sole-writer boundary enforced by `portd.lock`.

**Architecture:** Introduce a `Store` seam in `devkit-ports::registry` with two drivers — `FlockStore` (direct path: a shared `portd.lock` gate + the data flock) and `MemoryStore` (the daemon's `Arc<Mutex<Data>>` with file write-through). Each registry operation (`alloc/record_pid/release/snapshot/prune`) is written once as a generic `*_with(store)` so both drivers run identical invariant logic. The daemon loads the file into memory at startup and drives every handler through `MemoryStore`; direct callers use `FlockStore`, which hard-errors if a daemon holds `portd.lock`.

**Tech Stack:** Rust 2024, `anyhow`, `fd-lock` (advisory file locks), `serde`/`serde_json`, `interprocess` (daemon socket — unchanged here).

**Spec:** `docs/superpowers/specs/2026-06-21-authoritative-in-memory-portd-design.md`

## Global Constraints

- **Scope is the port registry only** (`devkit-ports` + `devkit-portd`). The lock registry is a separate follow-up; do not touch `devkit-locks`.
- **Reserve before bind:** `alloc` commits a pid-less reservation row before the caller binds. Never reorder.
- **`record_pid` re-inserts a pruned row** so a live process is never left untracked.
- **`RESERVATION_GRACE_SECS` (300) must exceed the 120s readiness timeout.** Do not change it.
- **`down` stops then releases without pruning first.** Keep that order.
- **Keep work inside any held lock minimal** — probe `listening()` outside the lock/mutex.
- **Public facade signatures (`registry::alloc/record_pid/release/snapshot/prune/with_lock`) stay unchanged** — `portman`, `devrun`, and `tests/registry.rs` call them and must keep compiling.
- **Gate refusal message (verbatim):** `a devkit-portd daemon holds the registry lock; refusing to modify ports.json behind it — stop the daemon or use a daemon-enabled binary`
- **Merge gate:** `cargo test --workspace` green and `cargo clippy --workspace --all-targets -- -D warnings` clean after every task.
- **Conventional commits.** End each commit message body with:
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`

---

### Task 1: Expose `load`/`save` on the shared store

The daemon must load the registry file into memory once and write through on each mutation, without `with_lock`'s flock. Expose the store's existing private read/write as `load`/`save`.

**Files:**
- Modify: `crates/devkit-common/src/store.rs`

**Interfaces:**
- Consumes: existing private `fn read<D: Document>(path) -> D`, `fn write<D: Document>(path, &D) -> Result<()>` in the same file.
- Produces: `pub fn load<D: Document>(path: &Path) -> D`; `pub fn save<D: Document>(path: &Path, data: &D) -> Result<()>`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `crates/devkit-common/src/store.rs` (the `Doc` test type already exists there):

```rust
    #[test]
    fn load_save_roundtrip_and_missing_default() {
        let p = scratch("loadsave.json");
        let _ = fs::remove_file(&p);
        assert!(load::<Doc>(&p).is_empty(), "missing file loads as default");
        let mut d = Doc::default();
        d.items.insert(8080, "api".into());
        save(&p, &d).unwrap();
        assert_eq!(load::<Doc>(&p).items[&8080], "api");
        let _ = fs::remove_file(&p);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-common store::tests::load_save_roundtrip_and_missing_default`
Expected: FAIL to compile — `load`/`save` not found.

- [ ] **Step 3: Add the public wrappers**

In `crates/devkit-common/src/store.rs`, immediately after the `salvage_map` function, add:

```rust
/// Load a document, salvaging on schema drift exactly as `with_lock` does on read.
/// A missing or empty file yields the default. Never takes a lock — intended for a
/// one-shot read by an owner that has its own exclusion (e.g. the daemon at startup).
pub fn load<D: Document>(path: &Path) -> D {
    read(path)
}

/// Persist a document with a crash-safe atomic rename. Takes no lock and does not
/// stamp the version — a caller that mutated the document should call
/// `Document::stamp_version` first (as `with_lock` does).
pub fn save<D: Document>(path: &Path, data: &D) -> Result<()> {
    write(path, data)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p devkit-common store::`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-common/src/store.rs
git commit -m "feat(store): expose load/save for lock-free owners

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: `Store` seam, `FlockStore`, the `portd.lock` gate, and the generic operations

Introduce the driver seam and rewrite the five operations once as generics. `FlockStore` is the direct path: reads load the file (ungated); writes acquire a shared `portd.lock` gate and run under the data flock. The public `registry::with_lock` becomes `FlockStore::new().commit(f)`, so `devrun` and the race test inherit the gate. The facade's daemon branch is split so a live-daemon error propagates instead of silently flock-writing.

**Files:**
- Modify: `crates/devkit-ports/src/registry.rs` (imports near 1-8; `via_daemon` 248-265; all of `snapshot/snapshot_flock/prune/prune_flock/alloc/alloc_flock/record_pid/record_pid_flock/release/release_flock` 267-469; the `with_lock` wrapper at 81-84)

**Interfaces:**
- Consumes: `Data`, `Entry`, `Role`, `now()`, `listening()`, `Data::{holds, alloc_one, record_pid, release, dead_ports}`, `devkit_common::store::{with_lock, load}`, `devkit_common::paths::{daemon_lock_file, lock_file, registry_file}`.
- Produces:
  - `pub trait Store { fn snapshot(&self) -> Result<Data>; fn commit<T>(&self, f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T>; }`
  - `pub struct FlockStore` with `pub fn new() -> Self`, `impl Default`, `impl Store`.
  - `pub struct DaemonHoldsLock` (error marker).
  - `fn alloc_with(&impl Store, holder, reqs, role) -> Result<Vec<(String,u16)>>`, `fn record_pid_with(&impl Store, port, app, holder, role, pid, logfile) -> Result<()>`, `fn release_with(&impl Store, holder, role) -> Result<Vec<u16>>`, `fn snapshot_with(&impl Store) -> Result<Data>`, `fn prune_with(&impl Store) -> Result<Vec<u16>>`.
  - `pub fn with_lock<T>(f) -> Result<T>` (now gated, signature unchanged).
  - unchanged public `alloc/record_pid/release/snapshot/prune` signatures.

- [ ] **Step 1: Write the failing tests**

Add a new test module at the end of `crates/devkit-ports/src/registry.rs`:

```rust
#[cfg(test)]
mod store_seam_tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("devkit-seam-{}-{}", std::process::id(), tag));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn alloc_with_reserves_pidless_then_record_pid_attaches() {
        let dir = tmp("alloc");
        let store = FlockStore::at(&dir);
        let out = alloc_with(&store, "/w", &[("api".into(), 9100)], Role::Issue).unwrap();
        let (_, port) = out[0];
        let d = store.snapshot().unwrap();
        assert_eq!(d.entries[&port].pid, None, "reserve before bind: pid-less row");
        record_pid_with(&store, port, "api", "/w", Role::Issue, 4321, PathBuf::from("/log")).unwrap();
        assert_eq!(store.snapshot().unwrap().entries[&port].pid, Some(4321));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn release_with_frees_holder() {
        let dir = tmp("release");
        let store = FlockStore::at(&dir);
        alloc_with(&store, "/w", &[("api".into(), 9100)], Role::Issue).unwrap();
        let freed = release_with(&store, "/w", None).unwrap();
        assert_eq!(freed.len(), 1);
        assert!(store.snapshot().unwrap().entries.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_refused_while_gate_held_exclusive() {
        let dir = tmp("gate-held");
        let store = FlockStore::at(&dir);
        // Simulate a running daemon: hold portd.lock exclusive on a separate fd.
        let f = std::fs::OpenOptions::new()
            .create(true).write(true).truncate(false)
            .open(dir.join("portd.lock")).unwrap();
        let mut excl = fd_lock::RwLock::new(f);
        let _held = excl.try_write().expect("take exclusive gate");
        let err = store
            .commit(|d| { d.alloc_one("/w", "api", 9100, Role::Issue); Ok(()) })
            .unwrap_err();
        assert!(err.downcast_ref::<DaemonHoldsLock>().is_some(), "got: {err:#}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_is_ungated_and_prune_is_best_effort_under_held_gate() {
        let dir = tmp("snap-gate");
        let store = FlockStore::at(&dir);
        // Seed a dead reservation (dead holder dir => dead_ports flags it).
        store
            .commit(|d| {
                d.entries.insert(9100, Entry {
                    app: "api".into(), holder: "/definitely/not/here".into(),
                    role: Role::Issue, pid: None, logfile: None, ts: 0,
                });
                Ok(())
            })
            .unwrap();
        // Now hold the gate exclusive: snapshot must still succeed (reads ungated)
        // and must not propagate the blocked prune.
        let f = std::fs::OpenOptions::new()
            .create(true).write(true).truncate(false)
            .open(dir.join("portd.lock")).unwrap();
        let mut excl = fd_lock::RwLock::new(f);
        let _held = excl.try_write().unwrap();
        let snap = snapshot_with(&store).expect("read must not fail under held gate");
        assert!(!snap.entries.contains_key(&9100), "dead entry pruned from the returned view");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-ports store_seam_tests`
Expected: FAIL to compile — `FlockStore`, `alloc_with`, `record_pid_with`, `release_with`, `snapshot_with`, `DaemonHoldsLock` not found.

- [ ] **Step 3: Restore the imports the gate needs**

At the top of `crates/devkit-ports/src/registry.rs`, change the import block to add `anyhow::anyhow`, the gate's file/lock types, and `Path`:

```rust
use anyhow::{Result, anyhow};
use devkit_common::paths;
use devkit_common::store::{self, Document, salvage_map};
use fd_lock::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
```

- [ ] **Step 4: Replace the `with_lock` wrapper with the `Store` seam, `FlockStore`, and the gate**

Replace the current wrapper (lines ~81-84):

```rust
pub fn with_lock<T>(f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> {
    store::with_lock(&paths::lock_file(), &paths::registry_file(), f)
}
```

with:

```rust
/// A driver for the registry read-modify-write cycle. `FlockStore` backs the
/// direct path; the daemon's `MemoryStore` (added later) backs in-memory state.
pub trait Store {
    /// Current registry state — a cheap read, no mutation.
    fn snapshot(&self) -> Result<Data>;
    /// Exclusive read-modify-write: run `f`, persist, return its value.
    fn commit<T>(&self, f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T>;
}

/// Error marker: a live `devkit-portd` holds the registry write gate (`portd.lock`).
/// Carried via `anyhow` so callers can distinguish it (e.g. a best-effort prune).
#[derive(Debug)]
pub struct DaemonHoldsLock;

impl std::fmt::Display for DaemonHoldsLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "a devkit-portd daemon holds the registry lock; refusing to modify ports.json \
             behind it — stop the daemon or use a daemon-enabled binary",
        )
    }
}
impl std::error::Error for DaemonHoldsLock {}

/// Direct file driver. Reads load the file ungated (the daemon keeps it current via
/// write-through). Writes first take a shared, non-blocking lock on `portd.lock` — the
/// gate — and refuse if a daemon holds it exclusive, then run the data-flock RMW.
pub struct FlockStore {
    gate_path: PathBuf,
    lock_path: PathBuf,
    data_path: PathBuf,
}

impl FlockStore {
    /// Real-paths store used by every direct caller.
    pub fn new() -> Self {
        Self {
            gate_path: paths::daemon_lock_file(),
            lock_path: paths::lock_file(),
            data_path: paths::registry_file(),
        }
    }
    /// Scratch-paths store for tests.
    fn at(dir: &Path) -> Self {
        Self {
            gate_path: dir.join("portd.lock"),
            lock_path: dir.join("ports.lock"),
            data_path: dir.join("ports.json"),
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
        // Hold the shared gate for the whole RMW; a held exclusive (a live daemon)
        // makes try_read fail, which we surface as the typed refusal.
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&self.gate_path)?;
        let mut gate = RwLock::new(file);
        // `anyhow::Error::new` (not `anyhow!`) so the type survives for `downcast_ref`.
        let _shared = gate
            .try_read()
            .map_err(|_| anyhow::Error::new(DaemonHoldsLock))?;
        store::with_lock(&self.lock_path, &self.data_path, f)
    }
}

/// Run `f` against the registry under the direct flock path. Public because
/// `devrun down` and the multiprocess race test drive a custom RMW through it;
/// now gated, so it refuses to write behind a live daemon.
pub fn with_lock<T>(f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> {
    FlockStore::new().commit(f)
}
```

- [ ] **Step 5: Replace `via_daemon` with the `daemon_request` split**

Replace `via_daemon` (lines ~248-265) with:

```rust
/// Try a running daemon. `Ok(None)` = no daemon (caller uses the flock path).
/// `Ok(Some(resp))` = the daemon answered (the response may be `Response::Err`,
/// which callers decode into an `Err`). `Err` = a *live* daemon failed mid-request
/// — surfaced to the caller rather than silently written behind its back.
/// Returns `Ok(None)` inside the daemon itself (`DEVKIT_PORTD_SELF`).
#[cfg(feature = "daemon")]
fn daemon_request(
    req: crate::daemon::proto::Request,
) -> Result<Option<crate::daemon::proto::Response>> {
    if std::env::var_os("DEVKIT_PORTD_SELF").is_some() {
        return Ok(None);
    }
    let Some(mut c) = crate::daemon::client::try_existing() else {
        return Ok(None);
    };
    Ok(Some(c.request(&req)?))
}
```

- [ ] **Step 6: Replace the five operations with generic `*_with` cores + thin facades**

Replace everything from `pub fn snapshot()` (line ~269) through the end of `release_flock` (line ~469) with the following. This deletes the `*_flock` functions and the duplicated bodies, keeping each invariant once.

```rust
/// Read the registry, pruning dead entries. Probes liveness *outside* the lock.
fn snapshot_with(store: &impl Store) -> Result<Data> {
    let data = store.snapshot()?;
    let dead = data.dead_ports();
    if dead.is_empty() {
        return Ok(data);
    }
    // Best-effort prune: a read must never fail because cleanup was blocked (a
    // daemon now owns the write gate). Persist the removals if we can; otherwise
    // return the dead-pruned view without persisting.
    match store.commit(|d| {
        for p in &dead {
            d.entries.remove(p);
        }
        Ok(d.clone())
    }) {
        Ok(pruned) => Ok(pruned),
        Err(_) => {
            let mut d = data;
            for p in &dead {
                d.entries.remove(&p);
            }
            Ok(d)
        }
    }
}

/// Prune dead entries; returns the ports removed. Probes outside the lock.
fn prune_with(store: &impl Store) -> Result<Vec<u16>> {
    let data = store.snapshot()?;
    let dead = data.dead_ports();
    if dead.is_empty() {
        return Ok(Vec::new());
    }
    store.commit(|d| {
        for p in &dead {
            d.entries.remove(p);
        }
        Ok(())
    })?;
    Ok(dead)
}

/// Reserve a port for each `(app, base_port)` under `holder`+`role`. Probes
/// `listening()` outside the lock; the commit re-checks under exclusion.
fn alloc_with(
    store: &impl Store,
    holder: &str,
    reqs: &[(String, u16)],
    role: Role,
) -> Result<Vec<(String, u16)>> {
    let mut data = store.snapshot()?;
    let mut chosen: Vec<(String, u16)> = Vec::with_capacity(reqs.len());
    for (app, base) in reqs {
        if let Some(p) = data.holds(holder, app, role) {
            chosen.push((app.clone(), p));
            continue;
        }
        let mut port = *base;
        loop {
            let taken =
                data.entries.contains_key(&port) || chosen.iter().any(|(_, p)| *p == port);
            if !taken && !listening(port) {
                break;
            }
            port = port
                .checked_add(1)
                .unwrap_or_else(|| panic!("no free port available at or above {base}"));
        }
        data.entries.insert(
            port,
            Entry {
                app: app.clone(),
                holder: holder.into(),
                role,
                pid: None,
                logfile: None,
                ts: now(),
            },
        );
        chosen.push((app.clone(), port));
    }

    store.commit(|d| {
        let mut out = Vec::with_capacity(chosen.len());
        for (app, port) in &chosen {
            if let Some(p) = d.holds(holder, app, role) {
                out.push((app.clone(), p));
            } else if d.entries.contains_key(port) {
                let base = reqs
                    .iter()
                    .find(|(a, _)| a == app)
                    .map(|(_, b)| *b)
                    .unwrap_or(*port);
                let p = d.alloc_one(holder, app, base, role);
                out.push((app.clone(), p));
            } else {
                d.entries.insert(
                    *port,
                    Entry {
                        app: app.clone(),
                        holder: holder.into(),
                        role,
                        pid: None,
                        logfile: None,
                        ts: now(),
                    },
                );
                out.push((app.clone(), *port));
            }
        }
        Ok(out)
    })
}

fn record_pid_with(
    store: &impl Store,
    port: u16,
    app: &str,
    holder: &str,
    role: Role,
    pid: u32,
    logfile: PathBuf,
) -> Result<()> {
    store.commit(|d| {
        d.record_pid(port, app, holder, role, pid, logfile);
        Ok(())
    })
}

fn release_with(store: &impl Store, holder: &str, role: Option<Role>) -> Result<Vec<u16>> {
    store.commit(|d| Ok(d.release(holder, role)))
}

/// Read the registry, pruning dead entries (daemon fast path, else flock).
pub fn snapshot() -> Result<Data> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(crate::daemon::proto::Request::Snapshot)? {
        return match resp {
            crate::daemon::proto::Response::Snapshot(d) => Ok(d),
            crate::daemon::proto::Response::Err(e) => Err(anyhow!(e)),
            other => Err(anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    snapshot_with(&FlockStore::new())
}

/// Prune dead entries; returns the ports removed.
pub fn prune() -> Result<Vec<u16>> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(crate::daemon::proto::Request::Prune)? {
        return match resp {
            crate::daemon::proto::Response::Freed(v) => Ok(v),
            crate::daemon::proto::Response::Err(e) => Err(anyhow!(e)),
            other => Err(anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    prune_with(&FlockStore::new())
}

/// Reserve a port for each `(app, base_port)` request under `holder`+`role`.
pub fn alloc(holder: &str, reqs: &[(String, u16)], role: Role) -> Result<Vec<(String, u16)>> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(crate::daemon::proto::Request::Alloc {
        holder: holder.to_string(),
        reqs: reqs.to_vec(),
        role,
    })? {
        return match resp {
            crate::daemon::proto::Response::Ports(v) => Ok(v),
            crate::daemon::proto::Response::Err(e) => Err(anyhow!(e)),
            other => Err(anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    alloc_with(&FlockStore::new(), holder, reqs, role)
}

/// Attach a pid + logfile to a reservation (re-establishing it if pruned).
pub fn record_pid(
    port: u16,
    app: &str,
    holder: &str,
    role: Role,
    pid: u32,
    logfile: PathBuf,
) -> Result<()> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(crate::daemon::proto::Request::RecordPid {
        port,
        app: app.to_string(),
        holder: holder.to_string(),
        role,
        pid,
        logfile: logfile.clone(),
    })? {
        return match resp {
            crate::daemon::proto::Response::Ok => Ok(()),
            crate::daemon::proto::Response::Err(e) => Err(anyhow!(e)),
            other => Err(anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    record_pid_with(&FlockStore::new(), port, app, holder, role, pid, logfile)
}

/// Release all entries for `holder` (optionally one role); returns freed ports.
pub fn release(holder: &str, role: Option<Role>) -> Result<Vec<u16>> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(crate::daemon::proto::Request::Release {
        holder: holder.to_string(),
        role,
    })? {
        return match resp {
            crate::daemon::proto::Response::Freed(v) => Ok(v),
            crate::daemon::proto::Response::Err(e) => Err(anyhow!(e)),
            other => Err(anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    release_with(&FlockStore::new(), holder, role)
}
```

- [ ] **Step 7: Run the new tests, then the whole gate**

Run: `cargo test -p devkit-ports store_seam_tests`
Expected: PASS (4 tests).

Run: `cargo test --workspace`
Expected: PASS — every existing test still green (no daemon runs in tests, so the gate is always free and behavior is identical).

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean. (If `anyhow::anyhow` or an import is unused, remove it.)

- [ ] **Step 8: Commit**

```bash
git add crates/devkit-ports/src/registry.rs
git commit -m "refactor(registry): Store seam, FlockStore, and portd.lock write gate

Each registry operation becomes a generic *_with(store); FlockStore drives the
direct path. Writes now take a shared portd.lock gate and hard-error if a daemon
holds it exclusive (DaemonHoldsLock); reads load the file ungated. The facade's
daemon branch propagates a live-daemon error instead of silently flock-writing.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: `MemoryStore` driver and `registry::load`

The daemon's authoritative state. `commit` writes the file through (the commit point) and updates memory only on success, so memory and file never diverge.

**Files:**
- Modify: `crates/devkit-ports/src/registry.rs` (add after `FlockStore`)

**Interfaces:**
- Consumes: `Store`, `Data`, `devkit_common::store::{load, save}`, `Document::stamp_version`, `std::sync::{Arc, Mutex}`.
- Produces: `pub struct MemoryStore` with `pub fn new(state: Arc<Mutex<Data>>, data_path: PathBuf) -> Self` and `impl Store`; `pub fn load() -> Data`.

- [ ] **Step 1: Write the failing tests**

Add to the `store_seam_tests` module in `crates/devkit-ports/src/registry.rs`:

```rust
    #[test]
    fn memorystore_commit_writes_through_and_updates_memory() {
        let dir = tmp("mem-ok");
        let state = std::sync::Arc::new(std::sync::Mutex::new(Data::default()));
        let store = MemoryStore::new(state.clone(), dir.join("ports.json"));
        alloc_with(&store, "/w", &[("api".into(), 9100)], Role::Issue).unwrap();
        // memory updated
        assert_eq!(state.lock().unwrap().entries.len(), 1);
        // file written through (load sees it)
        let on_disk: Data = devkit_common::store::load(&dir.join("ports.json"));
        assert_eq!(on_disk.entries.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn memorystore_commit_failure_leaves_memory_unchanged() {
        let dir = tmp("mem-fail");
        let state = std::sync::Arc::new(std::sync::Mutex::new(Data::default()));
        // Point the data path at a *directory* so the file write fails.
        let bad = dir.join("as-dir");
        std::fs::create_dir_all(&bad).unwrap();
        let store = MemoryStore::new(state.clone(), bad);
        let err = alloc_with(&store, "/w", &[("api".into(), 9100)], Role::Issue);
        assert!(err.is_err(), "write-through failure must error");
        assert!(
            state.lock().unwrap().entries.is_empty(),
            "memory must be unchanged when the write fails"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-ports store_seam_tests::memorystore`
Expected: FAIL to compile — `MemoryStore` not found.

- [ ] **Step 3: Add `MemoryStore` and `load`**

In `crates/devkit-ports/src/registry.rs`, after the `FlockStore` `impl Store` block, add:

```rust
/// The daemon's authoritative in-memory registry. Reads are served from memory
/// with no flock and no file read; a mutation writes the file through (atomic
/// rename) and updates memory only if that write succeeded — the file is the
/// commit point, so memory and file never diverge and a crash can't orphan a pid.
pub struct MemoryStore {
    state: std::sync::Arc<std::sync::Mutex<Data>>,
    data_path: PathBuf,
}

impl MemoryStore {
    pub fn new(state: std::sync::Arc<std::sync::Mutex<Data>>, data_path: PathBuf) -> Self {
        Self { state, data_path }
    }
}

impl Store for MemoryStore {
    fn snapshot(&self) -> Result<Data> {
        Ok(self.state.lock().expect("registry mutex poisoned").clone())
    }
    fn commit<T>(&self, f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> {
        let mut guard = self.state.lock().expect("registry mutex poisoned");
        let mut next = guard.clone();
        let out = f(&mut next)?;
        next.stamp_version();
        store::save(&self.data_path, &next)?; // commit point: persist before memory
        *guard = next;
        Ok(out)
    }
}

/// Load the registry file into a `Data` for an owner with its own exclusion
/// (the daemon, holding `portd.lock` exclusive, at startup).
pub fn load() -> Data {
    store::load(&paths::registry_file())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-ports store_seam_tests`
Expected: PASS (6 tests).

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/registry.rs
git commit -m "feat(registry): MemoryStore driver with write-through commit point

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Drive `devkit-portd` through `MemoryStore`

The daemon loads the registry into memory once after taking `portd.lock` exclusive, and every handler, the supervision thread, respawn, and adoption operate on that one authoritative `Data` via `MemoryStore`.

**Files:**
- Modify: `src/bin/devkit-portd/main.rs` (`Daemon` struct 26-33; construction 115-126; adoption 128-147; supervision read 172; `respawn` 51-70)
- Modify: `src/bin/devkit-portd/server.rs` (handlers 14-175)

**Interfaces:**
- Consumes: `registry::{MemoryStore, load, alloc_with-equivalents via the public ops, Data}`. Note the generic `*_with` are private to `registry`; the daemon calls the **public** `registry` ops through a `MemoryStore` it owns by using the new helper below.
- Produces: `Daemon.ports: Arc<Mutex<registry::Data>>`; `Daemon::port_store(&self) -> registry::MemoryStore`.

> **Design note for the implementer:** the private `*_with` functions live in `registry`. Rather than make them all `pub`, expose exactly the operations the daemon needs as public `*_in` methods on `MemoryStore` is overkill; instead make the five `*_with` functions `pub` (they already take `&impl Store`). Do that as the first step below.

- [ ] **Step 1: Make the generic ops callable from the daemon**

In `crates/devkit-ports/src/registry.rs`, change the five `fn *_with` signatures to `pub fn`: `pub fn alloc_with`, `pub fn record_pid_with`, `pub fn release_with`, `pub fn snapshot_with`, `pub fn prune_with`. (No body changes.)

Run: `cargo build -p devkit-ports`
Expected: builds; `cargo clippy -p devkit-ports --all-targets -- -D warnings` clean.

- [ ] **Step 2: Write the failing integration test**

Create `crates/devkit-ports/tests/memory_store.rs`:

```rust
use devkit_ports::registry::{self, MemoryStore, Role};
use std::sync::{Arc, Mutex};

#[test]
fn memory_store_serves_reads_from_memory_after_alloc() {
    let dir = std::env::temp_dir().join(format!("devkit-memstore-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let state = Arc::new(Mutex::new(registry::Data::default()));
    let store = MemoryStore::new(state.clone(), dir.join("ports.json"));

    let out = registry::alloc_with(&store, "/w", &[("api".to_string(), 9100)], Role::Issue).unwrap();
    let (_, port) = out[0];

    // A snapshot reflects the alloc straight from memory (no file read needed).
    let snap = registry::snapshot_with(&store).unwrap();
    assert!(snap.entries.contains_key(&port));
    let _ = std::fs::remove_dir_all(&dir);
}
```

Run: `cargo test -p devkit-ports --test memory_store`
Expected: FAIL to compile until Step 1 is done; then PASS. (This locks in that `MemoryStore` + the public ops are usable from outside the crate — the shape the daemon needs.)

- [ ] **Step 3: Give `Daemon` authoritative state**

In `src/bin/devkit-portd/main.rs`, add to the `Daemon` struct (after `sup`):

```rust
    /// Authoritative port registry, served from memory; the file is write-through.
    pub(crate) ports: std::sync::Arc<std::sync::Mutex<registry::Data>>,
```

Add a helper in `impl Daemon`:

```rust
    /// A `Store` view over the daemon's authoritative registry.
    pub(crate) fn port_store(&self) -> registry::MemoryStore {
        registry::MemoryStore::new(self.ports.clone(), devkit_common::paths::registry_file())
    }
```

In `main()`, construct `ports` from the file **after** the `portd.lock` guard is held (right before building the `Arc<Daemon>`), and add it to the struct literal:

```rust
    let ports = std::sync::Arc::new(std::sync::Mutex::new(registry::load()));
    let daemon = Arc::new(Daemon {
        last_activity: Mutex::new(Instant::now()),
        active_conns: AtomicUsize::new(0),
        shutdown: AtomicBool::new(false),
        idle_timeout,
        sup: Mutex::new(supervisor::Supervisor::new(
            max_restarts,
            restart_window,
            mem_warn,
            mem_limit,
        )),
        ports,
    });
```

- [ ] **Step 4: Point adoption, the supervision thread, and respawn at memory**

In `main.rs` adoption (line ~129), replace `registry::snapshot()` with a raw read of the loaded state:

```rust
    // Adopt servers a previous daemon left running: monitor by poll, not waitpid.
    {
        let data = daemon.ports.lock().unwrap().clone();
        let mut sup = daemon.sup.lock().unwrap();
        for (port, e) in &data.entries {
            if let (Some(pid), Some(log)) = (e.pid, e.logfile.clone())
                && registry::pid_alive(pid)
            {
                sup.insert_adopted(
                    supervisor::Key {
                        holder: e.holder.clone(),
                        app: e.app.clone(),
                        role: e.role,
                    },
                    pid,
                    *port,
                    log,
                );
            }
        }
    }
```

In the supervision thread (line ~172), replace the raw flock read with a raw memory read:

```rust
                    let snap = d.ports.lock().unwrap().clone();
```

In `Daemon::respawn` (line ~62), replace `registry::record_pid(...)` with the memory store:

```rust
                let _ = registry::record_pid_with(
                    &self.port_store(),
                    port,
                    &key.app,
                    &key.holder,
                    key.role,
                    pid,
                    log,
                );
```

- [ ] **Step 5: Route every server handler through `MemoryStore`**

In `src/bin/devkit-portd/server.rs`, replace the registry facade calls with the generic ops over `daemon.port_store()`. Change the imports at the top:

```rust
use devkit_ports::registry::{self, MemoryStore, Role};
```

Then in `dispatch`, for the five registry ops use the store:

```rust
        Request::Alloc { holder, reqs, role } => {
            match registry::alloc_with(&daemon.port_store(), &holder, &reqs, role) {
                Ok(ports) => (Response::Ports(ports), false),
                Err(e) => (Response::Err(format!("{e:#}")), false),
            }
        }
        Request::RecordPid { port, app, holder, role, pid, logfile } => {
            match registry::record_pid_with(&daemon.port_store(), port, &app, &holder, role, pid, logfile) {
                Ok(()) => (Response::Ok, false),
                Err(e) => (Response::Err(format!("{e:#}")), false),
            }
        }
        Request::Release { holder, role } => {
            match registry::release_with(&daemon.port_store(), &holder, role) {
                Ok(freed) => (Response::Freed(freed), false),
                Err(e) => (Response::Err(format!("{e:#}")), false),
            }
        }
        Request::Snapshot => match registry::snapshot_with(&daemon.port_store()) {
            Ok(data) => (Response::Snapshot(data), false),
            Err(e) => (Response::Err(format!("{e:#}")), false),
        },
        Request::Prune => match registry::prune_with(&daemon.port_store()) {
            Ok(freed) => (Response::Freed(freed), false),
            Err(e) => (Response::Err(format!("{e:#}")), false),
        },
```

In `supervise_app`, replace the two `registry::alloc(...)` / `registry::record_pid(...)` calls with `registry::alloc_with(&daemon.port_store(), ...)` and `registry::record_pid_with(&daemon.port_store(), ...)` (same arguments, prefixed with the store). In `down`, replace `registry::snapshot()` and `registry::release(&holder, role)` with `registry::snapshot_with(&daemon.port_store())` and `registry::release_with(&daemon.port_store(), &holder, role)`.

`tail` currently takes no `daemon`, so thread it in. Change its dispatch arm from `(tail(holder, app, role, lines), false)` to `(tail(daemon, holder, app, role, lines), false)`, change its signature to `fn tail(daemon: &Arc<Daemon>, holder: String, app: String, role: Option<Role>, lines: usize) -> Response`, and replace its `registry::snapshot()` with `registry::snapshot_with(&daemon.port_store())`.

The `MemoryStore` import is now used; if `Role` becomes unused in `server.rs`, drop it from the `use`.

- [ ] **Step 6: Build, test, and lint the whole workspace**

Run: `cargo test --workspace`
Expected: PASS — including the 4 `devkit-portd` lifecycle tests, the multiprocess race test (`tests/registry.rs`, no daemon → gate free), and the new `memory_store` test.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean. (Watch for an unused `DEVKIT_PORTD_SELF` set in `main.rs` — leave it; it is a harmless belt-and-braces guard now that handlers use `MemoryStore` directly.)

- [ ] **Step 7: Manual smoke test (optional but recommended)**

```bash
cargo build --release
# Start a daemon, allocate via the facade, confirm a direct write is refused.
DEVKIT_DAEMON_IDLE_SECS=60 ./target/release/devkit-portd &
./target/release/portman status   # routes through the daemon's memory
```

Expected: `portman status` succeeds; while the daemon runs, a binary built without the `daemon` feature (or any direct `with_lock` write) reports the gate refusal.

- [ ] **Step 8: Commit**

```bash
git add src/bin/devkit-portd/main.rs src/bin/devkit-portd/server.rs crates/devkit-ports/src/registry.rs crates/devkit-ports/tests/memory_store.rs
git commit -m "feat(portd): serve the port registry from authoritative memory

The daemon loads ports.json into an Arc<Mutex<Data>> after taking portd.lock
exclusive, and drives every handler, the supervision thread, respawn, and
adoption through MemoryStore — reads from memory, mutations write through to the
file. Direct callers are gated out by portd.lock while the daemon is up.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Documentation

Record the new model where the invariants and daemon behavior are documented.

**Files:**
- Modify: `CLAUDE.md` (Registry facade section)
- Modify: `docs/next-steps.md` (mark the ports half done; reframe the locks follow-up)

- [ ] **Step 1: Update `CLAUDE.md`**

In the "Registry facade" section of `CLAUDE.md`, append a paragraph:

```markdown
When a `devkit-portd` daemon is running it is the *authoritative* registry: it
loads `ports.json` into memory under `portd.lock` (held exclusive for its life),
serves reads from memory, and writes through to the file on each mutation. Direct
callers take `portd.lock` *shared* before any write (`FlockStore` / `registry::with_lock`)
and hard-error (`DaemonHoldsLock`) if the daemon holds it — so a non-daemon binary
can never modify `ports.json` behind a live daemon. Reads are ungated.
```

- [ ] **Step 2: Update `docs/next-steps.md`**

Replace the "Route `lock` through the supervisor daemon" section's framing so it reflects that the port registry now has authoritative in-memory mode and the lock registry is the remaining half:

```markdown
## Authoritative in-memory mode for the lock registry

The port registry now serves reads from the daemon's memory and writes through to
the file, with `portd.lock` enforcing the daemon-vs-direct boundary (see
`docs/superpowers/specs/2026-06-21-authoritative-in-memory-portd-design.md`). Give
the lock registry the same treatment: it needs a daemon path built from scratch
(proto variants, client, server dispatch) plus resolved-context facade variants —
the lock facade resolves the project root from CWD and the holder from process
identity client-side, so the server can't reuse the high-level functions directly.
Reuse the `Store` seam and extract the daemon framing/transport/client into
`devkit-common` at that point (a second daemon consumer makes it pay off).
```

- [ ] **Step 3: Verify and commit**

Run: `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`
Expected: still green/clean (docs-only).

```bash
git add CLAUDE.md docs/next-steps.md
git commit -m "docs: record authoritative in-memory port registry; reframe locks follow-up

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage:**
- Consistency model / `portd.lock` gate → Task 2 (`FlockStore` gate + `DaemonHoldsLock`), enforced for all direct writers via gated `registry::with_lock`.
- `Store` seam + one engine + two drivers → Task 2 (trait, `FlockStore`, generic `*_with`) + Task 3 (`MemoryStore`).
- `store::load`/`save` → Task 1.
- Write-through commit point → Task 3 (`MemoryStore::commit` persists before swapping memory; failure test).
- `via_daemon` split → Task 2 (`daemon_request` returns `Result<Option<Response>>`).
- Daemon holds `Mutex<Data>`, loads at startup, all handlers via `MemoryStore` → Task 4.
- Incidental-prune-on-read best-effort → Task 2 (`snapshot_with` swallows a blocked commit) with a test.
- Invariants preserved → reserve-before-bind & record_pid re-insert tested in Task 2; `down` order kept in Task 4 Step 5; `RESERVATION_GRACE_SECS` untouched.
- Docs → Task 5.

**Placeholder scan:** none — every code step shows full code; commands have expected output.

**Type consistency:** `Store::{snapshot,commit}`, `FlockStore::{new,at}`, `MemoryStore::new(Arc<Mutex<Data>>, PathBuf)`, `DaemonHoldsLock`, `daemon_request(req) -> Result<Option<Response>>`, and the five `*_with` signatures are used identically in Tasks 2–4. `Daemon::port_store()` returns `MemoryStore`. The five `*_with` are made `pub` in Task 4 Step 1 before the daemon and the integration test call them.

**Note on `DEVKIT_PORTD_SELF`:** retained as a harmless guard; with handlers on `MemoryStore` the daemon never reaches `daemon_request`, so it is no longer load-bearing.
