# devkit MCP cooperation/correctness follow-ups — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the MCP lock actions cooperate with a live `devkitd`, echo the host's MCP protocol version on `initialize`, and document why the ports holder is the worktree root path.

**Architecture:** Add explicit-context (`_resolved`) lock facade fns to `devkit-locks` that route daemon-first/flock-fallback (the existing CWD fns delegate to them); repoint the MCP locks handler at those fns and at the already-daemon-aware `prune()`. Read `protocolVersion` from `initialize` params and echo it back. Document the ports-holder rationale.

**Tech Stack:** Rust (edition 2024), `anyhow`, `serde_json`, the `devkit-locks` flock/daemon store, the stdio JSON-RPC MCP server in `devkit-mcp`.

**Spec:** `docs/superpowers/specs/2026-06-24-devkit-mcp-cooperation-followups-design.md`

---

## Reference: current code being changed

- `crates/devkit-locks/src/lib.rs` — CWD fns `acquire`/`check`/`release`/`release_all`/`status`/`prune` each inline a `#[cfg(feature = "daemon")] daemon_request(…)?` block then fall back to `store::*_with(&FlockStore::new(), …)`. Types: `AcquireOutcome { acquired: Vec<Acquired>, conflicts: Vec<Conflict> }`, `Acquired { path, ttl_secs }`, `Conflict { path, held_by, age_secs, note }`, `LockEntry { path, root, holder, pid, note, … }`. The test module has a `scratch(tag)` helper returning a unique temp `PathBuf`.
- `crates/devkit-mcp/src/locks.rs` — handlers call `store::*_with(&FlockStore::new(), …, now())` directly. Local `now()` helper + `use std::time::{SystemTime, UNIX_EPOCH};` exist only for that. `normalize`/`resolve_holder` helpers stay.
- `crates/devkit-mcp/src/lib.rs` — `dispatch`'s `initialize` arm calls `initialize_result()` (no args), which hardcodes `"protocolVersion": "2024-11-05"`. `Request { params: Value (serde default Null) }`. Test helper `drive(input) -> Vec<Value>`.

## File Structure

| File | Change |
|---|---|
| `crates/devkit-locks/src/lib.rs` | Add 5 `_resolved` fns; refactor the 5 CWD fns to delegate; add a fallback unit test. |
| `crates/devkit-mcp/src/locks.rs` | Repoint handlers to `devkit_locks::{*_resolved, prune}`; drop now-unused `store` imports, `std::time` import, local `now()`; add a round-trip guard test. |
| `crates/devkit-mcp/src/lib.rs` | Read+echo `protocolVersion`; add two tests. |
| `AGENTS.md` | Document ports-holder = root-path rationale. |
| `docs/next-steps.md` | Flip the ports-holder bullet to RESOLVED; flip the daemon-aware-locks and protocol-negotiation bullets to shipped. |

---

## Task 1: Explicit-context lock facade variants in `devkit-locks`

**Files:**
- Modify: `crates/devkit-locks/src/lib.rs` (add `_resolved` fns; refactor CWD fns `acquire`/`check`/`release`/`release_all`/`status` to delegate)
- Test: `crates/devkit-locks/src/lib.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/devkit-locks/src/lib.rs`. It exercises each new fn against the global flock store on a unique root (so entries can't collide with other tests or the user's real locks), and cleans up:

```rust
#[test]
fn resolved_fns_roundtrip_via_flock_path() {
    // No daemon runs in unit tests, so the `_resolved` fns fall through to the
    // FlockStore path. A unique root namespaces these lock rows.
    let root = scratch("resolved-roundtrip");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let r = root.to_string_lossy().into_owned();
    let paths = vec!["a.rs".to_string()];

    let out = acquire_resolved(&r, "holder-a", &paths, None, None, 60).expect("acquire");
    assert_eq!(out.acquired.len(), 1);
    assert_eq!(out.acquired[0].path, "a.rs");
    assert!(out.conflicts.is_empty());

    let conflicts = check_resolved(&r, "holder-b", &paths).expect("check");
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].held_by, "holder-a");

    let entries = status_resolved(&r, false).expect("status");
    assert!(entries.iter().any(|e| e.path == "a.rs" && e.holder == "holder-a"));

    let (released, refused) = release_resolved(&r, "holder-a", &paths, false).expect("release");
    assert_eq!(released, vec!["a.rs".to_string()]);
    assert!(refused.is_empty());

    // release_all on a now-empty root is a no-op but must succeed.
    assert!(release_all_resolved(&r, "holder-a").expect("release_all").is_empty());

    let _ = std::fs::remove_dir_all(&root);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p devkit-locks resolved_fns_roundtrip_via_flock_path`
Expected: FAIL to compile — `cannot find function acquire_resolved` (and the other four).

- [ ] **Step 3: Add the five `_resolved` fns**

Insert these into `crates/devkit-locks/src/lib.rs` (next to the CWD fns, before `decide_write`). Each is the single dispatch body for its operation:

```rust
/// Acquire `paths` for `holder` under `root` with a pre-resolved context (no CWD
/// or identity derivation). Routes through a live daemon when one is up, else the
/// flock store. The CWD-deriving `acquire` delegates here.
pub fn acquire_resolved(
    root: &str,
    holder: &str,
    paths: &[String],
    pid: Option<u32>,
    note: Option<&str>,
    ttl: u64,
) -> Result<AcquireOutcome> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(daemon::proto::Request::Acquire {
        root: root.to_string(),
        holder: holder.to_string(),
        paths: paths.to_vec(),
        pid,
        note: note.map(str::to_string),
        ttl,
    })? {
        return match resp {
            daemon::proto::Response::Acquired(o) => Ok(o),
            daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    store::acquire_with(
        &store::FlockStore::new(),
        root,
        holder,
        paths,
        pid,
        note,
        ttl,
        now(),
    )
}

/// Conflicts that would block `holder` from `paths` under `root` (pre-resolved).
pub fn check_resolved(root: &str, holder: &str, paths: &[String]) -> Result<Vec<Conflict>> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(daemon::proto::Request::Check {
        root: root.to_string(),
        holder: holder.to_string(),
        paths: paths.to_vec(),
    })? {
        return match resp {
            daemon::proto::Response::Conflicts(v) => Ok(v),
            daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    store::check_with(&store::FlockStore::new(), root, holder, paths, now())
}

/// Release named `paths` held by `holder` under `root` (pre-resolved). Returns
/// (released, refused).
pub fn release_resolved(
    root: &str,
    holder: &str,
    paths: &[String],
    force: bool,
) -> Result<(Vec<String>, Vec<String>)> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(daemon::proto::Request::Release {
        root: root.to_string(),
        holder: holder.to_string(),
        paths: paths.to_vec(),
        force,
    })? {
        return match resp {
            daemon::proto::Response::Released { released, refused } => Ok((released, refused)),
            daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    store::release_with(&store::FlockStore::new(), root, holder, paths, force)
}

/// Release every lock held by `holder` under `root` (pre-resolved).
pub fn release_all_resolved(root: &str, holder: &str) -> Result<Vec<String>> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(daemon::proto::Request::ReleaseAll {
        root: root.to_string(),
        holder: holder.to_string(),
    })? {
        return match resp {
            daemon::proto::Response::Freed(v) => Ok(v),
            daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    store::release_all_with(&store::FlockStore::new(), root, holder)
}

/// Live locks for `root`, or every project when `all` (pre-resolved root).
pub fn status_resolved(root: &str, all: bool) -> Result<Vec<LockEntry>> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(daemon::proto::Request::Status {
        root: root.to_string(),
        all,
    })? {
        return match resp {
            daemon::proto::Response::Locks(v) => Ok(v),
            daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    store::status_with(&store::FlockStore::new(), root, all, now())
}
```

- [ ] **Step 4: Refactor the CWD fns to delegate**

Replace the bodies of the existing `acquire`, `check`, `release`, `release_all`, and `status` fns (lines ~113-230) so each derives context then delegates — removing the now-duplicated daemon blocks:

```rust
pub fn acquire(
    paths_in: &[String],
    as_flag: Option<&str>,
    note: Option<&str>,
    ttl: u64,
) -> Result<AcquireOutcome> {
    let c = ctx(paths_in, as_flag)?;
    acquire_resolved(&c.root, &c.holder, &c.paths, ident::anchor_pid(), note, ttl)
}

pub fn check(paths_in: &[String], as_flag: Option<&str>) -> Result<Vec<Conflict>> {
    let c = ctx(paths_in, as_flag)?;
    check_resolved(&c.root, &c.holder, &c.paths)
}

pub fn release(
    paths_in: &[String],
    as_flag: Option<&str>,
    force: bool,
) -> Result<(Vec<String>, Vec<String>)> {
    let c = ctx(paths_in, as_flag)?;
    release_resolved(&c.root, &c.holder, &c.paths, force)
}

pub fn release_all(as_flag: Option<&str>) -> Result<Vec<String>> {
    let c = ctx(&[], as_flag)?;
    release_all_resolved(&c.root, &c.holder)
}

/// Live locks for the current project root, or every project when `all`.
pub fn status(all: bool) -> Result<Vec<LockEntry>> {
    let root = find_root()?.to_string_lossy().into_owned();
    status_resolved(&root, all)
}
```

Leave `prune`, `decide_write`, and `release_prefix` unchanged.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p devkit-locks`
Expected: PASS — the new `resolved_fns_roundtrip_via_flock_path` and all existing tests (including `facade_without_daemon_uses_flock_path` and the CWD-path tests) are green.

- [ ] **Step 6: Lint**

Run: `cargo clippy -p devkit-locks --all-targets -- -D warnings`
Expected: zero warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/devkit-locks/src/lib.rs
git commit -m "feat(locks): add explicit-context daemon-aware facade fns"
```

---

## Task 2: Repoint the MCP locks handler at the facade

**Files:**
- Modify: `crates/devkit-mcp/src/locks.rs` (imports, all five handlers, drop local `now()`)
- Test: `crates/devkit-mcp/src/locks.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the guard test**

This is a behavior-preserving refactor (no daemon ⇒ identical flock path), so the test guards against regression rather than failing first. Add a `tests` module at the end of `crates/devkit-mcp/src/locks.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ServerCtx {
        ServerCtx {
            default_holder: format!("mcp-locks-test-{}", std::process::id()),
        }
    }

    #[test]
    fn acquire_status_release_roundtrip_through_handlers() {
        let root = std::env::temp_dir().join(format!("devkit-mcp-locks-{}", std::process::id()));
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let r = root.to_string_lossy().into_owned();
        let c = ctx();

        let out = acquire(&c, serde_json::json!({ "root": r, "paths": ["x.rs"], "ttl": 60 }))
            .expect("acquire");
        assert_eq!(out["acquired"].as_array().unwrap().len(), 1);

        let st = status(&c, serde_json::json!({ "root": r })).expect("status");
        assert!(st.as_array().unwrap().iter().any(|e| e["path"] == "x.rs"));

        let rel = release(&c, serde_json::json!({ "root": r, "paths": ["x.rs"] }))
            .expect("release");
        assert_eq!(rel["released"], serde_json::json!(["x.rs"]));

        let _ = std::fs::remove_dir_all(&root);
    }
}
```

- [ ] **Step 2: Run it against the current code**

Run: `cargo test -p devkit-mcp acquire_status_release_roundtrip_through_handlers`
Expected: PASS (the current handler already works without a daemon). This locks in current behavior before the swap.

- [ ] **Step 3: Swap the imports**

In `crates/devkit-mcp/src/locks.rs`, replace the top import block:

```rust
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

use devkit_locks::normalize_under_root;
use devkit_locks::store::{
    FlockStore, acquire_with, check_with, prune_with, release_all_with, release_with, status_with,
};

use crate::ServerCtx;
use crate::actions::Action;
```

with:

```rust
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

use devkit_locks::normalize_under_root;
use devkit_locks::{
    acquire_resolved, check_resolved, release_all_resolved, release_resolved, status_resolved,
};

use crate::ServerCtx;
use crate::actions::Action;
```

`prune` is **not** imported — the module already has a local `fn prune` handler, so an
import of the same name would be a duplicate definition. The handler calls
`devkit_locks::prune()` fully qualified instead (Step 5).

- [ ] **Step 4: Delete the local `now()` helper**

Remove this block (it has no remaining callers after the swap):

```rust
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
```

- [ ] **Step 5: Repoint the five handlers**

Replace each `store::*_with(&FlockStore::new(), …)` call:

In `acquire`:
```rust
    let outcome = acquire_resolved(&a.root, &holder, &paths, None, a.note.as_deref(), a.ttl)?;
    Ok(serde_json::to_value(outcome)?)
```

In `check`:
```rust
    let conflicts = check_resolved(&a.root, &holder, &paths)?;
    Ok(serde_json::to_value(conflicts)?)
```

In `release` (both branches):
```rust
    if a.all {
        let released = release_all_resolved(&a.root, &holder)?;
        return Ok(serde_json::json!({ "released": released, "refused": [] }));
    }
    if a.paths.is_empty() {
        bail!("locks.release requires `paths` unless `all` is true");
    }
    let paths = normalize(&a.root, &a.paths)?;
    let (released, refused) = release_resolved(&a.root, &holder, &paths, a.force)?;
    Ok(serde_json::json!({ "released": released, "refused": refused }))
```

In `status`:
```rust
    let entries = status_resolved(&root, a.all)?;
    Ok(serde_json::to_value(entries)?)
```

In `prune` — call `devkit_locks::prune()` fully qualified (the local handler fn is also
named `prune`, so a bare call would recurse into itself):

```rust
fn prune(_ctx: &ServerCtx, _args: Value) -> Result<Value> {
    let pruned = devkit_locks::prune()?;
    Ok(serde_json::json!({ "pruned": pruned }))
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p devkit-mcp`
Expected: PASS — the round-trip guard and the existing `actions.rs` describe/schema tests stay green.

- [ ] **Step 7: Lint**

Run: `cargo clippy -p devkit-mcp --all-targets -- -D warnings`
Expected: zero warnings (no unused `FlockStore`/`SystemTime`/`now` left behind).

- [ ] **Step 8: Commit**

```bash
git add crates/devkit-mcp/src/locks.rs
git commit -m "feat(mcp): route lock actions through the daemon-aware facade"
```

---

## Task 3: `initialize` protocol-version negotiation

**Files:**
- Modify: `crates/devkit-mcp/src/lib.rs` (`dispatch` initialize arm, `initialize_result`, new helper)
- Test: `crates/devkit-mcp/src/lib.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/devkit-mcp/src/lib.rs`:

```rust
#[test]
fn initialize_echoes_client_protocol_version() {
    let resps = drive(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\"}}\n",
    );
    assert_eq!(resps[0]["result"]["protocolVersion"], "2025-06-18");
}

#[test]
fn initialize_defaults_protocol_version_when_absent() {
    let resps = drive("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n");
    assert_eq!(resps[0]["result"]["protocolVersion"], "2024-11-05");
}
```

- [ ] **Step 2: Run them to verify failure**

Run: `cargo test -p devkit-mcp initialize_echoes_client_protocol_version`
Expected: FAIL — the result is `"2024-11-05"`, not `"2025-06-18"` (the handler ignores params today).

- [ ] **Step 3: Read and echo the requested version**

In `crates/devkit-mcp/src/lib.rs`, change the `initialize` dispatch arm:

```rust
        "initialize" => Some(Response::ok(
            req.id.clone()?,
            initialize_result(client_protocol_version(&req.params)),
        )),
```

Add the helper (above `initialize_result`):

```rust
/// The client's requested MCP protocol version from `initialize` params, when it is
/// a non-empty string. The server is version-agnostic, so this is echoed back.
fn client_protocol_version(params: &Value) -> Option<&str> {
    params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}
```

Change `initialize_result` to take the requested version and default when absent:

```rust
fn initialize_result(requested: Option<&str>) -> Value {
    serde_json::json!({
        "protocolVersion": requested.unwrap_or("2024-11-05"),
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "devkit-mcp", "version": env!("CARGO_PKG_VERSION") }
    })
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p devkit-mcp`
Expected: PASS — both new tests plus the existing `initialize_returns_server_info` (it sends `params:{}`, so it still gets `"2024-11-05"` and the unchanged `serverInfo`/`capabilities`).

- [ ] **Step 5: Lint**

Run: `cargo clippy -p devkit-mcp --all-targets -- -D warnings`
Expected: zero warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-mcp/src/lib.rs
git commit -m "feat(mcp): echo the client's initialize protocol version"
```

---

## Task 4: Document ports-holder semantics (#4)

**Files:**
- Modify: `AGENTS.md` (Registry-facade section)
- Modify: `docs/next-steps.md` (MCP deferred-follow-ups bullets)

- [ ] **Step 1: Add the AGENTS.md note**

In `AGENTS.md`, at the end of the "Registry facade" section (after the daemon paragraph, near line 154), append:

```markdown

The ports holder is the worktree **root path**, not a minted session token:
`registry::holder_alive(holder)` is `Path::new(holder).exists()`, so a holder is judged
live by whether its directory still exists. This is what makes a worktree's ports
auto-reclaim on `git worktree remove` — the holder path vanishes and `prune` frees the
rows. (Locks instead use a session-token holder with TTL/pid liveness; the two registries
intentionally differ.) Cross-worktree, an agent addresses each worktree's allocations by
that worktree's root path.
```

- [ ] **Step 2: Flip the next-steps bullets**

In `docs/next-steps.md`, under "## MCP server for devkit" → "Deferred follow-ups", update the three resolved items.

Replace the "Daemon-aware locks" bullet body with a shipped note:

```markdown
- **Daemon-aware locks (shipped).** Lock actions route through explicit-context
  `devkit_locks::{acquire,check,release,release_all,status}_resolved` (and the
  already-daemon-aware `prune`), which try a live `devkitd` over `locks.sock` first and
  fall back to `FlockStore`. The MCP locks handler no longer hits `FlockStore` directly,
  so it cooperates with a running daemon instead of erroring `DaemonHoldsLock`.
```

Replace the "`initialize` protocol-version negotiation" bullet body:

```markdown
- **`initialize` protocol-version negotiation (shipped).** The server echoes the
  client's requested `protocolVersion` back (falling back to the `2024-11-05` baseline
  when absent). devkit-mcp is version-agnostic (only `tools/list` + `tools/call`), so
  echoing maximizes host compatibility.
```

Replace the "Ports holder is the project root" bullet body:

```markdown
- **Ports holder is the project root (resolved).** Confirmed correct by design: the
  holder is the worktree root **path** because `holder_alive` = `path.exists()` is the
  liveness signal that auto-reclaims a worktree's ports on removal — distinct from locks'
  session-token holder. Documented in `AGENTS.md` (Registry facade).
```

- [ ] **Step 3: Verify the docs render**

Run: `rg -n "Daemon-aware locks \(shipped\)|protocol-version negotiation \(shipped\)|Ports holder is the project root \(resolved\)" docs/next-steps.md`
Expected: three matches — one per flipped bullet.

- [ ] **Step 4: Commit**

```bash
git add AGENTS.md docs/next-steps.md
git commit -m "docs: record mcp daemon-aware locks, version echo, ports holder"
```

---

## Final verification

- [ ] **Full gate**

Run:
```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: formatting clean, zero clippy warnings, all tests pass.

- [ ] **Finish the branch**

Use superpowers:finishing-a-development-branch to merge/clean up.
