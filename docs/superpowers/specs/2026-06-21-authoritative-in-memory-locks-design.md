# Authoritative in-memory lock registry (unified `devkitd`) — design

**Goal:** Give the lock registry (`devkit-locks`) the same authoritative
in-memory model the port registry already has, by **folding it into the existing
daemon** rather than building a second one. The daemon holds both registries in
memory, serves lock reads (`check`/`status`) without taking a flock or re-reading
the file, and writes through to `locks.json` on every mutation, with the
sole-writer boundary enforced by the daemon's single-instance lock.

**Scope:** `devkit-locks`, `devkit-common` (daemon-framework extraction), and the
daemon binary (renamed `devkit-portd` → `devkitd`, now serving both registries).
Builds directly on the `Store`/gate model from
`2026-06-21-authoritative-in-memory-portd-design.md`.

**Status:** Approved design. Implementation pending (see the plan).

---

## Background

The port registry now serves reads from the daemon's memory and writes through to
the file, with `portd.lock` (held exclusive for the daemon's life) enforcing the
daemon-vs-direct boundary. The lock registry is still a pure flock participant:
every `lock` invocation takes the exclusive `locks.lock`, re-reads `locks.json`,
mutates, and writes it back.

Two facts shape this follow-up:

1. **Lock ops are already cheap.** Lock liveness is `kill(0)` + TTL arithmetic, no
   blocking call — pruning safely runs inside the flock. So the raw "memory beats
   a slow flock RMW" win that justified the port daemon is small here; the real
   payoff is uniformity and serving high-frequency `check` from memory **when a
   daemon is already up**.
2. **A standalone lock daemon has no keep-alive anchor.** `devkit-portd` stays
   alive because `devrun` supervises dev servers; nothing analogous would keep a
   dedicated `devkit-lockd` running. So instead of a second process, the lock
   registry folds into the daemon that's already kept alive — locks get the
   in-memory path for free whenever that daemon exists, and fall back to the flock
   otherwise.

## Architecture — one process, two registries, two sockets

`devkitd` (the renamed daemon) holds **both** `Arc<Mutex<ports::Data>>` and
`Arc<Mutex<locks::Data>>`, each loaded at startup after it takes `devkitd.lock`
exclusive and before it binds **either** socket. It binds two local sockets — the
existing port socket (`ports.sock`) and a new lock socket (`locks.sock`) — and
runs one accept loop per listener over a shared `Arc<Daemon>`.

`devkitd.lock` (exclusive-for-life, the renamed `portd.lock`) is the **single**
daemon-vs-direct write gate for **both** registries. A direct lock *writer* takes
it shared exactly as a direct port writer does.

**Why two sockets, not one combined proto.** A single socket would need a combined
`Request` enum referencing both `devkit-ports` and `devkit-locks` types; to let
both the `lockm` binary and `portm`/`devrun` construct it, that enum would have to
live in `devkit-common`, forcing `common → ports` and `common → locks`
(backwards). Two sockets keep the two registry crates fully independent: each owns
its own proto, and both consume a generic transport/client/framing from
`devkit-common`. This is the framework extraction the port spec deferred,
realized inside one process — the "second daemon consumer" is the lock proto on
the second socket.

## Naming — mirror ports and locks

The daemon now serves both registries, so its ports-specific name and the
asymmetric CLI names are corrected to a mirrored scheme:

| | Port side | Lock side |
|---|---|---|
| CLI binary | `portman` → `portm` | `lock` → `lockm` |
| Daemon | one process: `devkit-portd` → `devkitd` | (shared) |
| Socket file | `ports.sock` | `locks.sock` |
| Dispatch module | `src/bin/devkitd/port_server.rs` | `src/bin/devkitd/lock_server.rs` |
| Client module | `devkit-ports::daemon` | `devkit-locks::daemon` |
| Shared framework | `devkit-common::daemon` | (shared) |
| Single-instance lock | `devkitd.lock` (was `portd.lock`) | (shared gate) |
| Env markers | `DEVKITD_SELF` (was `DEVKIT_PORTD_SELF`), `DEVKITD_BIN` (was `DEVKIT_PORTD_BIN`) | (shared) |

`devrun` and `issue` are unchanged. The rename is mechanical but wide: binary
names, `Cargo.toml` `[[bin]]` targets, the `daemon` feature-gated bin, the
`completions` subcommands, `paths` accessors, env-var reads, README, and the
`docs/next-steps.md` external-caller note all move in lockstep. The wire format
and behavior are unchanged by the rename.

## The `Store` seam in `devkit-locks`

Mirror `devkit-ports::registry`: the invariant-bearing logic already lives in
`Data` methods (`prune_dead`, `check`, `try_acquire`, `do_release`, `release_all`).
Wrap *driving* them behind the same seam so each body runs identically under flock
or in memory.

```rust
trait Store {
    fn snapshot(&self) -> Result<Data>;
    fn commit<T>(&self, f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T>;
}
```

- **`FlockStore`** — `snapshot` reads `locks.json` (ungated). `commit` acquires the
  shared `devkitd.lock` gate, then runs `store::with_lock(locks_lock, locks_file, f)`
  (data-flock RMW + atomic write), then releases the gate.
- **`MemoryStore { state: Arc<Mutex<Data>>, data_path }`** — `snapshot` clones under
  the mutex. `commit` locks the mutex, clones, applies `f`, `store::save`s the
  clone, and swaps it into memory **only if the write succeeded** (file write is
  the commit point; memory and file never diverge).

The op cores become generics; the public facade keeps its signatures:

```rust
fn acquire_with(s: &impl Store, root: &str, holder: &str, paths: &[String],
                pid: Option<u32>, note: Option<&str>, ttl: u64, now: u64) -> Result<AcquireOutcome>;
fn check_with(s: &impl Store, root: &str, holder: &str, paths: &[String], now: u64) -> Result<Vec<Conflict>>;
fn release_with(s: &impl Store, root: &str, holder: &str, paths: &[String], force: bool) -> Result<(Vec<String>, Vec<String>)>;
fn release_all_with(s: &impl Store, root: &str, holder: &str) -> Result<Vec<String>>;
fn status_with(s: &impl Store, root: &str, all: bool, now: u64) -> Result<Vec<LockEntry>>;
fn prune_with(s: &impl Store, now: u64) -> Result<usize>;
```

## Resolved-context split — the lock-specific difference

Unlike the port handlers (which receive an already-resolved `holder`), the lock
facade resolves context from the **calling** process, which a daemon cannot see:
`ctx()` reads `current_dir()` (→ project root), the `--as`/`DEVKIT_SESSION`/
`TMUX_PANE`/tty/ppid precedence (→ `holder`), and normalizes the path arguments;
`anchor_pid()` reads tmux/tty/ppid (→ `pid`).

So the facade always resolves context **client-side**, then picks a driver:

```
devkit_locks::acquire(paths, as, note, ttl)
  ├─ ctx() resolves (root, holder, normalized paths); anchor_pid() resolves pid
  └─ via_daemon:
       try_existing(locks.sock) → Some(client): send Acquire{root,holder,paths,pid,note,ttl}
                                                  daemon stamps `now`, runs Data ops on MemoryStore
       try_existing(locks.sock) → None:          acquire_with(&FlockStore, …, now())
```

The daemon receives resolved context and stamps its own `now` (single clock); it
never re-resolves CWD, identity, or paths.

## Wire protocol (`devkit-locks::daemon::proto`)

Mirrors the lib facade 1:1, payloads carrying resolved context:

```rust
enum Request {
    Ping { proto: u32 },
    Acquire   { root: String, holder: String, paths: Vec<String>, pid: Option<u32>, note: Option<String>, ttl: u64 },
    Check     { root: String, holder: String, paths: Vec<String> },
    Release   { root: String, holder: String, paths: Vec<String>, force: bool },
    ReleaseAll{ root: String, holder: String },
    Status    { root: String, all: bool },
    Prune,
}
enum Response {
    Pong { proto: u32, pid: u32 },
    Acquired(AcquireOutcome),
    Conflicts(Vec<Conflict>),
    Released { released: Vec<String>, refused: Vec<String> },
    Freed(Vec<String>),
    Locks(Vec<LockEntry>),
    Pruned(usize),
    Ok,
    Err(String),
}
```

The lock proto has its own `PROTO` version, independent of the port proto.

## The extracted framework (`devkit-common::daemon`)

Move the registry-agnostic plumbing out of `devkit-ports::daemon` so both crates
consume it:

- `framing.rs` — `send`/`recv` (already generic over `Serialize`/`DeserializeOwned`),
  moved verbatim.
- `transport.rs` — `socket_name(path)` generalized: the Windows named-pipe name is
  folded from the full `path` (drop the hardcoded `devkit-portd-` literal) so two
  distinct sockets in one state dir map to two distinct pipes.
- `client.rs` — generic `Client<Req, Resp>` (`from_stream` + `Ping`/`Pong`
  handshake, `request`), plus `try_existing(socket_path, proto)` and
  `ensure_running(socket_path, bin)`. The proto version and binary name are passed
  in by each consumer.

`devkit-ports::daemon` keeps its port `Request`/`Response` and becomes a thin
binding of the common machinery to `ports.sock` + the `devkitd` binary — no
wire-format or behavior change. `devkit-locks::daemon` is the parallel binding to
`locks.sock`, `try_existing` only.

## Component-by-component changes

**`devkit-common`**
- New `daemon` module (framing/transport/client) per above.
- `paths`: add `lock_socket_file()`; rename the daemon lock accessor to
  `devkitd_lock()` (was `daemon_lock_file()`/`portd.lock`) and the port socket
  accessor symmetrically; both registries share `devkitd_lock()`.

**`devkit-ports`** — rebind `daemon::{client,transport}` onto `devkit-common::daemon`;
`registry`'s gate now reads `paths::devkitd_lock()`. No behavior change.

**`devkit-locks`**
- `store.rs`: add `Store`, `FlockStore` (shared `devkitd.lock` gate + `with_lock`),
  `MemoryStore` (write-through). Keep `with_lock`.
- `lib.rs`: rewrite the six op cores as `*_with`; the public fns resolve context
  via `ctx()` and pick the driver via a `via_daemon` split (`try_existing`,
  never autostart).
- `daemon/proto.rs` + `daemon/client.rs`: lock proto and the thin lock-socket
  client.

**`devkitd` binary** (`src/bin/devkitd/`, renamed from `devkit-portd`)
- `Daemon` gains `locks: Arc<Mutex<locks::Data>>`, loaded right after the port
  load (still before any bind).
- Bind `locks.sock` alongside `ports.sock`; a sibling accept thread serves lock
  connections. Idle-shutdown wakes **both** listeners (connect to both sockets).
- `lock_server.rs` dispatches lock requests against
  `MemoryStore::new(daemon.locks.clone(), paths::locks_file())` via `locks::*_with`.

`lockm`/`portm` keep their CLI surface (only the binary names change); no facade
signature changes.

## Error handling (mirrors the port spec)

- **Write-gate refusal is hard** for `acquire/release/release_all/prune`:
  `FlockStore::commit`'s non-blocking `try_read` on `devkitd.lock` fails ⇒ the
  daemon-held message (*"a devkitd daemon holds the registry lock; refusing to
  modify `locks.json` behind it — stop the daemon or use a daemon-enabled
  binary."*), exit non-zero. This is the safety net even for a daemon-unaware
  build.
- **`check`/`status` are ungated reads.** Any incidental prune they attempt
  swallows a gate refusal and returns the un-pruned view (the daemon prunes its own
  copy regardless).
- **Write-through failure** leaves memory unchanged and returns the error (clone
  not swapped) — file == memory always.
- **Live-daemon request error propagates** (no silent flock fallback behind a live
  daemon).

## Invariants preserved

- **All-or-nothing acquire, idempotent per-holder renew, overlap/ancestor conflict
  rules, force-release, release_all scoping, TTL/dead-pid prune** — all unchanged
  `Data` methods, now driven through the seam.
- **Cheap-prune-inside-the-lock** stays true: lock liveness has no blocking call,
  so `FlockStore::commit` prunes within the flock exactly as today.
- **`devkitd.lock` exclusive-for-life** and the shared-gate boundary behave
  identically to the port spec; the rename is cosmetic.

## Testing (TDD)

- **Store-seam unit tests** against a fileless in-memory `Store`: all-or-nothing
  acquire, idempotent renew, overlap conflict, release/force, release_all scoping,
  TTL/dead-pid prune — the `Data` invariants asserted once at the seam both drivers
  share.
- **Gate tests** (`FlockStore`): with `devkitd.lock` held exclusive in-test, a
  direct `commit` returns the daemon-held error; free ⇒ commit persists;
  `check`/`status` succeed ungated and swallow a gate-blocked prune.
- **Write-through commit-point** (`MemoryStore`): a forced unwritable data path
  leaves memory unchanged and errors; on success memory == file.
- **Framework-extraction regression**: existing `devkit-ports` proto and the four
  `devkit-portd` lifecycle tests stay green on the relocated
  `devkit-common::daemon` framing/transport/client and the renamed binary.
- **Daemon integration**: with a real `devkitd` up, a client `acquire` is visible
  in the daemon's `check`/`status`; an out-of-band direct writer (holding
  `devkitd.lock` exclusive to simulate a peer) is refused with the hard error;
  after idle-exit, flock fallback reads the written-through `locks.json`.

## Out of scope / future

- **MCP server for devkit** (tracked in `docs/next-steps.md`). An MCP host is a
  natural long-lived process that could keep these in-memory registries warm; its
  relationship to `devkitd` is its own brainstorming pass.
- **Supervision for locks.** Locks need no restart/idle semantics; the daemon's
  existing supervision is untouched and holding locks does not keep it alive
  (write-through makes idle-exit safe).
