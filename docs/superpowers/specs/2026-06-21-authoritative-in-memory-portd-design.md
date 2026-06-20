# Authoritative in-memory `devkit-portd` — design

**Goal:** Make `devkit-portd` hold the port registry in memory and serve reads
from memory without taking the flock or re-reading the file, writing through to
the file on every mutation, with the sole-writer boundary **enforced** by
`portd.lock` rather than assumed.

**Scope:** Port registry only (`devkit-ports` + `devkit-portd`). The lock
registry (`devkit-locks`) gets the same treatment in a separate follow-up spec
that reuses the seam introduced here; it is explicitly out of scope below.

**Status:** Approved design. Implementation pending (see the plan).

---

## Background

Today the daemon is a *flock participant*: every request handler calls the same
`registry::{alloc,record_pid,release,snapshot,prune}` flock facade the no-daemon
path uses, so each request takes the exclusive advisory lock, re-reads
`ports.json`, mutates, and writes it back. The daemon therefore does **not**
avoid the per-call flock + read — it only serializes access through one process
and adds supervision.

The win we want: high-frequency reads (`snapshot`, and later lock `check`)
served from the daemon's memory with no flock and no file parse. Mutations stay
durable by writing through to the file, which remains the source of truth for a
cold start (no daemon running).

## Consistency model — `portd.lock` as a shared/exclusive gate

`portd.lock` (the daemon's existing single-instance lock) becomes a read-write
gate every binary respects, independent of the `daemon` cargo feature:

- **Daemon** holds `portd.lock` **exclusive** for its whole life (unchanged from
  today's single-instance behavior).
- **Any direct file *writer*** (the `FlockStore` commit path) acquires a
  **non-blocking shared (`try_read`) lock on `portd.lock`** and holds it across
  the data-file read-modify-write:
  - **Acquire fails** (a daemon holds it exclusive) → **hard error**, surfaced to
    the user: *"a devkit-portd daemon holds the registry lock; refusing to modify
    `ports.json` behind it — stop the daemon or use a daemon-enabled binary."*
    Exit non-zero.
  - **Acquire succeeds** (no daemon) → keep the shared lock held, run the
    existing data-flock RMW, release.
- **Direct *reads* are ungated.** A reader reads the file directly; the daemon
  keeps the file current via write-through, so a read is always at least as fresh
  as the last committed mutation.

Because shared and exclusive are mutually exclusive at the OS level, this yields:

1. **No out-of-band desync.** A binary built without the `daemon` feature (or any
   process on the flock path) physically cannot take the shared gate while a
   daemon holds exclusive; it hard-errors instead of corrupting the daemon's
   view. This gate code is feature-independent, so it is compiled into every
   binary.
2. **No startup race.** The daemon takes `portd.lock` exclusive *before* loading
   the file. Any in-flight direct writer holds the shared gate, so the daemon's
   exclusive acquire blocks until that writer commits, then loads a consistent
   snapshot. No writer can slip in after the load.
3. **Flock-free write-through.** While the daemon holds exclusive, no direct
   writer can be active and its own connection threads serialize on a
   `Mutex<Data>`, so write-through is just the atomic rename — no data flock.

The data-file flock (`ports.lock`) now coordinates only concurrent *direct*
writers among themselves (the no-daemon case). `portd.lock` coordinates the
daemon-vs-direct boundary.

## Architecture — one `Store` seam, one engine, two drivers

The invariant-bearing logic (reserve-before-bind, probe-`listening`-outside-the-
lock, `record_pid` re-insert, `dead_ports` grace period) already lives in `Data`
methods + free probe fns. We wrap *driving* that logic behind a seam so each
invariant body exists exactly once and runs identically under flock or in memory.

```rust
/// A driver for the registry's read-modify-write cycle.
trait Store {
    /// Current registry state (a cheap read; no mutation).
    fn snapshot(&self) -> Result<Data>;
    /// Exclusive read-modify-write: run `f`, persist, return its value.
    fn commit<T>(&self, f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T>;
}
```

- **`FlockStore`** — `snapshot` reads the file (ungated). `commit` acquires the
  shared `portd.lock` gate, then runs `store::with_lock` (data-flock RMW + atomic
  write), then releases the gate. This is today's behavior plus the gate.
- **`MemoryStore { state: Arc<Mutex<Data>>, data_path: PathBuf }`** — `snapshot`
  clones under the mutex. `commit` locks the mutex, clones the state, applies `f`
  to the clone, `store::save`s the clone to the file, and **swaps the clone into
  memory only if the write succeeded** — the file write is the commit point, so
  memory and file never diverge and a daemon crash can't orphan a pid.

The cores of the five operations are rewritten once as generics:

```rust
fn alloc_with(store: &impl Store, holder: &str, reqs: &[(String, u16)], role: Role)
    -> Result<Vec<(String, u16)>>;     // snapshot -> probe `listening` outside -> commit
fn record_pid_with(store: &impl Store, ...) -> Result<()>;
fn release_with(store: &impl Store, holder: &str, role: Option<Role>) -> Result<Vec<u16>>;
fn snapshot_with(store: &impl Store) -> Result<Data>;   // read; incidental best-effort prune
fn prune_with(store: &impl Store) -> Result<Vec<u16>>;  // probe outside -> commit removals
```

The public facade keeps its current signatures and picks the driver:

```
registry::alloc(...)                      // and the other four
  ├─ via_daemon present?  ── send request ─► devkit-portd handler ─► op_with(&MemoryStore)
  └─ no daemon            ── op_with(&FlockStore)   (identical to today + the gate)
```

`MemoryStore` lives in `devkit-ports::registry` (constructed from an
`Arc<Mutex<Data>>` + the data path) so the daemon binary can build one over its
own state and the same generic ops drive it — the daemon path never calls
`via_daemon`, so it is inherently non-recursive (today's `DEVKIT_PORTD_SELF`
guard becomes unnecessary for these ops).

## Component-by-component changes

**`devkit-common::store`** — expose `load::<D>(path) -> D` and
`save::<D>(path, &D) -> Result<()>` (the read/write that back `with_lock` today)
so the daemon can load once at startup and write through without the data flock.
`with_lock` is unchanged.

**`devkit-ports::registry`**
- Add `trait Store`, `FlockStore`, `MemoryStore`.
- Add a gate helper: `fn acquire_write_gate() -> Result<GateGuard>` — non-blocking
  `try_read` on `portd.lock`; `Err` (with the user-facing message) when a daemon
  holds it exclusive, otherwise an RAII guard held for the op.
- Rewrite the cores of `alloc/record_pid/release/snapshot/prune` as `*_with`.
- Replace the current `via_daemon` ("`None` means flock; errors also `None`")
  with: `try_existing()` → `None` means *no daemon* → run `*_with(&FlockStore)`;
  `Some(client)` → send the request and **propagate** an `Err` response (no silent
  flock fallback behind a live daemon).

**`devkit-portd`**
- `Daemon` gains `ports: Arc<Mutex<Data>>`, populated via `store::load` right
  after it takes `portd.lock` exclusive and before it binds the socket.
- `server.rs` handlers — `Alloc/RecordPid/Release/Snapshot/Prune` and the
  supervision handlers `supervise_app/down/tail` — call `registry::*_with(
  &MemoryStore::new(daemon.ports.clone(), data_path))`, so every handler operates
  on one authoritative in-memory `Data`.

Public facade signatures are unchanged; `portman` and `devrun` need no edits.

## Error handling

- **Write gate refusal is a hard error** for explicit mutations
  (`alloc/record_pid/release/prune`): the process reports the daemon-held message
  and exits non-zero.
- **`snapshot`'s incidental prune is best-effort.** `snapshot_with` reads, and if
  it observes dead entries it *attempts* a prune-commit; a gate refusal there
  (a daemon came up) is **swallowed** and the un-pruned read is returned — a read
  must not hard-fail because it couldn't clean up, and the daemon prunes its own
  copy regardless. Only explicit mutations hard-error on the gate.
- **Write-through failure** leaves memory unchanged and returns the error (the
  clone is not swapped in), preserving file == memory.
- **Live-daemon request error** propagates to the caller (no flock fallback); the
  user retries.

## Invariants preserved (CLAUDE.md "do not break")

- **Reserve before bind** — `alloc_with` commits the pid-less reservation row
  before the caller binds, exactly as `alloc_flock` does today.
- **`record_pid` re-inserts a pruned row** — unchanged `Data::record_pid`.
- **`RESERVATION_GRACE_SECS` (300) > readiness timeout (120s)** — constant
  unchanged; `dead_ports` logic unchanged.
- **`with_lock` holds the exclusive lock for the whole RMW, minimal work** —
  `FlockStore` unchanged; `MemoryStore` holds its mutex only for the in-memory
  RMW + write-through, probing `listening` outside.
- **`down` stops then releases without pruning first** — the `down` handler keeps
  its order, now over `MemoryStore`.

## Testing

- **Store-seam unit tests** against a fileless in-memory `Store`: reserve-before-
  bind, idempotent per-holder alloc, `record_pid` re-insert after prune,
  release/prune — the invariants asserted once at the seam both drivers share.
- **Gate tests** (`FlockStore`): with `portd.lock` held exclusive in-test, a
  direct `commit` returns the daemon-held error; with it free, `commit` succeeds
  and persists; `snapshot` succeeds (ungated) and swallows a gate-blocked prune.
- **Write-through commit-point** (`MemoryStore`): a forced write failure (e.g. an
  unwritable data path) leaves memory unchanged and errors; on success memory ==
  file.
- **Regressions kept green:** the multiprocess flock race test
  (`tests/registry.rs`), the four `devkit-portd` lifecycle tests, and the full
  `alloc/record_pid/release` unit suite.
- **Daemon integration:** with a real daemon up, an `alloc` is reflected in the
  daemon's `snapshot`; an out-of-band direct writer (holding `portd.lock`
  exclusive to simulate a peer daemon) is refused with the hard error.

## Out of scope / future

- **Lock registry (`devkit-locks`).** A follow-up spec applies the same
  `Store`/gate model to the lock registry, which also needs a daemon path built
  from scratch and resolved-context facade variants (its facade resolves the
  project root from CWD and the holder from process identity client-side, so the
  server cannot reuse the high-level functions directly). It will reuse the seam
  defined here and the framework extraction deferred from this spec.
- **Framework extraction into `devkit-common`** (framing/transport/client) is
  deferred to the lock-registry follow-up, where a second daemon consumer makes
  it pay off.
