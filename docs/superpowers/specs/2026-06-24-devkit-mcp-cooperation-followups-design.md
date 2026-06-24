# devkit MCP server — cooperation/correctness follow-ups — design

## Goal

Close three of the deferred `devkit-mcp` follow-ups from `docs/next-steps.md` in one
cycle. They share a theme — making the running MCP server cooperate correctly with its
environment — and none of them changes the two-tool meta shape (`devkit_describe` +
`devkit_call`):

1. **Daemon-aware MCP locks (#1).** Lock actions cooperate with a live `devkitd`
   instead of hard-erroring.
2. **`initialize` protocol-version negotiation (#3).** Echo the host's requested
   protocol version instead of hardcoding one.
3. **Ports-holder semantics (#4).** Confirm the root-path holder is correct by design
   and document the rationale; no code change.

The deferred **mutating** `issue.review`/`issue.end` actions (#2) need a confirm-gating
design and are handled in a separate cycle — out of scope here.

## Background

The MCP server (`crates/devkit-mcp` + `src/bin/devkit-mcp`) is a meta-MCP stdio server
exposing port + lock + devrun + issue actions over the library facades. Two facts drive
this cycle:

- **Ports MCP is already daemon-aware; locks MCP is not.** `crates/devkit-mcp/src/ports.rs`
  calls `registry::{snapshot,alloc,release,prune}` — the daemon-aware facade, which tries
  the daemon over `ports.sock` first (`daemon_request`) and falls back to `FlockStore`.
  `crates/devkit-mcp/src/locks.rs` instead calls `devkit_locks::store::*_with(&FlockStore::new(), …)`
  directly, bypassing the daemon. Under a live `devkitd` (which holds `devkitd.lock`
  exclusive), those direct flock writes hard-error with `DaemonHoldsLock`.
- **The daemon-aware routing already exists in `devkit-locks`, but only on the CWD path.**
  `devkit_locks::{acquire,check,release,release_all,status,prune,…}` each call
  `daemon_request(…)` first and fall back to `store::*_with`. But they derive
  `root`/`holder`/`paths` from the process CWD (`ctx()` → `std::env::current_dir()`) and
  `ident::identity()`. The MCP server's CWD is **not** the agent's worktree, and the MCP
  already receives `root`/`holder`/`paths` as explicit arguments. So the MCP cannot call
  these CWD-deriving fns; it needs explicit-context variants of the same daemon-first,
  flock-fallback dispatch.

## Scope

| Item | Change |
|---|---|
| #1 Daemon-aware locks | New explicit-context facade fns in `devkit-locks`; repoint the MCP locks handler at them. |
| #3 Protocol negotiation | `initialize` echoes the client's `protocolVersion`, falls back to the current default. |
| #4 Ports holder | Documentation only (AGENTS.md + `docs/next-steps.md`); no code. |

Non-goals: mutating `issue` actions and their confirm-gating (#2, separate cycle);
`issue setup`/`dashboard`; any change to the two-tool meta shape; any change to the
no-daemon behavior of lock actions.

## Item #1 — Daemon-aware MCP locks

### Facade variants (`crates/devkit-locks/src/lib.rs`)

Add explicit-context public fns that take the already-resolved context and route
**daemon-first, flock-fallback** — the same shape the CWD fns use today, minus the
`ctx()` derivation. Each `_resolved` fn *is* the single dispatch body for its operation
(one `daemon_request` + `Response` match, flock fallback); the existing CWD fns delegate
to it after deriving context. No separate private helper — the `_resolved` fn is the one
body both call paths share.

```rust
pub fn acquire_resolved(
    root: &str, holder: &str, paths: &[String],
    pid: Option<u32>, note: Option<&str>, ttl: u64,
) -> Result<AcquireOutcome>;

pub fn check_resolved(root: &str, holder: &str, paths: &[String]) -> Result<Vec<Conflict>>;

pub fn release_resolved(
    root: &str, holder: &str, paths: &[String], force: bool,
) -> Result<(Vec<String>, Vec<String>)>;

pub fn release_all_resolved(root: &str, holder: &str) -> Result<Vec<String>>;

pub fn status_resolved(root: &str, all: bool) -> Result<Vec<LockEntry>>;
```

- Each calls the lib-internal `now()`; callers do not pass a clock.
- `prune` needs no variant — the existing context-free `devkit_locks::prune()` is already
  daemon-aware; the MCP handler calls it directly.
- The daemon `Request`/`Response` mapping reuses the existing arms (`Acquire`→`Acquired`,
  `Check`→`Conflicts`, `Release`→`Released{released,refused}`, `ReleaseAll`→`Freed`,
  `Status`→`Locks`); `Err(e)` → `anyhow::anyhow!(e)`; an unexpected variant errors.
- When `daemon_request` returns `Ok(None)` (no daemon, or inside the daemon via
  `DEVKITD_SELF`), the fn falls back to `store::*_with(&FlockStore::new(), …, now())`.

### Sharing the dispatch

The existing CWD fns (`acquire`, `check`, `release`, `release_all`, `status`) become thin
wrappers: derive `Ctx` from CWD/identity, then delegate to the matching `_resolved` fn.
This removes the duplicated daemon match arms and keeps one dispatch body per operation.
`decide_write` and `release_prefix` (hook paths) are untouched.

### MCP handler (`crates/devkit-mcp/src/locks.rs`)

Each handler swaps its `store::*_with(&FlockStore::new(), …)` call for the matching
`_resolved` fn; `prune` calls `devkit_locks::prune()`. `pid` stays `None` (TTL-backstopped,
released by lifecycle — unchanged from today). The `normalize`/`resolve_holder` helpers and
the arg structs/schemas are unchanged.

**Invariant preserved:** without a daemon, behavior is byte-for-byte identical (same
`FlockStore` path). With a daemon, the action cooperates instead of erroring.

## Item #3 — Protocol-version negotiation (`crates/devkit-mcp/src/lib.rs`)

`initialize` currently ignores params and returns a hardcoded `protocolVersion`. Change
the `initialize` dispatch arm to read `params.protocolVersion` and pass it to
`initialize_result(requested: Option<&str>)`, which:

- echoes `requested` back when it is a non-empty string;
- otherwise returns the current default `"2024-11-05"` (the MCP baseline).

`capabilities` and `serverInfo` are unchanged. The server is genuinely version-agnostic
(only `tools/list` + `tools/call` with basic JSON-RPC, no version-specific features), so
echoing maximizes host compatibility and never rejects a host on version.

## Item #4 — Ports-holder semantics (documentation only)

`registry::holder_alive(holder)` is `Path::new(holder).exists()`. The ports holder being
the worktree **root path** is therefore load-bearing: it is the liveness signal, so when a
worktree is removed its holder path vanishes and `prune` auto-reclaims those ports. A
session-token holder (as locks use) would break that automatic reclamation. This is correct
by design.

Document this in:

- **`AGENTS.md`** — a short note (near the registry-facade / holder discussion) that the
  ports holder is the worktree root path because liveness is `path.exists()`, distinct from
  locks' session-token holder; cross-worktree, an agent addresses each worktree's
  allocations by that worktree's root.
- **`docs/next-steps.md`** — flip the "Ports holder is the project root" bullet from a
  deferred question to RESOLVED with that rationale.

## Testing

TDD throughout; `cargo test --workspace` is the gate.

- **`devkit-locks` explicit fns:** unit-test the no-daemon fallback for each `_resolved`
  fn, mirroring the existing `facade_without_daemon_uses_flock_path` (no daemon runs in CI,
  so `daemon_request` returns `Ok(None)` and the flock path is exercised). Assert the CWD
  wrappers still resolve and delegate. Daemon-routed paths stay out of CI, matching existing
  precedent.
- **#3:** `lib.rs` unit tests — an `initialize` whose params carry `protocolVersion:
  "2025-06-18"` is echoed back; an `initialize` with no `protocolVersion` returns
  `"2024-11-05"`. Existing `initialize_returns_server_info` stays green.
- **MCP integration:** lock actions still appear in `devkit_describe` and validate their
  schemas (the handler swap must not change the surface).
- **Gate:** `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
  warnings`, `cargo test --workspace`. CI tests poll for state rather than sleeping a fixed
  interval (the Windows-runner rule).

## Error and conflict semantics

Unchanged, with one improvement: under a live daemon, a mid-request daemon failure now
surfaces as a `tools/call` result with `isError: true` and the daemon's error chain (the
facade returns the daemon's `Err`), instead of today's `DaemonHoldsLock`. Protocol/validation
failures remain JSON-RPC error responses; action failures remain `isError: true` results.

## Sequencing

1. **#1 facade variants.** Add the `_resolved` fns + shared dispatch helper in
   `devkit-locks`; refactor the CWD fns to delegate; unit tests. Repoint the MCP locks
   handler; MCP integration test.
2. **#3 negotiation.** Echo `protocolVersion` in `initialize`; unit tests.
3. **#4 docs.** AGENTS.md note + flip the next-steps bullet.

Each step is independently committable and keeps the gate green.

## Resolved decisions

1. **Facade shape (#1)** — separate explicit-context `_resolved` variants, not an injected
   optional `Ctx`, to keep the CWD-deriving public API the CLI/hook depend on clear.
2. **`pid` for MCP locks (#1)** — stays `None` (TTL-backstopped); no anchor to the server
   pid. No behavior change.
3. **`prune` (#1)** — reuse the existing context-free daemon-aware `prune()`; no variant.
4. **Negotiation policy (#3)** — echo the client's `protocolVersion`, default
   `"2024-11-05"` when absent. Maximizes compatibility; no supported-set list to maintain.
5. **Ports holder (#4)** — confirmed correct by design (path-based liveness); documented,
   not changed.

## Open questions

None blocking.
