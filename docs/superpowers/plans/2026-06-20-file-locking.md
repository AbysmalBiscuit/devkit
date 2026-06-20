# File Locking (`lock`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `lock` binary that lets parallel local sessions coordinate file ownership through advisory path locks, replacing ad-hoc markdown `.lock` files.

**Architecture:** A new library crate `devkit-locks` holds the pure model + a flock-guarded JSON store (twin of the `portman` port registry, keyed by path instead of port). A thin `src/bin/lock.rs` CLI drives it. devkit's state home moves from the Claude-coupled `~/.claude/state/devkit` to the agent-neutral XDG `~/.local/state/devkit`, with a best-effort migration and an in-place fallback.

**Tech Stack:** Rust (edition 2024), `clap` + `clap_complete`, `serde`/`serde_json`, `fd-lock`, `nix`, `comfy-table` (via `devkit-common::ui`).

---

## Reference: shared type & function signatures

These are defined across the tasks below; later tasks rely on these exact names/shapes.

```rust
// devkit-locks::model
pub const SCHEMA_VERSION: u32 = 1;

pub struct LockEntry { pub path: String, pub root: String, pub holder: String,
                       pub pid: Option<u32>, pub note: Option<String>, pub ts: u64, pub ttl: u64 }
pub struct Data { pub version: u32, pub locks: std::collections::BTreeMap<String, LockEntry> }

pub fn key_for(root: &str, path: &str) -> String;       // "{root}\0{path}"
pub fn paths_overlap(a: &str, b: &str) -> bool;          // equal or component-ancestor
pub fn pid_alive(pid: u32) -> bool;
pub fn entry_dead(e: &LockEntry, now: u64) -> bool;      // ttl lapsed OR pid known-and-dead

#[derive(serde::Serialize)] pub struct Acquired { pub path: String, pub ttl_secs: u64 }
#[derive(serde::Serialize)] pub struct Conflict { pub path: String, pub held_by: String,
                                                  pub age_secs: u64, pub note: Option<String> }
pub struct AcquireOutcome { pub acquired: Vec<Acquired>, pub conflicts: Vec<Conflict> }

impl Data {
    pub fn prune_dead(&mut self, now: u64) -> usize;
    pub fn try_acquire(&mut self, root: &str, paths: &[String], holder: &str,
                       pid: Option<u32>, note: Option<&str>, ttl: u64, now: u64) -> AcquireOutcome;
    pub fn check(&self, root: &str, paths: &[String], holder: &str, now: u64) -> Vec<Conflict>;
    pub fn do_release(&mut self, root: &str, paths: &[String], holder: &str, force: bool)
        -> (Vec<String>, Vec<String>);                   // (released, refused)
    pub fn release_all(&mut self, root: &str, holder: &str) -> Vec<String>;
}

// devkit-locks::ident
pub struct Env { pub devkit_session: Option<String>, pub tmux_pane: Option<String>,
                 pub tty: Option<String>, pub ppid: Option<String> }
pub fn resolve_identity(as_flag: Option<&str>, env: &Env) -> String;
pub fn decide_anchor_pid(tmux_pid: Option<u32>, is_tty: bool, ppid: u32) -> Option<u32>;
pub fn identity(as_flag: Option<&str>) -> String;        // reads process env
pub fn anchor_pid() -> Option<u32>;                      // reads process env

// devkit-locks (lib.rs) — public ops the CLI calls
pub fn find_root_from(start: &std::path::Path) -> std::path::PathBuf;  // .git walk-up, else start
pub fn normalize_under_root(abs: &std::path::Path, root: &std::path::Path) -> anyhow::Result<String>;
pub fn acquire(paths: &[String], as_flag: Option<&str>, note: Option<&str>, ttl: u64) -> anyhow::Result<AcquireOutcome>;
pub fn check(paths: &[String], as_flag: Option<&str>) -> anyhow::Result<Vec<Conflict>>;
pub fn release(paths: &[String], as_flag: Option<&str>, force: bool) -> anyhow::Result<(Vec<String>, Vec<String>)>;
pub fn release_all(as_flag: Option<&str>) -> anyhow::Result<Vec<String>>;
pub fn status(all: bool) -> anyhow::Result<Vec<LockEntry>>;
pub fn prune() -> anyhow::Result<usize>;

// devkit-common::paths (added)
pub fn state_dir() -> PathBuf;          // XDG, with in-place legacy fallback
pub fn locks_file() -> PathBuf;         // state_dir()/locks.json
pub fn locks_lock() -> PathBuf;         // state_dir()/locks.lock
pub fn migrate_legacy_state();          // one-time best-effort rename
```

---

## Task 1: Scaffold the `devkit-locks` crate

**Files:**
- Create: `crates/devkit-locks/Cargo.toml`
- Create: `crates/devkit-locks/src/lib.rs`
- Modify: `Cargo.toml` (workspace members + workspace.dependencies)

- [ ] **Step 1: Create the crate manifest**

Create `crates/devkit-locks/Cargo.toml`:

```toml
[package]
name = "devkit-locks"
edition.workspace = true
version.workspace = true

[dependencies]
anyhow.workspace = true
serde = { workspace = true }
serde_json.workspace = true
fd-lock.workspace = true
nix.workspace = true
devkit-common.workspace = true
```

- [ ] **Step 2: Create a minimal lib with a smoke test**

Create `crates/devkit-locks/src/lib.rs`:

```rust
pub mod ident;
pub mod model;
pub mod store;

#[cfg(test)]
mod smoke {
    #[test]
    fn crate_builds() {
        assert_eq!(2 + 2, 4);
    }
}
```

(`ident`, `model`, `store` modules are filled in by later tasks. Create them now as empty files so the crate compiles.)

Create empty `crates/devkit-locks/src/model.rs`, `crates/devkit-locks/src/ident.rs`, `crates/devkit-locks/src/store.rs` (each containing only a line comment `//! filled in by a later task`).

- [ ] **Step 3: Register the crate in the workspace**

In `Cargo.toml` (root), add the member and the workspace dependency.

Change:
```toml
[workspace]
resolver = "3"
members = ["crates/devkit-common", "crates/devkit-ports"]
```
to:
```toml
[workspace]
resolver = "3"
members = ["crates/devkit-common", "crates/devkit-ports", "crates/devkit-locks"]
```

Add to the `[workspace.dependencies]` table (next to the other `devkit-*` entries):
```toml
devkit-locks = { path = "crates/devkit-locks" }
```

- [ ] **Step 4: Verify it builds**

Run: `cargo build -p devkit-locks`
Expected: compiles clean (the smoke test isn't run yet).

Run: `cargo test -p devkit-locks`
Expected: PASS (`crate_builds`).

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-locks Cargo.toml
git commit -m "feat(locks): scaffold devkit-locks crate"
```

---

## Task 2: Agent-neutral state home in `paths.rs`

**Files:**
- Modify: `crates/devkit-common/src/paths.rs`

State resolution must be **pure** (no destructive I/O) so `cargo test` never moves real data; migration is a separate, explicit, sandbox-testable step. `state_dir()` returns the XDG path when it exists, otherwise the legacy dir when *it* exists (in-place fallback), otherwise the XDG path.

- [ ] **Step 1: Write failing tests for the pure resolver and migration**

In `crates/devkit-common/src/paths.rs`, replace the existing `#[cfg(test)] mod tests { … }` block with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_under_state() {
        assert!(registry_file().ends_with("devkit/ports.json"));
        assert!(logs_dir().ends_with("devkit/logs"));
    }
    #[test]
    fn lock_paths_under_state() {
        assert!(locks_file().ends_with("devkit/locks.json"));
        assert!(locks_lock().ends_with("devkit/locks.lock"));
    }
    #[test]
    fn leaf_is_basename() {
        assert_eq!(leaf("/a/b/eng-1234"), Some("eng-1234"));
        assert_eq!(leaf("solo"), Some("solo"));
    }
    #[test]
    fn daemon_paths_under_state() {
        assert!(socket_file().ends_with("devkit/portd.sock"));
        assert!(daemon_lock_file().ends_with("devkit/portd.lock"));
        assert!(daemon_log().ends_with("devkit/logs/portd.log"));
    }

    #[test]
    fn pick_prefers_new_when_present() {
        let n = PathBuf::from("/new/devkit");
        let l = PathBuf::from("/legacy/devkit");
        assert_eq!(pick_state_dir(n.clone(), l.clone(), true, true), n);
        assert_eq!(pick_state_dir(n.clone(), l.clone(), true, false), n);
    }
    #[test]
    fn pick_falls_back_to_legacy_in_place() {
        let n = PathBuf::from("/new/devkit");
        let l = PathBuf::from("/legacy/devkit");
        assert_eq!(pick_state_dir(n.clone(), l.clone(), false, true), l);
    }
    #[test]
    fn pick_defaults_to_new_when_neither_exists() {
        let n = PathBuf::from("/new/devkit");
        let l = PathBuf::from("/legacy/devkit");
        assert_eq!(pick_state_dir(n.clone(), l.clone(), false, false), n);
    }
    #[test]
    fn migrate_moves_legacy_to_new() {
        let base = std::env::temp_dir().join(format!("devkit-paths-{}", std::process::id()));
        let new = base.join("new/devkit");
        let legacy = base.join("legacy/devkit");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("ports.json"), b"{}").unwrap();

        migrate_state_between(&new, &legacy);

        assert!(new.join("ports.json").exists(), "data moved to new home");
        assert!(!legacy.exists(), "legacy home removed after move");
        let _ = std::fs::remove_dir_all(&base);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-common paths`
Expected: FAIL — `locks_file`, `locks_lock`, `pick_state_dir`, `migrate_state_between` don't exist yet.

- [ ] **Step 3: Implement the new state home**

In `crates/devkit-common/src/paths.rs`, replace the top of the file (the `state_dir` definition and its derived helpers) so it reads:

```rust
use std::path::{Path, PathBuf};

/// Agent-neutral state home: `$XDG_STATE_HOME/devkit` (default `~/.local/state/devkit`).
///
/// Pure resolution (stat only, never writes): prefer the XDG path when it exists;
/// otherwise fall back to the legacy `~/.claude/state/devkit` in place when it exists
/// (so live state is never orphaned before `migrate_legacy_state` runs); otherwise the
/// XDG path. Run `migrate_legacy_state()` once at process startup to move the data.
pub fn state_dir() -> PathBuf {
    let new = xdg_state_home().join("devkit");
    let legacy = home().join(".claude/state/devkit");
    let (ne, le) = (new.exists(), legacy.exists());
    pick_state_dir(new, legacy, ne, le)
}

fn pick_state_dir(new: PathBuf, legacy: PathBuf, new_exists: bool, legacy_exists: bool) -> PathBuf {
    if new_exists {
        new
    } else if legacy_exists {
        legacy
    } else {
        new
    }
}

fn xdg_state_home() -> PathBuf {
    match std::env::var_os("XDG_STATE_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x),
        _ => home().join(".local/state"),
    }
}

/// One-time best-effort migration of the legacy `~/.claude/state/devkit` home to the
/// XDG state dir. No-op if the new home already exists or the legacy one is absent.
/// On rename failure (cross-device, permissions) the legacy dir is left in place and
/// `state_dir()` keeps resolving to it.
pub fn migrate_legacy_state() {
    migrate_state_between(&xdg_state_home().join("devkit"), &home().join(".claude/state/devkit"));
}

fn migrate_state_between(new: &Path, legacy: &Path) {
    if new.exists() || !legacy.exists() {
        return;
    }
    if let Some(parent) = new.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::rename(legacy, new);
}

pub fn registry_file() -> PathBuf { state_dir().join("ports.json") }
pub fn lock_file() -> PathBuf { state_dir().join("ports.lock") }
pub fn locks_file() -> PathBuf { state_dir().join("locks.json") }
pub fn locks_lock() -> PathBuf { state_dir().join("locks.lock") }
pub fn logs_dir() -> PathBuf { state_dir().join("logs") }
/// Unix socket the daemon binds; clients connect here.
pub fn socket_file() -> PathBuf { state_dir().join("portd.sock") }
/// Single-instance lock for the daemon — separate from the registry's `ports.lock`.
pub fn daemon_lock_file() -> PathBuf { state_dir().join("portd.lock") }
/// Daemon log file.
pub fn daemon_log() -> PathBuf { logs_dir().join("portd.log") }
```

Leave `cache_dir()`, `home()`, and `leaf()` as they are. Remove the now-duplicate `use std::path::PathBuf;` at the original top of the file (the new block imports `Path, PathBuf`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-common paths`
Expected: PASS (all 8 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-common/src/paths.rs
git commit -m "feat(paths): move state home to XDG ~/.local/state/devkit with legacy fallback"
```

---

## Task 3: Lock model — types & overlap

**Files:**
- Modify: `crates/devkit-locks/src/model.rs`

- [ ] **Step 1: Write failing tests for types & overlap**

Replace `crates/devkit-locks/src/model.rs` with the struct/const definitions plus tests (implementation of ops comes in Task 4). First write just the data types and `key_for`/`paths_overlap`, with these tests at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_joins_root_and_path() {
        assert_eq!(key_for("/repo", "scenes/x"), "/repo\u{0}scenes/x");
    }
    #[test]
    fn overlap_equal_paths() {
        assert!(paths_overlap("scenes/x", "scenes/x"));
    }
    #[test]
    fn overlap_ancestor_either_direction() {
        assert!(paths_overlap("scenes", "scenes/player.tscn"));
        assert!(paths_overlap("scenes/player.tscn", "scenes"));
    }
    #[test]
    fn no_overlap_sibling_prefix() {
        assert!(!paths_overlap("scenes", "scenes-old"));
        assert!(!paths_overlap("scenes/a", "scenes/b"));
    }
    #[test]
    fn root_dot_overlaps_everything() {
        assert!(paths_overlap(".", "scenes/x"));
        assert!(paths_overlap("scenes/x", "."));
        assert!(paths_overlap(".", "."));
    }
    #[test]
    fn roundtrip_serde() {
        let mut d = Data::default();
        d.locks.insert(
            key_for("/repo", "scenes"),
            LockEntry { path: "scenes".into(), root: "/repo".into(), holder: "alice".into(),
                        pid: None, note: Some("refactor".into()), ts: 5, ttl: 1800 },
        );
        let s = serde_json::to_string(&d).unwrap();
        let back: Data = serde_json::from_str(&s).unwrap();
        assert_eq!(back.locks[&key_for("/repo", "scenes")].holder, "alice");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-locks model`
Expected: FAIL — types/functions undefined.

- [ ] **Step 3: Implement the types & overlap**

At the top of `crates/devkit-locks/src/model.rs` (above the test module):

```rust
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// On-disk schema version. Bump when the layout changes incompatibly.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEntry {
    /// Project-root-relative, normalized ('/'-separated; root itself is ".").
    pub path: String,
    /// Absolute project root the path is relative to.
    pub root: String,
    /// Resolved session identity holding the lock.
    pub holder: String,
    /// Durable anchor pid, when one could be trusted (None for agent sessions).
    #[serde(default)]
    pub pid: Option<u32>,
    /// Optional human-readable intent.
    #[serde(default)]
    pub note: Option<String>,
    /// Acquired/renewed at, unix seconds.
    pub ts: u64,
    /// Time-to-live in seconds; 0 means no expiry.
    pub ttl: u64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Data {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub locks: BTreeMap<String, LockEntry>,
}

/// Registry key for a claim: root and path joined by a NUL (never valid in a path).
pub fn key_for(root: &str, path: &str) -> String {
    format!("{root}\u{0}{path}")
}

/// True if two root-relative normalized paths overlap: equal, or one is a
/// path-component ancestor of the other. "." (the project root) overlaps everything.
pub fn paths_overlap(a: &str, b: &str) -> bool {
    if a == b || a == "." || b == "." {
        return true;
    }
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    long.strip_prefix(short).is_some_and(|rest| rest.starts_with('/'))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-locks model`
Expected: PASS (6 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-locks/src/model.rs
git commit -m "feat(locks): lock entry model and path-overlap detection"
```

---

## Task 4: Lock model — operations

**Files:**
- Modify: `crates/devkit-locks/src/model.rs`

- [ ] **Step 1: Write failing tests for the operations**

Add these tests inside the existing `#[cfg(test)] mod tests` block in `model.rs`:

```rust
    fn entry(root: &str, path: &str, holder: &str, ts: u64, ttl: u64, pid: Option<u32>) -> (String, LockEntry) {
        (key_for(root, path),
         LockEntry { path: path.into(), root: root.into(), holder: holder.into(),
                     pid, note: None, ts, ttl })
    }

    #[test]
    fn acquire_inserts_and_is_idempotent_renew() {
        let mut d = Data::default();
        let r = d.try_acquire("/repo", &["scenes".into()], "alice", None, None, 1800, 100);
        assert!(r.conflicts.is_empty());
        assert_eq!(r.acquired.len(), 1);
        // re-acquire renews ts, no duplicate
        let r2 = d.try_acquire("/repo", &["scenes".into()], "alice", None, None, 1800, 250);
        assert!(r2.conflicts.is_empty());
        assert_eq!(d.locks.len(), 1);
        assert_eq!(d.locks[&key_for("/repo", "scenes")].ts, 250);
    }

    #[test]
    fn acquire_conflicts_for_other_holder_overlap() {
        let mut d = Data::default();
        let (k, e) = entry("/repo", "scenes", "alice", 100, 1800, None);
        d.locks.insert(k, e);
        let r = d.try_acquire("/repo", &["scenes/player.tscn".into()], "bob", None, None, 1800, 140);
        assert_eq!(r.acquired.len(), 0);
        assert_eq!(r.conflicts.len(), 1);
        assert_eq!(r.conflicts[0].held_by, "alice");
        assert_eq!(r.conflicts[0].age_secs, 40);
    }

    #[test]
    fn acquire_is_all_or_nothing() {
        let mut d = Data::default();
        let (k, e) = entry("/repo", "scenes", "alice", 100, 1800, None);
        d.locks.insert(k, e);
        let r = d.try_acquire("/repo", &["art".into(), "scenes/x".into()], "bob", None, None, 1800, 120);
        assert!(r.acquired.is_empty(), "no path acquired when any conflicts");
        assert_eq!(r.conflicts.len(), 1);
        assert_eq!(d.locks.len(), 1, "nothing new inserted");
    }

    #[test]
    fn other_root_never_conflicts() {
        let mut d = Data::default();
        let (k, e) = entry("/repoA", "scenes", "alice", 100, 1800, None);
        d.locks.insert(k, e);
        let r = d.try_acquire("/repoB", &["scenes".into()], "bob", None, None, 1800, 120);
        assert!(r.conflicts.is_empty());
        assert_eq!(r.acquired.len(), 1);
    }

    #[test]
    fn prune_drops_ttl_expired_keeps_live() {
        let mut d = Data::default();
        d.locks.extend([
            entry("/repo", "old", "alice", 100, 60, None),   // expired by now=1000
            entry("/repo", "fresh", "alice", 990, 60, None),  // still live
            entry("/repo", "forever", "alice", 1, 0, None),   // ttl 0 never expires
        ]);
        let removed = d.prune_dead(1000);
        assert_eq!(removed, 1);
        assert!(d.locks.contains_key(&key_for("/repo", "fresh")));
        assert!(d.locks.contains_key(&key_for("/repo", "forever")));
    }

    #[test]
    fn prune_drops_dead_pid() {
        let mut d = Data::default();
        // pid 1 is init (alive); use a pid that cannot be alive: u32::MAX.
        d.locks.extend([entry("/repo", "p", "alice", 1, 0, Some(u32::MAX))]);
        assert_eq!(d.prune_dead(2), 1);
    }

    #[test]
    fn release_by_holder_and_force() {
        let mut d = Data::default();
        d.locks.extend([
            entry("/repo", "a", "alice", 1, 0, None),
            entry("/repo", "b", "bob", 1, 0, None),
        ]);
        // alice cannot release bob's lock without force
        let (rel, refused) = d.do_release("/repo", &["b".into()], "alice", false);
        assert!(rel.is_empty());
        assert_eq!(refused, vec!["b".to_string()]);
        // alice releases her own
        let (rel, _) = d.do_release("/repo", &["a".into()], "alice", false);
        assert_eq!(rel, vec!["a".to_string()]);
        // force removes bob's
        let (rel, _) = d.do_release("/repo", &["b".into()], "alice", true);
        assert_eq!(rel, vec!["b".to_string()]);
        assert!(d.locks.is_empty());
    }

    #[test]
    fn release_all_clears_only_callers_locks_in_root() {
        let mut d = Data::default();
        d.locks.extend([
            entry("/repo", "a", "alice", 1, 0, None),
            entry("/repo", "b", "bob", 1, 0, None),
            entry("/other", "c", "alice", 1, 0, None),
        ]);
        let rel = d.release_all("/repo", "alice");
        assert_eq!(rel, vec!["a".to_string()]);
        assert!(d.locks.contains_key(&key_for("/repo", "b")));
        assert!(d.locks.contains_key(&key_for("/other", "c")));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-locks model`
Expected: FAIL — `try_acquire`, `prune_dead`, `do_release`, `release_all`, `pid_alive`, `entry_dead`, `Acquired`, `Conflict`, `AcquireOutcome` undefined.

- [ ] **Step 3: Implement the operations**

Add to `crates/devkit-locks/src/model.rs` (above the test module):

```rust
#[derive(Debug, Clone, Serialize)]
pub struct Acquired {
    pub path: String,
    pub ttl_secs: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Conflict {
    pub path: String,
    pub held_by: String,
    pub age_secs: u64,
    pub note: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct AcquireOutcome {
    pub acquired: Vec<Acquired>,
    pub conflicts: Vec<Conflict>,
}

/// True if a process with this pid currently exists (signal 0 probe).
pub fn pid_alive(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), None).is_ok()
}

/// A lock is dead when its TTL has lapsed, or its anchor pid is known and gone.
pub fn entry_dead(e: &LockEntry, now: u64) -> bool {
    if e.ttl != 0 && now.saturating_sub(e.ts) >= e.ttl {
        return true;
    }
    match e.pid {
        Some(pid) => !pid_alive(pid),
        None => false,
    }
}

impl Data {
    /// Remove every dead lock; returns how many were dropped.
    pub fn prune_dead(&mut self, now: u64) -> usize {
        let before = self.locks.len();
        self.locks.retain(|_, e| !entry_dead(e, now));
        before - self.locks.len()
    }

    /// Conflicts that would block acquiring `paths` for `holder` in `root`:
    /// any live lock by a *different* holder whose path overlaps a requested path.
    pub fn check(&self, root: &str, paths: &[String], holder: &str, now: u64) -> Vec<Conflict> {
        let mut out = Vec::new();
        for req in paths {
            for e in self.locks.values() {
                if e.root == root
                    && e.holder != holder
                    && !entry_dead(e, now)
                    && paths_overlap(&e.path, req)
                {
                    out.push(Conflict {
                        path: req.clone(),
                        held_by: e.holder.clone(),
                        age_secs: now.saturating_sub(e.ts),
                        note: e.note.clone(),
                    });
                }
            }
        }
        out
    }

    /// All-or-nothing acquire: if any requested path conflicts, acquire none and
    /// return the conflicts. Otherwise insert (or renew, for the same holder+path).
    pub fn try_acquire(
        &mut self, root: &str, paths: &[String], holder: &str,
        pid: Option<u32>, note: Option<&str>, ttl: u64, now: u64,
    ) -> AcquireOutcome {
        let conflicts = self.check(root, paths, holder, now);
        if !conflicts.is_empty() {
            return AcquireOutcome { acquired: Vec::new(), conflicts };
        }
        let mut acquired = Vec::with_capacity(paths.len());
        for req in paths {
            self.locks.insert(
                key_for(root, req),
                LockEntry {
                    path: req.clone(), root: root.into(), holder: holder.into(),
                    pid, note: note.map(str::to_string), ts: now, ttl,
                },
            );
            acquired.push(Acquired { path: req.clone(), ttl_secs: ttl });
        }
        AcquireOutcome { acquired, conflicts: Vec::new() }
    }

    /// Release named paths held by `holder` in `root`. Without `force`, a path held
    /// by another holder is refused (not freed). Returns (released, refused).
    pub fn do_release(
        &mut self, root: &str, paths: &[String], holder: &str, force: bool,
    ) -> (Vec<String>, Vec<String>) {
        let mut released = Vec::new();
        let mut refused = Vec::new();
        for req in paths {
            let key = key_for(root, req);
            match self.locks.get(&key) {
                Some(e) if e.holder == holder || force => {
                    self.locks.remove(&key);
                    released.push(req.clone());
                }
                Some(_) => refused.push(req.clone()),
                None => {} // not held: silently ignore
            }
        }
        (released, refused)
    }

    /// Release every lock held by `holder` in `root`; returns the freed paths.
    pub fn release_all(&mut self, root: &str, holder: &str) -> Vec<String> {
        let freed: Vec<String> = self.locks.values()
            .filter(|e| e.root == root && e.holder == holder)
            .map(|e| e.path.clone())
            .collect();
        for p in &freed {
            self.locks.remove(&key_for(root, p));
        }
        freed
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-locks model`
Expected: PASS (all model tests, including the 6 from Task 3).

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-locks/src/model.rs
git commit -m "feat(locks): acquire/release/check/prune operations"
```

---

## Task 5: Session identity & anchor PID

**Files:**
- Modify: `crates/devkit-locks/src/ident.rs`

- [ ] **Step 1: Write failing tests**

Put these tests at the bottom of `crates/devkit-locks/src/ident.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn env(session: Option<&str>, pane: Option<&str>, tty: Option<&str>, ppid: Option<&str>) -> Env {
        Env {
            devkit_session: session.map(str::to_string),
            tmux_pane: pane.map(str::to_string),
            tty: tty.map(str::to_string),
            ppid: ppid.map(str::to_string),
        }
    }

    #[test]
    fn explicit_flag_wins() {
        let e = env(Some("envsess"), Some("%3"), Some("/dev/pts/1"), Some("42"));
        assert_eq!(resolve_identity(Some("flag"), &e), "flag");
    }
    #[test]
    fn env_session_beats_tmux() {
        let e = env(Some("envsess"), Some("%3"), None, Some("42"));
        assert_eq!(resolve_identity(None, &e), "envsess");
    }
    #[test]
    fn tmux_pane_beats_tty_and_ppid() {
        let e = env(None, Some("%3"), Some("/dev/pts/1"), Some("42"));
        assert_eq!(resolve_identity(None, &e), "%3");
    }
    #[test]
    fn falls_through_to_tty_then_ppid() {
        assert_eq!(resolve_identity(None, &env(None, None, Some("/dev/pts/1"), Some("42"))), "/dev/pts/1");
        assert_eq!(resolve_identity(None, &env(None, None, None, Some("42"))), "42");
    }

    #[test]
    fn anchor_pid_prefers_tmux_then_tty_else_none() {
        assert_eq!(decide_anchor_pid(Some(5), true, 9), Some(5));
        assert_eq!(decide_anchor_pid(None, true, 9), Some(9));
        assert_eq!(decide_anchor_pid(None, false, 9), None); // agent-via-Bash: no durable pid
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-locks ident`
Expected: FAIL — `Env`, `resolve_identity`, `decide_anchor_pid` undefined.

- [ ] **Step 3: Implement identity resolution**

Put above the test module in `crates/devkit-locks/src/ident.rs`:

```rust
/// Environment inputs for identity resolution, captured so the logic is pure/testable.
pub struct Env {
    pub devkit_session: Option<String>,
    pub tmux_pane: Option<String>,
    pub tty: Option<String>,
    pub ppid: Option<String>,
}

impl Env {
    pub fn from_process() -> Self {
        let nonempty = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
        Env {
            devkit_session: nonempty("DEVKIT_SESSION"),
            tmux_pane: nonempty("TMUX_PANE"),
            tty: ttyname(),
            ppid: Some(nix::unistd::getppid().as_raw().to_string()),
        }
    }
}

fn ttyname() -> Option<String> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return None;
    }
    nix::unistd::ttyname(std::io::stdin())
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Resolve the holder identity by precedence:
/// `--as` > `$DEVKIT_SESSION` > `$TMUX_PANE` > controlling tty > parent pid.
pub fn resolve_identity(as_flag: Option<&str>, env: &Env) -> String {
    as_flag.map(str::to_string)
        .or_else(|| env.devkit_session.clone())
        .or_else(|| env.tmux_pane.clone())
        .or_else(|| env.tty.clone())
        .or_else(|| env.ppid.clone())
        .unwrap_or_else(|| "unknown".to_string())
}

/// A durable anchor pid, recorded only when one can be trusted: the tmux pane's
/// process, else a parent pid when attached to a tty. Agent-via-Bash sessions (no
/// tmux, no tty) get None and rely on TTL + explicit release.
pub fn decide_anchor_pid(tmux_pid: Option<u32>, is_tty: bool, ppid: u32) -> Option<u32> {
    if let Some(p) = tmux_pid {
        return Some(p);
    }
    if is_tty {
        return Some(ppid);
    }
    None
}

pub fn identity(as_flag: Option<&str>) -> String {
    resolve_identity(as_flag, &Env::from_process())
}

pub fn anchor_pid() -> Option<u32> {
    use std::io::IsTerminal;
    decide_anchor_pid(tmux_pane_pid(), std::io::stdin().is_terminal(),
                      nix::unistd::getppid().as_raw() as u32)
}

/// Best-effort: ask tmux for the current pane's process pid when inside tmux.
fn tmux_pane_pid() -> Option<u32> {
    if std::env::var_os("TMUX_PANE").is_none() {
        return None;
    }
    let out = std::process::Command::new("tmux")
        .args(["display-message", "-p", "-F", "#{pane_pid}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-locks ident`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-locks/src/ident.rs
git commit -m "feat(locks): session identity precedence and anchor-pid policy"
```

---

## Task 6: Flock-guarded JSON store

**Files:**
- Modify: `crates/devkit-locks/src/store.rs`

- [ ] **Step 1: Write failing tests for read/write/salvage**

Put these tests at the bottom of `crates/devkit-locks/src/store.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{key_for, Data, LockEntry};

    fn scratch_file(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("devkit-store-{}-{}.json", std::process::id(), tag))
    }

    #[test]
    fn write_then_read_roundtrips() {
        let p = scratch_file("rt");
        let mut d = Data::default();
        d.locks.insert(
            key_for("/repo", "scenes"),
            LockEntry { path: "scenes".into(), root: "/repo".into(), holder: "alice".into(),
                        pid: None, note: None, ts: 7, ttl: 1800 },
        );
        write(&p, &d).unwrap();
        let back = read(&p);
        assert_eq!(back.locks[&key_for("/repo", "scenes")].holder, "alice");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_missing_is_default() {
        let p = scratch_file("missing");
        let _ = std::fs::remove_file(&p);
        assert!(read(&p).locks.is_empty());
    }

    #[test]
    fn salvage_recovers_locks_from_drifted_schema() {
        let json = r#"{"version":"oops","locks":{"/repo scenes":{"path":"scenes","root":"/repo","holder":"alice","pid":null,"note":null,"ts":7,"ttl":1800}}}"#;
        assert!(serde_json::from_str::<Data>(json).is_err());
        let d = salvage(json).expect("locks object present");
        assert_eq!(d.locks[&key_for("/repo", "scenes")].holder, "alice");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-locks store`
Expected: FAIL — `read`, `write`, `salvage` undefined.

- [ ] **Step 3: Implement the store**

Put above the test module in `crates/devkit-locks/src/store.rs`:

```rust
use crate::model::{Data, LockEntry, SCHEMA_VERSION};
use anyhow::{Context, Result};
use devkit_common::paths;
use fd_lock::RwLock;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::Path;

fn read(path: &Path) -> Data {
    let s = match fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => return Data::default(),
    };
    match serde_json::from_str::<Data>(&s) {
        Ok(d) => d,
        Err(_) => match salvage(&s) {
            Some(d) => {
                eprintln!("warning: lock registry schema differs; salvaged {} lock(s)", d.locks.len());
                d
            }
            None => {
                let _ = fs::rename(path, path.with_extension("json.bak"));
                eprintln!("warning: unreadable lock registry; backed up and reinitialised");
                Data::default()
            }
        },
    }
}

/// Best-effort recovery of still-parseable locks from a registry whose top-level
/// schema has drifted. None only if there's no `locks` object at all.
fn salvage(s: &str) -> Option<Data> {
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    let obj = v.get("locks")?.as_object()?;
    let mut locks = BTreeMap::new();
    for (k, val) in obj {
        if let Ok(entry) = serde_json::from_value::<LockEntry>(val.clone()) {
            locks.insert(k.clone(), entry);
        }
    }
    Some(Data { version: 0, locks })
}

fn write(path: &Path, data: &Data) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(data)?)?;
    fs::rename(&tmp, path).context("atomically replacing lock registry")?;
    Ok(())
}

/// Run `f` while holding the exclusive lock-registry file lock; persists the result.
/// Liveness probes here are cheap, non-blocking syscalls (`kill(0)`) and TTL
/// arithmetic, so — unlike the port registry's TCP probes — pruning runs inside the
/// lock without risk of holding it across a blocking call.
pub fn with_lock<T>(f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> {
    fs::create_dir_all(paths::state_dir())?;
    let lock_path = paths::locks_lock();
    let _ = OpenOptions::new().create(true).write(true).truncate(false).open(&lock_path)?;
    let mut lock = RwLock::new(File::open(&lock_path)?);
    let _guard = lock.write()?; // blocks until exclusive
    let reg = paths::locks_file();
    let mut data = read(&reg);
    let out = f(&mut data)?;
    data.version = SCHEMA_VERSION;
    write(&reg, &data)?;
    Ok(out)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-locks store`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-locks/src/store.rs
git commit -m "feat(locks): flock-guarded JSON lock store with salvage"
```

---

## Task 7: Public ops — root detection, normalization, wiring

**Files:**
- Modify: `crates/devkit-locks/src/lib.rs`

- [ ] **Step 1: Write failing tests for root detection & normalization**

Replace the `#[cfg(test)] mod smoke { … }` block in `crates/devkit-locks/src/lib.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("devkit-lib-{}-{}", std::process::id(), tag));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn find_root_walks_up_to_git() {
        let root = scratch("git-root");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let deep = root.join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_root_from(&deep), root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn find_root_falls_back_to_start_without_git() {
        let start = scratch("no-git");
        assert_eq!(find_root_from(&start), start);
        let _ = std::fs::remove_dir_all(&start);
    }

    #[test]
    fn normalize_makes_root_relative() {
        let root = Path::new("/repo");
        assert_eq!(normalize_under_root(Path::new("/repo/scenes/x.tscn"), root).unwrap(), "scenes/x.tscn");
        assert_eq!(normalize_under_root(Path::new("/repo/./scenes/"), root).unwrap(), "scenes");
        assert_eq!(normalize_under_root(Path::new("/repo"), root).unwrap(), ".");
    }

    #[test]
    fn normalize_rejects_outside_root() {
        assert!(normalize_under_root(Path::new("/elsewhere/x"), Path::new("/repo")).is_err());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-locks --lib tests`
Expected: FAIL — `find_root_from`, `normalize_under_root` undefined.

- [ ] **Step 3: Implement root detection, normalization, and the public ops**

Replace the module declarations at the top of `crates/devkit-locks/src/lib.rs` (keep `pub mod ident; pub mod model; pub mod store;`) and add below them:

```rust
pub mod ident;
pub mod model;
pub mod store;

use anyhow::{Context, Result};
use model::{AcquireOutcome, Conflict, Data, LockEntry};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// Nearest ancestor of `start` containing a `.git` entry; `start` itself if none.
pub fn find_root_from(start: &Path) -> PathBuf {
    let mut dir = start;
    loop {
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => return start.to_path_buf(),
        }
    }
}

fn find_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("getting current dir")?;
    Ok(find_root_from(&cwd))
}

/// Lexically clean `abs` and express it relative to `root` ('/'-separated; the root
/// itself becomes "."). Errors if `abs` is not under `root`.
pub fn normalize_under_root(abs: &Path, root: &Path) -> Result<String> {
    let rel = abs.strip_prefix(root).ok().context("path is outside the project root")?;
    let mut parts: Vec<String> = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str().context("non-utf8 path")?.to_string()),
            Component::CurDir => {}
            Component::ParentDir => { parts.pop(); }
            _ => {}
        }
    }
    Ok(if parts.is_empty() { ".".to_string() } else { parts.join("/") })
}

/// Resolve a CLI path argument (absolute or cwd-relative) to a root-relative key.
fn normalize_arg(arg: &str, cwd: &Path, root: &Path) -> Result<String> {
    let p = Path::new(arg);
    let abs = if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) };
    normalize_under_root(&abs, root)
}

struct Ctx {
    root: String,
    holder: String,
    paths: Vec<String>,
}

fn ctx(paths_in: &[String], as_flag: Option<&str>) -> Result<Ctx> {
    let cwd = std::env::current_dir().context("getting current dir")?;
    let root = find_root_from(&cwd);
    let mut paths = Vec::with_capacity(paths_in.len());
    for a in paths_in {
        paths.push(normalize_arg(a, &cwd, &root)?);
    }
    Ok(Ctx {
        root: root.to_string_lossy().into_owned(),
        holder: ident::identity(as_flag),
        paths,
    })
}

pub fn acquire(paths_in: &[String], as_flag: Option<&str>, note: Option<&str>, ttl: u64) -> Result<AcquireOutcome> {
    let c = ctx(paths_in, as_flag)?;
    let pid = ident::anchor_pid();
    store::with_lock(|d| {
        d.prune_dead(now());
        Ok(d.try_acquire(&c.root, &c.paths, &c.holder, pid, note, ttl, now()))
    })
}

pub fn check(paths_in: &[String], as_flag: Option<&str>) -> Result<Vec<Conflict>> {
    let c = ctx(paths_in, as_flag)?;
    store::with_lock(|d| {
        d.prune_dead(now());
        Ok(d.check(&c.root, &c.paths, &c.holder, now()))
    })
}

pub fn release(paths_in: &[String], as_flag: Option<&str>, force: bool) -> Result<(Vec<String>, Vec<String>)> {
    let c = ctx(paths_in, as_flag)?;
    store::with_lock(|d| Ok(d.do_release(&c.root, &c.paths, &c.holder, force)))
}

pub fn release_all(as_flag: Option<&str>) -> Result<Vec<String>> {
    let c = ctx(&[], as_flag)?;
    store::with_lock(|d| Ok(d.release_all(&c.root, &c.holder)))
}

/// Live locks for the current project root, or every project when `all`.
pub fn status(all: bool) -> Result<Vec<LockEntry>> {
    let root = find_root()?.to_string_lossy().into_owned();
    store::with_lock(|d: &mut Data| {
        d.prune_dead(now());
        let mut out: Vec<LockEntry> = d.locks.values()
            .filter(|e| all || e.root == root)
            .cloned()
            .collect();
        out.sort_by(|a, b| (a.root.as_str(), a.path.as_str()).cmp(&(b.root.as_str(), b.path.as_str())));
        Ok(out)
    })
}

pub fn prune() -> Result<usize> {
    store::with_lock(|d| Ok(d.prune_dead(now())))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-locks`
Expected: PASS (lib tests + model/ident/store tests).

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-locks/src/lib.rs
git commit -m "feat(locks): root detection, path normalization, and public ops"
```

---

## Task 8: The `lock` CLI binary

**Files:**
- Create: `src/bin/lock.rs`
- Modify: `Cargo.toml` (root — add `devkit-locks` dependency)
- Modify: `src/bin/portman.rs`, `src/bin/devrun/main.rs`, `src/bin/issue/main.rs` (one-line startup migration call)

- [ ] **Step 1: Add the dependency**

In `Cargo.toml` (root) `[dependencies]`, add next to `devkit-ports.workspace = true`:
```toml
devkit-locks.workspace = true
```

- [ ] **Step 2: Write the CLI**

Create `src/bin/lock.rs`:

```rust
use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use devkit_locks::model::{Conflict, LockEntry};

#[derive(Parser)]
#[command(name = "lock", about = "Advisory file locks for parallel local sessions")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Claim one or more paths (files or directories). Fails if any is held by another session.
    Acquire {
        paths: Vec<String>,
        #[arg(long = "as")]
        holder: Option<String>,
        #[arg(long)]
        note: Option<String>,
        /// Lock lifetime, seconds (0 = no expiry). Default 1800 (30 min).
        #[arg(long, default_value_t = 1800)]
        ttl: u64,
        #[arg(long)]
        json: bool,
    },
    /// Read-only: would `acquire` of these paths succeed?
    Check {
        paths: Vec<String>,
        #[arg(long = "as")]
        holder: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Release your claims on the named paths (or all of them with --all).
    Release {
        paths: Vec<String>,
        #[arg(long = "as")]
        holder: Option<String>,
        #[arg(long)]
        all: bool,
        /// Release even a path held by another session.
        #[arg(long)]
        force: bool,
    },
    /// Show held locks for this project (or every project with --all).
    Status {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Drop expired/dead locks.
    Prune,
    /// Print a shell-completion script (bash, zsh, fish, …) to stdout.
    Completions { shell: Shell },
}

fn print_conflicts(conflicts: &[Conflict]) {
    eprintln!("conflict: {} path(s) held by another session:", conflicts.len());
    for c in conflicts {
        let note = c.note.as_deref().map(|n| format!(" — {n}")).unwrap_or_default();
        eprintln!("  {} held by {} ({}s ago){}", c.path, c.held_by, c.age_secs, note);
    }
}

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("lock");
    devkit_common::paths::migrate_legacy_state();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Acquire { paths, holder, note, ttl, json } => {
            let out = devkit_locks::acquire(&paths, holder.as_deref(), note.as_deref(), ttl)?;
            if json {
                let ok = out.conflicts.is_empty();
                let payload = serde_json::json!({ "ok": ok, "acquired": out.acquired, "conflicts": out.conflicts });
                println!("{}", serde_json::to_string(&payload)?);
            } else if out.conflicts.is_empty() {
                for a in &out.acquired {
                    println!("locked {} (ttl {}s)", a.path, a.ttl_secs);
                }
            } else {
                print_conflicts(&out.conflicts);
            }
            if !out.conflicts.is_empty() {
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::Check { paths, holder, json } => {
            let conflicts = devkit_locks::check(&paths, holder.as_deref())?;
            if json {
                let payload = serde_json::json!({ "ok": conflicts.is_empty(), "conflicts": conflicts });
                println!("{}", serde_json::to_string(&payload)?);
            } else if conflicts.is_empty() {
                println!("available");
            } else {
                print_conflicts(&conflicts);
            }
            if !conflicts.is_empty() {
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::Release { paths, holder, all, force } => {
            if all {
                let freed = devkit_locks::release_all(holder.as_deref())?;
                println!("released {} lock(s)", freed.len());
            } else {
                let (released, refused) = devkit_locks::release(&paths, holder.as_deref(), force)?;
                println!("released {} lock(s)", released.len());
                if !refused.is_empty() {
                    eprintln!("refused (held by another session; use --force): {}", refused.join(", "));
                    std::process::exit(1);
                }
            }
            Ok(())
        }
        Cmd::Status { all, json } => {
            let locks = devkit_locks::status(all)?;
            if json {
                println!("{}", serde_json::to_string(&status_json(&locks))?);
            } else {
                print!("{}", status_table(&locks, all));
            }
            Ok(())
        }
        Cmd::Prune => {
            let n = devkit_locks::prune()?;
            println!("pruned {n} lock(s)");
            Ok(())
        }
        Cmd::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "lock", &mut std::io::stdout());
            Ok(())
        }
    }
}

fn status_json(locks: &[LockEntry]) -> serde_json::Value {
    serde_json::json!({
        "locks": locks.iter().map(|e| serde_json::json!({
            "path": e.path, "root": e.root, "holder": e.holder,
            "pid": e.pid, "note": e.note, "ts": e.ts, "ttl": e.ttl,
        })).collect::<Vec<_>>()
    })
}

fn status_table(locks: &[LockEntry], all: bool) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let headers: Vec<&str> = if all {
        vec!["ROOT", "PATH", "HOLDER", "AGE", "TTL-LEFT", "PID", "NOTE"]
    } else {
        vec!["PATH", "HOLDER", "AGE", "TTL-LEFT", "PID", "NOTE"]
    };
    let mut t = devkit_common::ui::table(&headers);
    for e in locks {
        let age = format!("{}s", now.saturating_sub(e.ts));
        let ttl_left = if e.ttl == 0 {
            "∞".to_string()
        } else {
            format!("{}s", e.ttl.saturating_sub(now.saturating_sub(e.ts)))
        };
        let pid = e.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
        let note = e.note.clone().unwrap_or_default();
        let mut row = Vec::new();
        if all {
            row.push(devkit_common::paths::leaf(&e.root).unwrap_or(&e.root).to_string());
        }
        row.extend([e.path.clone(), e.holder.clone(), age, ttl_left, pid, note]);
        t.add_row(row);
    }
    format!("{t}\n")
}
```

- [ ] **Step 3: Add startup migration to the other user-facing binaries**

So the port registry actually moves to the new home when any tool runs, add one line after each `install_panic_hook(...)` call:

In `src/bin/portman.rs`, after `devkit_common::report::install_panic_hook("portman");`:
```rust
    devkit_common::paths::migrate_legacy_state();
```
In `src/bin/devrun/main.rs`, after `devkit_common::report::install_panic_hook("devrun");`:
```rust
    devkit_common::paths::migrate_legacy_state();
```
In `src/bin/issue/main.rs`, after `devkit_common::report::install_panic_hook("issue");`:
```rust
    devkit_common::paths::migrate_legacy_state();
```

(If a binary's `main` doesn't already call `install_panic_hook`, add the migration line as the first statement in `main` instead.)

- [ ] **Step 4: Verify it builds and runs**

Run: `cargo build`
Expected: compiles clean; `lock` binary produced.

Run: `HOME=$(mktemp -d) XDG_STATE_HOME=$(mktemp -d) ./target/debug/lock status`
Expected: prints an empty table header (no panics). (Overriding `HOME` keeps the
startup migration from touching your real `~/.claude/state/devkit`.)

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml src/bin/lock.rs src/bin/portman.rs src/bin/devrun/main.rs src/bin/issue/main.rs
git commit -m "feat(locks): lock CLI binary and startup state migration"
```

---

## Task 9: Integration tests

**Files:**
- Create: `tests/locks.rs`
- Modify: `tests/completions.rs`

- [ ] **Step 1: Write the integration tests**

Create `tests/locks.rs`:

```rust
//! End-to-end coverage of the `lock` binary: conflict detection, JSON output,
//! release, and exit codes. Each test is isolated via a private temp project
//! (with a `.git` marker) and a private `XDG_STATE_HOME`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("devkit-lock-it-{}-{}-{}", std::process::id(), tag, n));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn project() -> PathBuf {
    let p = scratch("proj");
    std::fs::create_dir_all(p.join(".git")).unwrap();
    p
}

fn run(project: &Path, state: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lock"))
        .args(args)
        .current_dir(project)
        .env("XDG_STATE_HOME", state)
        // Override HOME too: the binary runs migrate_legacy_state() at startup, which
        // reads $HOME/.claude/state/devkit. Pointing HOME at the throwaway temp dir
        // keeps the test from ever touching the developer's real state home.
        .env("HOME", state)
        .env_remove("DEVKIT_SESSION")
        .env_remove("TMUX_PANE")
        .output()
        .expect("spawn lock")
}

#[test]
fn second_holder_conflicts_with_overlap() {
    let proj = project();
    let state = scratch("state");
    let a = run(&proj, &state, &["acquire", "scenes", "--as", "alice"]);
    assert!(a.status.success(), "alice should acquire");

    let b = run(&proj, &state, &["acquire", "scenes/player.tscn", "--as", "bob"]);
    assert_eq!(b.status.code(), Some(1), "bob conflicts on an overlapping path");
    let text = String::from_utf8_lossy(&b.stderr);
    assert!(text.contains("alice"), "conflict names the holder: {text}");
}

#[test]
fn json_conflict_shape() {
    let proj = project();
    let state = scratch("state");
    run(&proj, &state, &["acquire", "scenes", "--as", "alice"]);

    let b = run(&proj, &state, &["check", "scenes/x", "--as", "bob", "--json"]);
    assert_eq!(b.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&b.stdout).expect("json on stdout");
    assert_eq!(v["ok"], serde_json::json!(false));
    assert_eq!(v["conflicts"][0]["held_by"], serde_json::json!("alice"));
}

#[test]
fn release_frees_for_other_holder() {
    let proj = project();
    let state = scratch("state");
    run(&proj, &state, &["acquire", "scenes", "--as", "alice"]);
    let r = run(&proj, &state, &["release", "scenes", "--as", "alice"]);
    assert!(r.status.success());

    let b = run(&proj, &state, &["acquire", "scenes", "--as", "bob"]);
    assert!(b.status.success(), "bob can acquire after alice releases");
}

#[test]
fn same_holder_reacquire_is_ok() {
    let proj = project();
    let state = scratch("state");
    assert!(run(&proj, &state, &["acquire", "scenes", "--as", "alice"]).status.success());
    assert!(run(&proj, &state, &["acquire", "scenes", "--as", "alice"]).status.success());
}
```

- [ ] **Step 2: Run them to verify they pass**

Run: `cargo test --test locks`
Expected: PASS (4 tests).

- [ ] **Step 3: Add a `lock` case to the completions test**

In `tests/completions.rs`, add after `issue_emits_completions`:

```rust
#[test]
fn lock_emits_completions() {
    completions_contain_name("lock", env!("CARGO_BIN_EXE_lock"));
}
```

- [ ] **Step 4: Run the completions test**

Run: `cargo test --test completions`
Expected: PASS (4 tests, including `lock_emits_completions`).

- [ ] **Step 5: Commit**

```bash
git add tests/locks.rs tests/completions.rs
git commit -m "test(locks): end-to-end conflict/release/json and completions coverage"
```

---

## Task 10: Documentation & final verification

**Files:**
- Modify: `docs/next-steps.md`
- Modify: `README.md`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add follow-ups to `docs/next-steps.md`**

Append to `docs/next-steps.md`:

```markdown
## Route `lock` through the supervisor daemon

`lock` acquire/release/status/check go straight to the flock'd `locks.json` today.
The port registry already has an optional `devkit-portd` fast path
(`devkit-ports::registry::via_daemon`); add the equivalent for the lock registry so
high-frequency lock checks avoid the per-call file-lock + read. The daemon proto and
client live in `crates/devkit-ports/src/daemon/`.

## Unify the two flock'd-JSON stores

`devkit-ports::registry` (private `read`/`write`/`salvage`/`with_lock`) and
`devkit-locks::store` are the same machine over different schemas. Extract a generic
`devkit-common` locked-JSON store (a `with_lock<T>` parameterized by lock path, data
path, and a `Default + Serialize + Deserialize` payload) and have both adopt it.
```

- [ ] **Step 2: Add a "File locks" section to the README and update counts**

In `README.md`, change the intro line:
```
A Rust workspace of four binaries that coordinate local development for a monorepo.
```
to:
```
A Rust workspace of five binaries that coordinate local development for a monorepo.
```

After the `### issue — Issue Lifecycle` block (before `## Configuration`), add:

```markdown
### `lock` — File Locks

Advisory locks on paths so parallel sessions sharing one checkout (where per-session
worktrees are too expensive) don't edit the same files at once. A flock-guarded
registry of claims keyed by path — the file-level twin of `portman`. Locks are
exclusive and overlap by path component, so locking a directory conflicts with
locking a file inside it.

```
lock acquire <paths…> [--as <id>] [--note <msg>] [--ttl <secs>] [--json]
lock release <paths…> [--as <id>]        # or: release --all
lock check   <paths…> [--json]           # read-only: would acquire succeed?
lock status  [--all] [--json]
lock prune
```

Sessions identify themselves by (in priority order) `--as <id>`, `$DEVKIT_SESSION`,
`$TMUX_PANE` (zero-config and unique per tmux pane), the controlling tty, or the
parent pid. Conflicts fail fast: `acquire`/`check` exit `1` and report who holds the
path. Locks expire after their TTL (default 30 min, `--ttl 0` disables) or when a
recorded anchor pid dies; `release` frees them explicitly. For non-interactive agent
sessions, pass a stable `--as`/`$DEVKIT_SESSION` so acquire and release agree.
```

In the `## Install` section, change `four binaries (\`portman\`, \`devrun\`, \`issue\`, \`devkit-portd\`)` to `five binaries (\`portman\`, \`devrun\`, \`issue\`, \`lock\`, \`devkit-portd\`)`.

In the `## State & Cache Locations` table, change the registry/logs rows' paths from `~/.claude/state/devkit/...` to `~/.local/state/devkit/...` and add a row:
```
| File-lock registry | `~/.local/state/devkit/locks.json` |
```
Append a note under the table:
```markdown
The state home honors `$XDG_STATE_HOME` (default `~/.local/state`). A legacy
`~/.claude/state/devkit` home is migrated automatically on first run.
```

In the `## Shell completions` section, add a `lock` example line:
```sh
lock completions zsh    > ~/.zfunc/_lock
```

- [ ] **Step 3: Update `CLAUDE.md`**

In `CLAUDE.md`, update the binary count and layout to include `lock`:
- Change "a root `devkit` binary package whose four CLIs live in `src/bin/`" to "...whose five CLIs live in `src/bin/`".
- In the layout table, add a row for `src/bin/lock.rs` ("advisory file-lock CLI") and `crates/devkit-locks` ("file-lock registry: model + flock'd JSON store").
- Update the test-count reference (search for the prior count, e.g. "92 tests") to the new total reported by `cargo test`.

Add a short agent-guidance block (near the existing tool guidance) so sessions use locks instead of markdown files:
```markdown
## File locks

When multiple sessions share one checkout, claim files before editing them with the
`lock` binary instead of writing ad-hoc `.lock` files:

- `lock acquire <paths…> --as <stable-session-id>` before editing; it exits `1` with
  the current holder if any path is taken — branch on that.
- `lock release <paths…> --as <same-id>` (or `lock release --all --as <id>`) when done.
- Always pass a consistent `--as <id>` (or set `$DEVKIT_SESSION`) so acquire and
  release refer to the same holder.
```

- [ ] **Step 4: Full verification**

Run: `cargo test`
Expected: PASS — all crates + integration suites green.

Run: `cargo clippy --all-targets -- -D warnings`
Expected: no warnings.

Run: `cargo build --no-default-features`
Expected: compiles; `lock` is built (it has no `daemon` gate) while `devkit-portd` is skipped.

- [ ] **Step 5: Commit**

```bash
git add docs/next-steps.md README.md CLAUDE.md
git commit -m "docs(locks): document the lock binary, state-home move, and daemon follow-ups"
```

---

## Final notes for the implementer

- Keep the panic-hook + `migrate_legacy_state()` as the first two lines of every binary `main`.
- Do **not** call `migrate_legacy_state()` from library code or `state_dir()` — migration is a startup-only action so `cargo test` never moves real data.
- The `--as` clap field is named `holder` (`as` is a reserved word); the identity functions take it as `as_flag`.
- All conflict exits use code `1`; clap emits `2` for usage errors automatically.
```
