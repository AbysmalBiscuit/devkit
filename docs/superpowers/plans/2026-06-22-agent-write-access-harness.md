# Agent write-access harness — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make devkit's advisory file-lock registry *enforced* for coding agents, via a blocking Claude Code PreToolUse hook that lets an agent write a file only when its session owns the lock (auto-acquiring when the file is free), releasing on SubagentStop / SessionEnd.

**Architecture:** New registry-level primitives in `devkit-locks` (an ancestor-or-self ownership decision and a prefix release), exposed through the existing flock/daemon `Store` seam and the `devkitd` lock socket. A new `lockm hook <event>` subcommand parses the Claude Code hook payload, derives a two-level holder id (`session_id` / `session_id/agent_id`), checks per-checkout opt-in, and emits the allow/deny envelope. A plugin `hooks/hooks.json` wires the three events.

**Tech Stack:** Rust (edition 2024), `anyhow`, `serde`/`serde_json`, `toml`, `fd-lock`, `clap`. Tests via `cargo test --workspace`. Claude Code plugin hooks (`hooks/hooks.json`, `${CLAUDE_PLUGIN_ROOT}`).

## Global Constraints

- **Spec:** `docs/superpowers/specs/2026-06-21-agent-write-access-harness-design.md`.
- **Writes only.** Gate `Edit`, `MultiEdit`, `Write`, `NotebookEdit`. Reads, and `Bash` writes, are out of scope (documented gap).
- **Opt-in per checkout, default off.** Harness engages only when the checkout's root `devkit.toml` has `[harness] enforce_writes = true`. No marker → the hook exits 0 immediately.
- **Ownership rule:** a write to file `F` by holder `H` is allowed iff every live overlapping lock on `F` is held by an ancestor-or-self of `H`; free → auto-acquire as `H`; otherwise deny.
- **Holder id is two-level:** top-level agent = `session_id`; sub-agent = `session_id/agent_id`. Ancestor test is leading path-segment prefix.
- **Fail open** when the harness is off or `lockm` is absent; **fail closed** (deny) on a registry error when the harness is on.
- **TDD**, frequent commits, Conventional Commits. `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all` must pass before the final commit. Tests that spawn/reap poll for state — never a fixed sleep.
- **`anyhow` everywhere**, `.context()` on fallible IO.
- Go through the `registry`/`store` facade; keep liveness syscalls out of the exclusive lock (the new ops use only TTL/`kill(0)` checks, which are allowed inside `commit`).

### Deviations from the spec (discovered during planning — fold back into the spec)

1. **Holder identity is a fixed two-level id, not an arbitrary-depth tree.** Claude Code's hook payload exposes `session_id` (shared by parent and sub-agents) and `agent_id` (sub-agents only) — no parent-chain pointer. The prefix-based ancestor rule is unchanged; only the achievable depth is capped at two.
2. **No `$DEVKIT_SESSION` interop for manual `lockm`.** A PreToolUse subprocess cannot set the parent session's environment, so manual `lockm acquire` calls cannot be made to share the hook's holder id. In an enforced checkout the hook is the *sole* acquirer; manual `lockm` is neither required nor expected there (it remains for non-enforced/advisory use). SKILL.md is updated to say so.

---

## File Structure

| File | Responsibility | Create/Modify |
|---|---|---|
| `crates/devkit-locks/src/model.rs` | `is_ancestor_or_self`, `WriteDecision`, `Data::{write_blockers, decide_write, release_prefix}` | Modify |
| `crates/devkit-locks/src/store.rs` | `write_decide_with`, `release_prefix_with` over the `Store` seam | Modify |
| `crates/devkit-locks/src/lib.rs` | facade `decide_write`, `release_prefix`, `write_ctx` (daemon split) | Modify |
| `crates/devkit-locks/src/daemon/proto.rs` | `Request::{WriteDecide, ReleasePrefix}`, `Response::WriteDecided` | Modify |
| `src/bin/devkitd/lock_server.rs` | dispatch arms for the two new requests | Modify |
| `crates/devkit-locks/src/hook.rs` | holder derivation, payload parse, deny envelope, `harness_enabled` | Create |
| `crates/devkit-locks/src/lib.rs` | `pub mod hook;` | Modify |
| `crates/devkit-locks/Cargo.toml` | add `toml` dep | Modify |
| `src/bin/lockm.rs` | `Cmd::Hook { event }` subcommand driver | Modify |
| `tests/lock_daemon.rs` | daemon round-trip for `WriteDecide` / `ReleasePrefix` | Modify |
| `tests/lock_harness_race.rs` | multiprocess concurrent-write race | Create |
| `hooks/hooks.json` | Claude Code plugin hook manifest (3 events) | Create |
| `devkit.toml` | repo dogfood: `[harness] enforce_writes = true` | Create |
| `skills/using-devkit/SKILL.md` | document enforced mode + opt-in | Modify |
| `docs/configuration.md` | document `[harness] enforce_writes` | Modify |

---

## Task 1: `is_ancestor_or_self` holder-ancestry helper

**Files:**
- Modify: `crates/devkit-locks/src/model.rs`
- Test: same file (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `pub fn is_ancestor_or_self(existing: &str, writer: &str) -> bool`

- [ ] **Step 1: Write the failing test** — add to `model.rs` `mod tests`:

```rust
#[test]
fn ancestry_self_and_prefix() {
    assert!(is_ancestor_or_self("S", "S"));            // self
    assert!(is_ancestor_or_self("S", "S/a1"));         // parent of sub-agent
    assert!(is_ancestor_or_self("S/a1", "S/a1/b2"));   // grandparent (if ever nested)
    assert!(!is_ancestor_or_self("S/a1", "S"));        // child does not own parent
    assert!(!is_ancestor_or_self("S/a1", "S/b2"));     // siblings contend
    assert!(!is_ancestor_or_self("S", "Sx"));          // not a segment boundary
    assert!(!is_ancestor_or_self("S", "T"));           // unrelated sessions
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-locks ancestry_self_and_prefix`
Expected: FAIL — `cannot find function is_ancestor_or_self`.

- [ ] **Step 3: Write minimal implementation** — add near `paths_overlap` in `model.rs`:

```rust
/// True if `existing` is the same holder as `writer`, or an ancestor of it in the
/// agent tree. Holder ids are '/'-separated segments (`session`, `session/agent`);
/// `existing` is an ancestor of `writer` when it is a leading segment-boundary prefix.
pub fn is_ancestor_or_self(existing: &str, writer: &str) -> bool {
    existing == writer
        || writer
            .strip_prefix(existing)
            .is_some_and(|rest| rest.starts_with('/'))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p devkit-locks ancestry_self_and_prefix`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-locks/src/model.rs
git commit -m "feat(locks): add holder ancestor-or-self predicate"
```

---

## Task 2: `WriteDecision` + decide/blockers/release-prefix on `Data`

**Files:**
- Modify: `crates/devkit-locks/src/model.rs`
- Test: same file

**Interfaces:**
- Consumes: `is_ancestor_or_self`, `paths_overlap`, `entry_dead`, `key_for`, `LockEntry`, `Conflict` (Task 1 + existing).
- Produces:
  - `pub enum WriteDecision { Acquired, AllowedByOwnership, Denied(Vec<Conflict>) }` (derives `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`)
  - `pub fn Data::write_blockers(&self, root: &str, path: &str, writer: &str, now: u64) -> Vec<Conflict>`
  - `pub fn Data::decide_write(&mut self, root: &str, path: &str, writer: &str, pid: Option<u32>, note: Option<&str>, ttl: u64, now: u64) -> WriteDecision`
  - `pub fn Data::release_prefix(&mut self, root: &str, prefix: &str) -> Vec<String>`

- [ ] **Step 1: Write the failing tests** — add to `model.rs` `mod tests`:

```rust
#[test]
fn decide_write_free_acquires() {
    let mut d = Data::default();
    let r = d.decide_write("/repo", "src/a.rs", "S", None, Some("write-harness"), 1800, 100);
    assert_eq!(r, WriteDecision::Acquired);
    assert_eq!(d.locks[&key_for("/repo", "src/a.rs")].holder, "S");
}

#[test]
fn decide_write_self_is_allowed_and_renews() {
    let mut d = Data::default();
    d.decide_write("/repo", "src/a.rs", "S", None, None, 1800, 100);
    let r = d.decide_write("/repo", "src/a.rs", "S", None, None, 1800, 250);
    assert_eq!(r, WriteDecision::AllowedByOwnership);
    assert_eq!(d.locks.len(), 1);
    assert_eq!(d.locks[&key_for("/repo", "src/a.rs")].ts, 250); // renewed
}

#[test]
fn decide_write_ancestor_allowed_without_clobber() {
    let mut d = Data::default();
    // parent S holds the directory; child S/a1 writes a file under it
    d.decide_write("/repo", "src", "S", None, None, 1800, 100);
    let r = d.decide_write("/repo", "src/a.rs", "S/a1", None, None, 1800, 120);
    assert_eq!(r, WriteDecision::AllowedByOwnership);
    // the parent's lock is untouched; no new row inserted for the child
    assert_eq!(d.locks.len(), 1);
    assert_eq!(d.locks[&key_for("/repo", "src")].holder, "S");
}

#[test]
fn decide_write_sibling_denied() {
    let mut d = Data::default();
    d.decide_write("/repo", "src/a.rs", "S/a1", None, None, 1800, 100);
    let r = d.decide_write("/repo", "src/a.rs", "S/b2", None, None, 1800, 140);
    match r {
        WriteDecision::Denied(c) => {
            assert_eq!(c.len(), 1);
            assert_eq!(c[0].held_by, "S/a1");
            assert_eq!(c[0].age_secs, 40);
        }
        other => panic!("expected Denied, got {other:?}"),
    }
    assert_eq!(d.locks.len(), 1); // nothing acquired for the loser
}

#[test]
fn decide_write_other_session_denied() {
    let mut d = Data::default();
    d.decide_write("/repo", "src/a.rs", "S", None, None, 1800, 100);
    let r = d.decide_write("/repo", "src/a.rs", "T", None, None, 1800, 110);
    assert!(matches!(r, WriteDecision::Denied(_)));
}

#[test]
fn release_prefix_frees_session_subtree_only() {
    let mut d = Data::default();
    d.decide_write("/repo", "a", "S", None, None, 1800, 1);
    d.decide_write("/repo", "b", "S/a1", None, None, 1800, 1);
    d.decide_write("/repo", "c", "T", None, None, 1800, 1);
    let freed = d.release_prefix("/repo", "S");
    assert_eq!(freed.len(), 2);                 // S and S/a1
    assert!(d.locks.contains_key(&key_for("/repo", "c"))); // T survives
}

#[test]
fn release_prefix_single_subagent() {
    let mut d = Data::default();
    d.decide_write("/repo", "a", "S", None, None, 1800, 1);
    d.decide_write("/repo", "b", "S/a1", None, None, 1800, 1);
    let freed = d.release_prefix("/repo", "S/a1");
    assert_eq!(freed, vec!["b".to_string()]);   // only the sub-agent's lock
    assert!(d.locks.contains_key(&key_for("/repo", "a")));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-locks decide_write release_prefix`
Expected: FAIL — `WriteDecision` / methods not defined.

- [ ] **Step 3: Write minimal implementation** — add to `model.rs`. The enum near the other result structs:

```rust
/// Outcome of evaluating a single write against the registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WriteDecision {
    /// File was free; a fresh lock was taken for the writer.
    Acquired,
    /// Writer already owns the file (self or an ancestor holds an overlapping lock).
    AllowedByOwnership,
    /// Blocked: live overlapping locks held by non-ancestors.
    Denied(Vec<Conflict>),
}
```

Add these methods inside `impl Data { … }`:

```rust
/// Live overlapping locks for `path` in `root` whose holder is NOT an
/// ancestor-or-self of `writer` — the locks that block the write.
pub fn write_blockers(&self, root: &str, path: &str, writer: &str, now: u64) -> Vec<Conflict> {
    let mut out = Vec::new();
    for e in self.locks.values() {
        if e.root == root
            && !entry_dead(e, now)
            && paths_overlap(&e.path, path)
            && !is_ancestor_or_self(&e.holder, writer)
        {
            out.push(Conflict {
                path: path.to_string(),
                held_by: e.holder.clone(),
                age_secs: now.saturating_sub(e.ts),
                note: e.note.clone(),
            });
        }
    }
    out
}

/// Decide a write and, only when the file is free, take a lock for `writer`.
/// Mutates self solely in the `Acquired` case (insert) or to renew the writer's
/// own exact-path lock; an ancestor's lock is never overwritten.
#[allow(clippy::too_many_arguments)]
pub fn decide_write(
    &mut self,
    root: &str,
    path: &str,
    writer: &str,
    pid: Option<u32>,
    note: Option<&str>,
    ttl: u64,
    now: u64,
) -> WriteDecision {
    let blockers = self.write_blockers(root, path, writer, now);
    if !blockers.is_empty() {
        return WriteDecision::Denied(blockers);
    }
    let overlaps = self.locks.values().any(|e| {
        e.root == root && !entry_dead(e, now) && paths_overlap(&e.path, path)
    });
    if overlaps {
        // Held only by self or an ancestor. Renew the writer's own exact lock if present.
        if let Some(e) = self.locks.get_mut(&key_for(root, path)) {
            if e.holder == writer {
                e.ts = now;
            }
        }
        return WriteDecision::AllowedByOwnership;
    }
    self.locks.insert(
        key_for(root, path),
        LockEntry {
            path: path.to_string(),
            root: root.to_string(),
            holder: writer.to_string(),
            pid,
            note: note.map(str::to_string),
            ts: now,
            ttl,
        },
    );
    WriteDecision::Acquired
}

/// Release every lock in `root` whose holder is `prefix` or a descendant
/// (`prefix/…`). Used by SubagentStop (`session/agent`) and SessionEnd (`session`).
pub fn release_prefix(&mut self, root: &str, prefix: &str) -> Vec<String> {
    let freed: Vec<String> = self
        .locks
        .values()
        .filter(|e| e.root == root && is_ancestor_or_self(prefix, &e.holder))
        .map(|e| e.path.clone())
        .collect();
    for p in &freed {
        self.locks.remove(&key_for(root, p));
    }
    freed
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-locks decide_write release_prefix write_blockers`
Expected: PASS (all 7).

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-locks/src/model.rs
git commit -m "feat(locks): add ancestor-aware write decision and prefix release"
```

---

## Task 3: `Store` seam — `write_decide_with` + `release_prefix_with`

**Files:**
- Modify: `crates/devkit-locks/src/store.rs`
- Test: same file (`mod seam_tests`)

**Interfaces:**
- Consumes: `Store`, `Data::{decide_write, release_prefix, prune_dead}`, `WriteDecision` (Task 2).
- Produces:
  - `pub fn write_decide_with(s: &impl Store, root: &str, holder: &str, path: &str, pid: Option<u32>, note: Option<&str>, ttl: u64, now: u64) -> Result<WriteDecision>`
  - `pub fn release_prefix_with(s: &impl Store, root: &str, prefix: &str) -> Result<Vec<String>>`

- [ ] **Step 1: Write the failing tests** — add to `store.rs` `mod seam_tests` (uses the existing `tmp` + `FlockStore::at`). Also add `WriteDecision` to the `use crate::model::…` line at the top of `store.rs`:

```rust
#[test]
fn write_decide_acquires_then_blocks_other_holder() {
    let dir = tmp("wd");
    let s = FlockStore::at(&dir);
    let first = write_decide_with(&s, "/repo", "S", "src/a.rs", None, Some("write-harness"), 1800, 100).unwrap();
    assert_eq!(first, crate::model::WriteDecision::Acquired);
    let blocked = write_decide_with(&s, "/repo", "T", "src/a.rs", None, None, 1800, 120).unwrap();
    assert!(matches!(blocked, crate::model::WriteDecision::Denied(_)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn write_decide_ancestor_allows() {
    let dir = tmp("wda");
    let s = FlockStore::at(&dir);
    write_decide_with(&s, "/repo", "S", "src", None, None, 1800, 100).unwrap();
    let child = write_decide_with(&s, "/repo", "S/a1", "src/a.rs", None, None, 1800, 120).unwrap();
    assert_eq!(child, crate::model::WriteDecision::AllowedByOwnership);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn release_prefix_with_frees_subtree() {
    let dir = tmp("rp");
    let s = FlockStore::at(&dir);
    write_decide_with(&s, "/repo", "S", "a", None, None, 1800, 1).unwrap();
    write_decide_with(&s, "/repo", "S/a1", "b", None, None, 1800, 1).unwrap();
    let freed = release_prefix_with(&s, "/repo", "S").unwrap();
    assert_eq!(freed.len(), 2);
    assert!(s.snapshot().unwrap().locks.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-locks --lib write_decide release_prefix_with`
Expected: FAIL — functions not defined.

- [ ] **Step 3: Write minimal implementation** — add to `store.rs` (next to `acquire_with`):

```rust
/// Hook write path: prune dead, then decide (and acquire when free) atomically.
#[allow(clippy::too_many_arguments)]
pub fn write_decide_with(
    s: &impl Store,
    root: &str,
    holder: &str,
    path: &str,
    pid: Option<u32>,
    note: Option<&str>,
    ttl: u64,
    now: u64,
) -> Result<crate::model::WriteDecision> {
    s.commit(|d| {
        d.prune_dead(now);
        Ok(d.decide_write(root, path, holder, pid, note, ttl, now))
    })
}

/// Hook release path: free every lock held by `prefix` or its descendants in `root`.
pub fn release_prefix_with(s: &impl Store, root: &str, prefix: &str) -> Result<Vec<String>> {
    s.commit(|d| Ok(d.release_prefix(root, prefix)))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-locks --lib write_decide release_prefix_with`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-locks/src/store.rs
git commit -m "feat(locks): add write-decide and prefix-release store ops"
```

---

## Task 4: Facade — `decide_write` + `release_prefix` (flock path)

**Files:**
- Modify: `crates/devkit-locks/src/lib.rs`
- Test: same file (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `store::{write_decide_with, release_prefix_with}`, `find_root_from`, `normalize_under_root`, `ident::anchor_pid`, `now` (existing + Task 3).
- Produces:
  - `pub fn decide_write(path_in: &str, holder: &str, note: Option<&str>, ttl: u64) -> Result<WriteDecision>`
  - `pub fn release_prefix(holder_prefix: &str) -> Result<Vec<String>>`
  - re-export: `pub use model::WriteDecision;` is NOT added — callers use `devkit_locks::model::WriteDecision`.

Note: `decide_write` derives the project root from the **file's own absolute path** (the hook payload's `file_path`), independent of process cwd. `release_prefix` uses the process cwd's repo root (the hook runs with cwd = the session's checkout).

- [ ] **Step 1: Write the failing test** — add to `lib.rs` `mod tests`:

```rust
#[test]
fn write_ctx_derives_root_and_relpath() {
    let root = scratch("wctx");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    let file = root.join("src/a.rs");
    let (r, rel) = write_ctx(file.to_str().unwrap()).unwrap();
    assert_eq!(PathBuf::from(&r), root);
    assert_eq!(rel, "src/a.rs");
    let _ = std::fs::remove_dir_all(&root);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-locks write_ctx_derives_root_and_relpath`
Expected: FAIL — `cannot find function write_ctx`.

- [ ] **Step 3: Write minimal implementation** — add to `lib.rs`:

```rust
/// Resolve a write target (absolute, or cwd-relative) to (project_root, root-relative
/// path). The root is the nearest `.git` ancestor of the file's directory, so the
/// decision does not depend on where the hook process was spawned.
fn write_ctx(path_in: &str) -> Result<(String, String)> {
    let p = Path::new(path_in);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().context("getting current dir")?.join(p)
    };
    let start = abs.parent().unwrap_or(abs.as_path());
    let root = find_root_from(start);
    let rel = normalize_under_root(&abs, &root)?;
    Ok((root.to_string_lossy().into_owned(), rel))
}

/// Enforced-write decision for `path_in` by an explicit `holder` (the hook derives
/// the holder from the agent payload; identity is not resolved here). Free → acquire;
/// self/ancestor → allow; otherwise deny.
pub fn decide_write(
    path_in: &str,
    holder: &str,
    note: Option<&str>,
    ttl: u64,
) -> Result<model::WriteDecision> {
    let (root, path) = write_ctx(path_in)?;
    let pid = ident::anchor_pid();
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(daemon::proto::Request::WriteDecide {
        root: root.clone(),
        holder: holder.to_string(),
        path: path.clone(),
        pid,
        note: note.map(str::to_string),
        ttl,
    })? {
        return match resp {
            daemon::proto::Response::WriteDecided(d) => Ok(d),
            daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    store::write_decide_with(
        &store::FlockStore::new(),
        &root,
        holder,
        &path,
        pid,
        note,
        ttl,
        now(),
    )
}

/// Release every lock held by `holder_prefix` or its descendants in the cwd's repo.
pub fn release_prefix(holder_prefix: &str) -> Result<Vec<String>> {
    let root = find_root()?.to_string_lossy().into_owned();
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(daemon::proto::Request::ReleasePrefix {
        root: root.clone(),
        prefix: holder_prefix.to_string(),
    })? {
        return match resp {
            daemon::proto::Response::Freed(v) => Ok(v),
            daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    store::release_prefix_with(&store::FlockStore::new(), &root, holder_prefix)
}
```

Note: the two `daemon::proto::Request`/`Response` variants referenced here are added in Task 5; this task compiles only with the `daemon` feature off (default `cargo test` does not enable it for `-p devkit-locks` unless the workspace default includes it). If the workspace builds `devkit-locks` with `daemon` on by default, do Task 5 first or land Tasks 4–5 in one commit. **Sequencing note:** implement Task 5's proto additions before running the full `--workspace` build.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p devkit-locks write_ctx_derives_root_and_relpath`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-locks/src/lib.rs
git commit -m "feat(locks): add decide_write and release_prefix facade"
```

---

## Task 5: Daemon proto + dispatch for the new ops

**Files:**
- Modify: `crates/devkit-locks/src/daemon/proto.rs`
- Modify: `src/bin/devkitd/lock_server.rs`
- Test: `tests/lock_daemon.rs`

**Interfaces:**
- Consumes: `store::{write_decide_with, release_prefix_with}` (Task 3), `WriteDecision`.
- Produces:
  - `Request::WriteDecide { root, holder, path: String, pid: Option<u32>, note: Option<String>, ttl: u64 }`
  - `Request::ReleasePrefix { root: String, prefix: String }`
  - `Response::WriteDecided(WriteDecision)`

- [ ] **Step 1: Write the failing test** — add to `tests/lock_daemon.rs`:

```rust
/// A write decision through the daemon acquires a free file, then denies a
/// non-ancestor holder; a prefix release frees the subtree, written through to file.
#[test]
fn write_decide_and_release_prefix_through_daemon() {
    let mut h = Harness::start();
    h.wait_for_lock_socket(Duration::from_secs(5));

    let acq = h.lock_request(&Request::WriteDecide {
        root: "/repo".into(),
        holder: "S".into(),
        path: "src/a.rs".into(),
        pid: None,
        note: Some("write-harness".into()),
        ttl: 0,
    });
    assert!(matches!(acq, Response::WriteDecided(devkit_locks::model::WriteDecision::Acquired)),
        "expected Acquired, got {acq:?}");

    let denied = h.lock_request(&Request::WriteDecide {
        root: "/repo".into(),
        holder: "T".into(),
        path: "src/a.rs".into(),
        pid: None,
        note: None,
        ttl: 0,
    });
    assert!(matches!(denied, Response::WriteDecided(devkit_locks::model::WriteDecision::Denied(_))),
        "expected Denied, got {denied:?}");

    let freed = h.lock_request(&Request::ReleasePrefix { root: "/repo".into(), prefix: "S".into() });
    assert!(matches!(freed, Response::Freed(v) if v.len() == 1), "expected one freed, got {freed:?}");
    h.shutdown();
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit --test lock_daemon write_decide_and_release_prefix_through_daemon`
Expected: FAIL to compile — `WriteDecide` / `WriteDecided` not found.

- [ ] **Step 3a: Extend the proto** — in `crates/devkit-locks/src/daemon/proto.rs`, add to `enum Request`:

```rust
    WriteDecide {
        root: String,
        holder: String,
        path: String,
        pid: Option<u32>,
        note: Option<String>,
        ttl: u64,
    },
    ReleasePrefix {
        root: String,
        prefix: String,
    },
```

and to `enum Response`:

```rust
    WriteDecided(crate::model::WriteDecision),
```

- [ ] **Step 3b: Dispatch the new requests** — in `src/bin/devkitd/lock_server.rs`, add arms to the `match req` (before the closing brace):

```rust
        Request::WriteDecide {
            root,
            holder,
            path,
            pid,
            note,
            ttl,
        } => match store::write_decide_with(
            &s, &root, &holder, &path, pid, note.as_deref(), ttl, now(),
        ) {
            Ok(d) => Response::WriteDecided(d),
            Err(e) => Response::Err(format!("{e:#}")),
        },
        Request::ReleasePrefix { root, prefix } => {
            match store::release_prefix_with(&s, &root, &prefix) {
                Ok(v) => Response::Freed(v),
                Err(e) => Response::Err(format!("{e:#}")),
            }
        }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p devkit --test lock_daemon write_decide_and_release_prefix_through_daemon`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-locks/src/daemon/proto.rs src/bin/devkitd/lock_server.rs tests/lock_daemon.rs
git commit -m "feat(devkitd): serve write-decide and prefix-release over locks.sock"
```

---

## Task 6: Hook glue module — holder, payload parse, envelope, activation

**Files:**
- Create: `crates/devkit-locks/src/hook.rs`
- Modify: `crates/devkit-locks/src/lib.rs` (add `pub mod hook;`)
- Modify: `crates/devkit-locks/Cargo.toml` (add `toml`)
- Test: in `hook.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `serde_json`, `toml`, `find_root_from`, `model::WriteDecision`.
- Produces:
  - `pub fn holder_from_fields(session_id: &str, agent_id: Option<&str>) -> String`
  - `pub enum HookEvent { Write { tool_name: String, file_path: String, holder: String }, ReleaseSubagent { holder: String }, ReleaseSession { holder: String }, Ignore }`
  - `pub fn parse_event(event: &str, payload: &serde_json::Value) -> HookEvent`
  - `pub fn deny_json(reason: &str) -> serde_json::Value`
  - `pub fn harness_enabled(root: &std::path::Path) -> bool`

- [ ] **Step 1: Add the `toml` dependency** — in `crates/devkit-locks/Cargo.toml` under `[dependencies]`:

```toml
toml = { workspace = true }
```

Verify the workspace defines it: `rg -n '^toml' Cargo.toml` should show a `toml = …` line under `[workspace.dependencies]`. If absent, add `toml.workspace = true` mapping to the existing root `toml` version. (devkit-ports already uses `toml`, so the workspace dep exists.)

- [ ] **Step 2: Write the failing tests** — create `crates/devkit-locks/src/hook.rs` with only the tests first (module body added in Step 4):

```rust
//! Claude Code hook glue: holder derivation, payload parsing, decision envelope,
//! and per-checkout activation. Agent-specific shapes live here; the registry
//! decision logic stays in `model`/`store`.

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn holder_top_level_is_session() {
        assert_eq!(holder_from_fields("S", None), "S");
    }

    #[test]
    fn holder_subagent_is_session_slash_agent() {
        assert_eq!(holder_from_fields("S", Some("a1")), "S/a1");
    }

    #[test]
    fn parse_write_event_pulls_file_and_holder() {
        let p = json!({
            "session_id": "S",
            "tool_name": "Edit",
            "tool_input": { "file_path": "/repo/src/a.rs" }
        });
        match parse_event("pretooluse", &p) {
            HookEvent::Write { tool_name, file_path, holder } => {
                assert_eq!(tool_name, "Edit");
                assert_eq!(file_path, "/repo/src/a.rs");
                assert_eq!(holder, "S");
            }
            other => panic!("expected Write, got {other:?}"),
        }
    }

    #[test]
    fn parse_write_event_subagent_holder() {
        let p = json!({
            "session_id": "S", "agent_id": "a1",
            "tool_name": "Write", "tool_input": { "file_path": "/repo/x" }
        });
        match parse_event("pretooluse", &p) {
            HookEvent::Write { holder, .. } => assert_eq!(holder, "S/a1"),
            other => panic!("expected Write, got {other:?}"),
        }
    }

    #[test]
    fn parse_write_event_ignores_non_write_tool() {
        let p = json!({ "session_id": "S", "tool_name": "Bash", "tool_input": { "command": "ls" } });
        assert!(matches!(parse_event("pretooluse", &p), HookEvent::Ignore));
    }

    #[test]
    fn parse_write_event_ignores_missing_file_path() {
        let p = json!({ "session_id": "S", "tool_name": "Edit", "tool_input": {} });
        assert!(matches!(parse_event("pretooluse", &p), HookEvent::Ignore));
    }

    #[test]
    fn parse_subagent_stop_releases_subagent_holder() {
        let p = json!({ "session_id": "S", "agent_id": "a1" });
        match parse_event("subagent-stop", &p) {
            HookEvent::ReleaseSubagent { holder } => assert_eq!(holder, "S/a1"),
            other => panic!("expected ReleaseSubagent, got {other:?}"),
        }
    }

    #[test]
    fn parse_session_end_releases_session_prefix() {
        let p = json!({ "session_id": "S" });
        match parse_event("session-end", &p) {
            HookEvent::ReleaseSession { holder } => assert_eq!(holder, "S"),
            other => panic!("expected ReleaseSession, got {other:?}"),
        }
    }

    #[test]
    fn deny_json_has_pretooluse_envelope() {
        let v = deny_json("blocked by S/a1");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(v["hookSpecificOutput"]["permissionDecisionReason"], "blocked by S/a1");
    }

    #[test]
    fn harness_enabled_reads_flag() {
        let dir = std::env::temp_dir().join(format!("devkit-harness-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("devkit.toml"), "[harness]\nenforce_writes = true\n").unwrap();
        assert!(harness_enabled(&dir));
        std::fs::write(dir.join("devkit.toml"), "[harness]\nenforce_writes = false\n").unwrap();
        assert!(!harness_enabled(&dir));
        std::fs::write(dir.join("devkit.toml"), "[defaults]\nworktree_root = \"x\"\n").unwrap();
        assert!(!harness_enabled(&dir)); // missing section → off, despite unrelated keys
        let _ = std::fs::remove_file(dir.join("devkit.toml"));
        assert!(!harness_enabled(&dir)); // no devkit.toml → off
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p devkit-locks --lib hook::`
Expected: FAIL — items not defined.

- [ ] **Step 4: Write the module body** — prepend above the `mod tests` in `hook.rs`:

```rust
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

/// Tool names whose writes the harness governs.
const WRITE_TOOLS: [&str; 4] = ["Edit", "MultiEdit", "Write", "NotebookEdit"];

/// Two-level holder id: top-level agents are `session_id`; sub-agents are
/// `session_id/agent_id`. The Claude Code payload exposes no deeper ancestry.
pub fn holder_from_fields(session_id: &str, agent_id: Option<&str>) -> String {
    match agent_id {
        Some(a) if !a.is_empty() => format!("{session_id}/{a}"),
        _ => session_id.to_string(),
    }
}

#[derive(Debug)]
pub enum HookEvent {
    Write { tool_name: String, file_path: String, holder: String },
    ReleaseSubagent { holder: String },
    ReleaseSession { holder: String },
    Ignore,
}

fn str_field<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// Classify a hook payload. `event` is the subcommand arg
/// (`pretooluse` | `subagent-stop` | `session-end`).
pub fn parse_event(event: &str, p: &Value) -> HookEvent {
    let session = str_field(p, "session_id").unwrap_or("unknown");
    let agent = str_field(p, "agent_id");
    let holder = holder_from_fields(session, agent);
    match event {
        "pretooluse" => {
            let tool = str_field(p, "tool_name").unwrap_or("");
            if !WRITE_TOOLS.contains(&tool) {
                return HookEvent::Ignore;
            }
            match p.get("tool_input").and_then(|ti| str_field(ti, "file_path")) {
                Some(fp) => HookEvent::Write {
                    tool_name: tool.to_string(),
                    file_path: fp.to_string(),
                    holder,
                },
                None => HookEvent::Ignore,
            }
        }
        "subagent-stop" => HookEvent::ReleaseSubagent { holder },
        "session-end" => HookEvent::ReleaseSession { holder },
        _ => HookEvent::Ignore,
    }
}

/// The current PreToolUse deny envelope. `reason` is surfaced to the agent.
pub fn deny_json(reason: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason
        }
    })
}

#[derive(Deserialize, Default)]
struct HarnessSection {
    #[serde(default)]
    enforce_writes: bool,
}

#[derive(Deserialize, Default)]
struct HarnessProbe {
    #[serde(default)]
    harness: HarnessSection,
}

/// True iff `<root>/devkit.toml` opts this checkout into write enforcement. Parses
/// leniently — only the `[harness]` table is read, so a checkout that wants the
/// harness need not supply a full devkit project config.
pub fn harness_enabled(root: &Path) -> bool {
    let Ok(body) = std::fs::read_to_string(root.join("devkit.toml")) else {
        return false;
    };
    toml::from_str::<HarnessProbe>(&body)
        .map(|p| p.harness.enforce_writes)
        .unwrap_or(false)
}
```

Then add to `crates/devkit-locks/src/lib.rs` near the other `pub mod` lines:

```rust
pub mod hook;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p devkit-locks --lib hook::`
Expected: PASS (all 10).

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-locks/src/hook.rs crates/devkit-locks/src/lib.rs crates/devkit-locks/Cargo.toml
git commit -m "feat(locks): add hook payload parsing and activation gate"
```

---

## Task 7: `lockm hook <event>` subcommand

**Files:**
- Modify: `src/bin/lockm.rs`
- Test: `src/bin/lockm.rs` (`#[cfg(test)] mod tests` — pure decision-to-exit mapping) + a manual CLI smoke in the step.

**Interfaces:**
- Consumes: `devkit_locks::hook::{parse_event, deny_json, harness_enabled, HookEvent}`, `devkit_locks::{decide_write, release_prefix}`, `devkit_locks::model::WriteDecision`, `devkit_locks::find_root_from`.
- Produces: CLI `lockm hook <event>` reading JSON on stdin, writing the decision to stdout, always exiting 0.

Behavior: read stdin → parse JSON (on malformed JSON: allow, exit 0). Resolve the checkout root from the payload `cwd` (fallback: process cwd). If `!harness_enabled(root)` → exit 0 silently. Else dispatch the `HookEvent`:
- `Write` → `decide_write(file_path, holder, Some("write-harness"), 1800)`. `Acquired`/`AllowedByOwnership` → exit 0 (no stdout, normal flow). `Denied(conflicts)` → print `deny_json(reason)` and exit 0. On `Err` → **fail closed**: print `deny_json("devkit write-harness: registry error (fail-closed): …")`, exit 0.
- `ReleaseSubagent`/`ReleaseSession` → `release_prefix(holder)` (best-effort; ignore errors), exit 0.
- `Ignore` → exit 0.

- [ ] **Step 1: Write the failing test** — add to `src/bin/lockm.rs` a `#[cfg(test)] mod tests` covering the pure decision→output mapping (factor the mapping into a helper so it is testable without IO):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use devkit_locks::model::{Conflict, WriteDecision};

    #[test]
    fn allowed_decisions_emit_nothing() {
        assert_eq!(write_output(&WriteDecision::Acquired), None);
        assert_eq!(write_output(&WriteDecision::AllowedByOwnership), None);
    }

    #[test]
    fn denied_decision_emits_deny_with_holder() {
        let d = WriteDecision::Denied(vec![Conflict {
            path: "src/a.rs".into(),
            held_by: "S/b2".into(),
            age_secs: 5,
            note: None,
        }]);
        let out = write_output(&d).expect("deny json");
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
        let reason = out["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap();
        assert!(reason.contains("S/b2"), "reason names the holder: {reason}");
        assert!(reason.contains("src/a.rs"), "reason names the path: {reason}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit --bin lockm write_output`
Expected: FAIL — `cannot find function write_output`.

- [ ] **Step 3: Implement the subcommand** — in `src/bin/lockm.rs`:

Add the variant to `enum Cmd`:

```rust
    /// Internal: evaluate a coding-agent hook payload (stdin JSON) and emit a
    /// PreToolUse decision (stdout). Events: pretooluse | subagent-stop | session-end.
    #[command(hide = true)]
    Hook { event: String },
```

Add the pure helper (above `fn main`):

```rust
use devkit_locks::hook::{self, HookEvent};
use devkit_locks::model::WriteDecision;

/// Map a write decision to the optional stdout envelope. `None` = allow silently.
fn write_output(d: &WriteDecision) -> Option<serde_json::Value> {
    match d {
        WriteDecision::Acquired | WriteDecision::AllowedByOwnership => None,
        WriteDecision::Denied(conflicts) => {
            let who = conflicts
                .iter()
                .map(|c| format!("{} (held by {})", c.path, c.held_by))
                .collect::<Vec<_>>()
                .join(", ");
            Some(hook::deny_json(&format!(
                "devkit write-harness: {who} — locked by another agent; \
                 coordinate or wait for it to finish"
            )))
        }
    }
}
```

Add the match arm in `main` (returns `Ok(())` after emitting; the hook always exits 0):

```rust
        Cmd::Hook { event } => {
            run_hook(&event);
            Ok(())
        }
```

Add the driver:

```rust
fn run_hook(event: &str) {
    use std::io::Read;
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        return; // can't read payload → allow
    }
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&buf) else {
        return; // malformed → allow
    };

    // Resolve the checkout root from the payload cwd (fallback: process cwd).
    let root = payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .map(|p| devkit_locks::find_root_from(&p))
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    if !hook::harness_enabled(&root) {
        return; // opt-in absent → no enforcement
    }

    match hook::parse_event(event, &payload) {
        HookEvent::Write { file_path, holder, .. } => {
            match devkit_locks::decide_write(&file_path, &holder, Some("write-harness"), 1800) {
                Ok(decision) => {
                    if let Some(out) = write_output(&decision) {
                        println!("{out}");
                    }
                }
                Err(e) => {
                    // fail closed: a registry error must not silently reopen the window
                    let out = hook::deny_json(&format!(
                        "devkit write-harness: registry error (fail-closed): {e:#}"
                    ));
                    println!("{out}");
                }
            }
        }
        HookEvent::ReleaseSubagent { holder } | HookEvent::ReleaseSession { holder } => {
            let _ = devkit_locks::release_prefix(&holder);
        }
        HookEvent::Ignore => {}
    }
}
```

- [ ] **Step 4: Run the unit test + a CLI smoke test**

Run: `cargo test -p devkit --bin lockm write_output`
Expected: PASS.

Smoke (no devkit.toml in a temp dir → allow, no output):

```bash
cargo build -q --bin lockm
echo '{"session_id":"S","cwd":"/tmp","tool_name":"Edit","tool_input":{"file_path":"/tmp/x"}}' \
  | ./target/debug/lockm hook pretooluse
# Expected: no output (no opt-in marker at /tmp)
```

- [ ] **Step 5: Commit**

```bash
git add src/bin/lockm.rs
git commit -m "feat(lockm): add hook subcommand enforcing write access"
```

---

## Task 8: Multiprocess concurrent-write race test

**Files:**
- Create: `tests/lock_harness_race.rs`

**Interfaces:**
- Consumes: the public facade `devkit_locks::decide_write` (Task 4). Mirrors the multiprocess self-re-exec pattern of `crates/devkit-ports/tests/registry.rs`.

This proves two unrelated holders that race to write the same free file resolve to exactly one `Acquired` — the reserve-before-bind guarantee, now for writes. It must be **multiprocess** (the established pattern): same-process flock contention is not portable, so the test re-execs its own binary as a worker, isolating the registry with `HOME` + `XDG_STATE_HOME` pinned to a temp dir (state lives under `<XDG_STATE_HOME>/devkit/`).

- [ ] **Step 1: Write the failing test** — create `tests/lock_harness_race.rs`:

```rust
//! Two holders race to write the same free file through the real `decide_write`
//! facade in separate processes; exactly one acquires. Registry isolated via
//! HOME + XDG_STATE_HOME pinned to a temp dir (mirrors tests/registry.rs).

use devkit_locks::model::WriteDecision;
use std::process::Command;

#[test]
fn concurrent_write_decide_yields_one_winner() {
    // Worker mode: decide a write on the shared path and print the outcome.
    if let Ok(holder) = std::env::var("DEVKIT_TEST_WRITE") {
        let file = std::env::var("DEVKIT_TEST_FILE").unwrap();
        let d = devkit_locks::decide_write(&file, &holder, Some("race"), 0).unwrap();
        let tag = match d {
            WriteDecision::Acquired => "acquired",
            WriteDecision::AllowedByOwnership => "owned",
            WriteDecision::Denied(_) => "denied",
        };
        print!("{tag}");
        std::process::exit(0);
    }

    let tmp = std::env::temp_dir().join(format!("devkit-wrace-{}", std::process::id()));
    let repo = tmp.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    let file = repo.join("src/a.rs");
    let exe = std::env::current_exe().unwrap();

    let kids: Vec<_> = ["A", "B"]
        .into_iter()
        .map(|holder| {
            Command::new(&exe)
                // Pin BOTH state-home inputs so the worker registry is isolated;
                // setting only one leaks to the developer's real registry.
                .env("HOME", &tmp)
                .env("XDG_STATE_HOME", &tmp)
                .env("DEVKIT_TEST_WRITE", holder)
                .env("DEVKIT_TEST_FILE", &file)
                .args(["--exact", "concurrent_write_decide_yields_one_winner", "--nocapture"])
                .spawn()
                .unwrap()
        })
        .collect();

    let tags: Vec<String> = kids
        .into_iter()
        .map(|c| {
            let o = c.wait_with_output().unwrap();
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        })
        .collect();

    let acquired = tags.iter().filter(|t| *t == "acquired").count();
    let denied = tags.iter().filter(|t| *t == "denied").count();
    assert_eq!(acquired, 1, "exactly one writer acquires: {tags:?}");
    assert_eq!(denied, 1, "the other is denied: {tags:?}");
    let _ = std::fs::remove_dir_all(&tmp);
}
```

Note: `.spawn()` (not `.output()`) starts both workers before either blocks, so they genuinely race; `wait_with_output` collects each. The worker's stdout carries only the tag because the worker `exit(0)`s before the test harness prints its own summary.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit --test lock_harness_race`
Expected: FAIL — no such test file yet (or, once created, FAIL only if `decide_write` is missing; it exists from Task 4).

- [ ] **Step 3: No implementation needed** — the test exercises existing facade code (Task 4). If it fails because both workers `acquire`, the registry is not isolated: confirm both `HOME` and `XDG_STATE_HOME` are set and that `decide_write` resolves the root to `<tmp>/repo` (the `.git` dir makes `find_root_from` stop there).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p devkit --test lock_harness_race`
Expected: PASS — one `acquired`, one `denied`.

- [ ] **Step 5: Commit**

```bash
git add tests/lock_harness_race.rs
git commit -m "test(locks): cover concurrent write-decide race"
```

---

## Task 9: Plugin wiring + dogfood marker

**Files:**
- Create: `hooks/hooks.json`
- Create: `devkit.toml` (repo root, dogfood opt-in)

**Interfaces:**
- Consumes: the installed `lockm` binary (on PATH via `cargo install --path .`).
- Produces: a Claude Code plugin that fires the three hooks.

- [ ] **Step 1: Create the hook manifest** — `hooks/hooks.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Edit|MultiEdit|Write|NotebookEdit",
        "hooks": [
          { "type": "command", "command": "lockm hook pretooluse" }
        ]
      }
    ],
    "SubagentStop": [
      {
        "hooks": [
          { "type": "command", "command": "lockm hook subagent-stop" }
        ]
      }
    ],
    "SessionEnd": [
      {
        "hooks": [
          { "type": "command", "command": "lockm hook session-end" }
        ]
      }
    ]
  }
}
```

Note: `lockm` is invoked by bare name so it resolves from PATH (where `cargo install --path .` puts it). If absent, the shell exit (127) is a non-blocking PreToolUse error — the write proceeds (fail-open), matching the spec.

- [ ] **Step 2: Reference the manifest from the plugin** — confirm `.claude-plugin/plugin.json` picks up `hooks/hooks.json`. Per the plugin spec, a plugin-root `hooks/hooks.json` is auto-discovered; if `plugin.json` requires an explicit `"hooks"` pointer, add `"hooks": "./hooks/hooks.json"`. Verify by reading the current `.claude-plugin/plugin.json` and matching the established `"skills"`/`"hooks"` key convention used by `hooks-codex.json`.

- [ ] **Step 3: Add the dogfood opt-in** — create repo-root `devkit.toml`:

```toml
# devkit dogfoods its own write-access harness on this checkout.
[harness]
enforce_writes = true
```

- [ ] **Step 4: Validate the plugin** — dispatch the `plugin-dev:plugin-validator` agent against the repo, and confirm the JSON parses:

```bash
jq . hooks/hooks.json >/dev/null && echo "hooks.json valid"
jq . .claude-plugin/plugin.json >/dev/null && echo "plugin.json valid"
```

Expected: both print "valid"; validator reports no errors.

- [ ] **Step 5: Commit**

```bash
git add hooks/hooks.json devkit.toml
git commit -m "feat(plugin): wire write-access harness hooks for claude code"
```

---

## Task 10: Docs + full gate + manual e2e checklist

**Files:**
- Modify: `skills/using-devkit/SKILL.md`
- Modify: `docs/configuration.md`

**Interfaces:** none (documentation + verification).

- [ ] **Step 1: Document enforced mode in SKILL.md** — add a section to `skills/using-devkit/SKILL.md` stating: when a checkout's `devkit.toml` sets `[harness] enforce_writes = true`, the devkit plugin enforces write locks automatically via a PreToolUse hook; agents do **not** call `lockm acquire`/`release` themselves there (the harness owns the protocol, auto-acquiring on first write and releasing on SubagentStop/SessionEnd). A blocked write returns a deny naming the holder — wait or coordinate. Manual `lockm` remains for non-enforced checkouts. Keep wording timeless (no PR/change narration).

- [ ] **Step 2: Document the flag in docs/configuration.md** — add a `[harness]` subsection documenting `enforce_writes` (bool, default false): what it gates (Edit/MultiEdit/Write/NotebookEdit), that it is per-checkout opt-in, the fail-open/fail-closed stance, and that `Bash` writes are out of scope.

- [ ] **Step 3: Run the full gate**

```bash
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: fmt clean; all tests green; zero clippy warnings.

- [ ] **Step 4: Commit**

```bash
git add skills/using-devkit/SKILL.md docs/configuration.md
git commit -m "docs: document the write-access harness and enforce_writes flag"
```

- [ ] **Step 5: Manual end-to-end (record results; not a blocker for the gate)**

Install and exercise on a real Claude Code install (Windows, the dev platform):

```bash
cargo install --path .
# /plugin install devkit, then in a checkout with [harness] enforce_writes = true:
```

Confirm, polling state (no fixed sleeps):
1. **Auto-acquire:** a fresh session edits a free file → succeeds; `lockm status` shows a `write-harness` lock held by the session id.
2. **Cross-session block:** a second session editing the same file → denied with a message naming the first holder.
3. **Sub-agent lanes:** two parallel sub-agents editing the same file → exactly one succeeds; the other is denied; `lockm status` shows holders `S/<agent_id>`.
4. **Ancestor allow:** a parent that holds a file, then delegates an edit of it to a sub-agent → the sub-agent's edit succeeds (ancestor owns it).
5. **Release:** after a sub-agent finishes, its lock disappears (SubagentStop); after the session ends, all its locks disappear (SessionEnd).
6. **Opt-out:** in a checkout without the flag, edits are never blocked and no locks are taken.

---

## Self-Review

**Spec coverage:**
- Activation (opt-in, default off) → Task 6 (`harness_enabled`) + Task 9 (marker) + Task 7 (gate before dispatch). ✓
- Ownership rule (free/self/ancestor/other) → Tasks 1–2. ✓
- Auto-acquire → Task 2 (`Acquired`) + Task 7 (`decide_write` call). ✓
- Two-level holder identity → Task 6 (`holder_from_fields`). ✓ (deviation noted)
- Lifecycle (SubagentStop/SessionEnd release; prune backstop) → Tasks 2/6/7 (`release_prefix`) + existing `prune`. ✓
- `lockm hook` over the facade → Tasks 4–7. ✓
- Daemon path → Task 5. ✓
- Failure modes (off/absent → open; registry error → closed; malformed/no file_path → open) → Task 6 (`Ignore`) + Task 7 (driver branches). ✓
- Writes only; Bash out of scope → Task 6 (`WRITE_TOOLS`) + Task 10 docs. ✓
- Claude Code first, portable → logic in `devkit-locks`/`lockm`; only `hooks/hooks.json` is CC-specific. ✓
- Tests: decision matrix (Task 2), seam (Task 3), daemon (Task 5), race (Task 8), manual e2e (Task 10). ✓

**Placeholder scan:** none — every code/test step carries complete content.

**Type consistency:** `WriteDecision { Acquired, AllowedByOwnership, Denied(Vec<Conflict>) }` used identically in Tasks 2/3/4/5/7. `decide_write(path_in, holder, note, ttl)` and `release_prefix(holder_prefix)` signatures match between Task 4 (def) and Task 7 (call). `write_decide_with`/`release_prefix_with` signatures match Tasks 3/5. `holder_from_fields`/`parse_event`/`deny_json`/`harness_enabled` match Tasks 6/7. Task 8 reuses the public `decide_write` facade (Task 4) via multiprocess self-re-exec — no store change, no new ctor.

## Open questions (verify during execution)

1. **`.claude-plugin/plugin.json` hooks discovery** (Task 9 Step 2) — whether plugin-root `hooks/hooks.json` is auto-discovered or needs an explicit `"hooks": "./hooks/hooks.json"` pointer. Resolve by reading the current manifest and the plugin spec.
2. **`SessionEnd` reliability on hard exit** — if it does not fire on Ctrl-C, locks fall to the TTL backstop (1800s). Confirm during the Task 10 manual run; if too coarse, consider a shorter harness TTL.
3. **`agent_id` stability across a sub-agent's tool calls** — the design assumes it is constant for the life of one sub-agent invocation (so all its writes share a holder). Confirm in the Task 10 run.
