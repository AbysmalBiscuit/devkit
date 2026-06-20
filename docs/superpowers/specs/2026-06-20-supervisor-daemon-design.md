# Supervisor Daemon Design

**Status:** approved design, pre-implementation
**Date:** 2026-06-20

**Goal:** An optional, opt-in `devkit-portd` daemon that owns running dev-server
processes — restarting them on crash, exposing their logs, and keeping the port
registry live — while serving registry operations over a unix socket. Default
behavior is unchanged: with no daemon, every binary runs exactly as today.

**Architecture:** The daemon is a *flock participant, never a flock replacement*.
It performs every `ports.json` mutation through the existing `registry::with_lock`,
so a daemon-aware binary and a daemon-unaware one coexist over the same file
without corruption. Supervision — owning live child processes — is the one
responsibility that cannot live in a file, and is the sole reason the daemon
exists. Registry RPC comes along for free because the supervisor already knows
what is running where.

**Tech stack:** Rust (edition 2024), unix domain socket, line-delimited JSON
(`serde_json`), `fd-lock` for the single-instance lock, `nix` for `waitpid`/signals.
No async runtime — a thread-per-connection accept loop plus a supervision thread.

---

## 1. Why a daemon at all (scope guard)

A daemon that only manages `ports.json` is pure overhead: `flock` already
serializes the tiny read-modify-write correctly and sub-millisecond, and
correctness forces `ports.json` to remain the source of truth anyway (to survive
daemon crash, idle-exit, and version skew). Caching a file that is already cheap
to read buys nothing.

The daemon earns its keep only by holding state a file cannot: **live child
process handles**. That enables:

- **restart-on-crash** — respawn a dev server that exits unexpectedly;
- **continuous health** — the readiness probe on a loop (optional, see §7);
- **log access** — `logs`/`tail` served without the caller knowing paths;
- **a live in-memory registry** — as a *side effect* of owning the processes.

Everything else (plain `alloc`/`release`/`status`) keeps working with no daemon.

## 2. Two gates: compiled-in, then turned-on

Optionality is enforced at two independent layers, so the default build and the
default runtime are byte-for-byte today's behavior.

- **Build gate.** `devkit-ports` gains a `daemon` Cargo feature that compiles in
  the client-side dispatch and the IPC types (`src/daemon/`). With the feature
  off, none of that code ships and the facade is flock-only. The `devkit-portd`
  binary is its own crate, built only when wanted.
- **Run gate.** Even with the feature compiled, the daemon is **autostarted only
  when explicitly enabled**: `[daemon] enabled = true` in config, or
  `DEVKIT_DAEMON=1`, or `devrun up --supervise`. Registry-only operations never
  start a daemon; they *use* one if it already happens to be running, but
  supervision is the only thing that brings a daemon up.

## 3. Components & files

| Unit | Responsibility |
|---|---|
| `crates/devkit-portd/` (new bin) | daemon: lock + socket bind, accept loop, idle timer, panic hook |
| `devkit-portd/src/server.rs` | accept connections, decode requests, dispatch to handlers |
| `devkit-portd/src/supervisor.rs` | child table, spawn/reap/restart/backoff, adoption |
| `devkit-ports/src/daemon/proto.rs` (feature-gated) | `Request`/`Response`/handshake types, framing |
| `devkit-ports/src/daemon/client.rs` (feature-gated) | connect, handshake, autostart, RPC, fallback |
| `devkit-common/src/supervise.rs` (moved) | `spawn_detached`/`wait_ready`/`stop`/`tail` shared by devrun + daemon |
| `devkit-ports/src/registry.rs` (edit) | facade fns dispatch daemon-first, else flock |
| `devkit-ports/src/config.rs` (edit) | `[daemon]` section |
| `devkit-common/src/paths.rs` (edit) | `socket_file()`, `daemon_lock_file()`, `daemon_log()` |
| `devrun/src/main.rs` (edit) | `up --supervise`; route `down`/`logs` through daemon when up |

**Prerequisite refactor.** `devrun/src/supervise.rs` moves to
`devkit-common/src/supervise.rs` unchanged, so both `devrun` and the daemon share
`spawn_detached`/`wait_ready`/`stop`/`tail`. `devrun` re-imports from common.

## 4. Transport & protocol

- **Socket:** `paths::socket_file()` = `~/.claude/state/devkit/portd.sock`.
- **Framing:** one JSON object per line (newline-delimited). One request line,
  one response line, then the connection may be reused or closed. Dead simple,
  no length-prefix bookkeeping.
- **Handshake:** first frame on every connection is `Ping { proto: u32 }`. The
  daemon replies `Pong { proto: u32, pid: u32 }`. `proto` is an integer bumped on
  any wire-incompatible change.

```
Request  = Ping{proto}
         | Alloc{holder, reqs:[(app,base)], role}
         | RecordPid{port, app, holder, role, pid, logfile}
         | Release{holder, role:Option}
         | Snapshot
         | Prune
         | Supervise{holder, app, role, argv, cwd, env, logfile}   // spawn + own
         | Down{holder, role:Option}                                // stop + release, no restart
         | Tail{holder, app, role:Option, lines}
         | Shutdown                                                 // graceful, for upgrade
Response = Pong{proto, pid}
         | Ports[(app,port)] | Data{...} | Freed[port] | Lines(String) | Ok | Err(String)
```

The registry requests (`Alloc`/`RecordPid`/`Release`/`Snapshot`/`Prune`) map
1:1 to today's facade functions; the daemon implements them by calling the very
same `registry::with_lock`-based code. The supervision requests
(`Supervise`/`Down`/`Tail`) are daemon-only.

## 5. Lifecycle: autostart, single-instance, idle-exit

**Client connect logic** (`daemon::client`):

1. Try to connect to the socket.
2. **Connected** → send `Ping`. If `proto` matches → use this daemon. If it
   **mismatches** (old daemon after a binary upgrade) → send `Shutdown`, wait for
   the socket to disappear (bounded), then go to step 3 to start a fresh one.
3. **Not connected** → if the run gate is on, **autostart**: fork-exec
   `devkit-portd`, wait for the socket to accept (bounded poll), connect. If the
   run gate is off, return `NotAvailable` (caller falls back to flock).

**Daemon startup** (resolves stale-socket and race in one move):

1. Acquire an exclusive `fd-lock` on `daemon_lock_file()`
   (`portd.lock` — *separate* from the registry's `ports.lock`). If it can't, a
   daemon is already running → exit 0. This makes autostart races safe: two
   clients may both spawn a daemon; exactly one wins the lock, the other exits.
2. Holding the lock, `unlink` any stale `portd.sock`, then `bind`. Stale-socket
   ambiguity is gone because the lock guarantees no live daemon owns it.
3. Adopt survivors (§8), start the supervision thread, start the accept loop.

**Idle-exit.** The daemon exits after `daemon.idle_timeout_secs` (default 1800)
with **zero connected clients AND zero supervised children**. Idle-exit is
*suppressed while supervising* — orphaning live servers it is meant to watch
would defeat the point. Next request autostarts a fresh daemon.

## 6. The stop/restart coordination (correctness core)

This is the subtle part: a restart policy must never fight a stop.

**Restart rule:** when a supervised child exits, restart it **only if** its row
is still present in `ports.json` and it is not marked stopping. The shared
`ports.json` row *is* the cross-tool signal — no extra IPC needed for a
daemon-unaware tool to say "stop."

- **Child crashes.** `waitpid` reaps it; row still present → respawn, `record_pid`
  with the new pid. Restart uses exponential backoff with a crash-loop guard:
  at most `max_restarts` within `restart_window_secs`, then give up, leave the
  row with `pid=None`, and log. Never respawn an instantly-crashing server
  forever.
- **`Down` via daemon.** Daemon-aware `devrun down` routes a `Down` RPC. The
  daemon marks the child stopping, SIGTERMs, reaps, releases the row — atomically
  from the restart thread's view, so no restart fires.
- **Legacy daemon-unaware `down`.** Only possible if a daemon is up but an old
  binary runs the flock `down` path. It SIGTERMs then releases the row. To cover
  the tiny window where the daemon could observe the child dead before the row is
  released, the restart path **re-checks the row after a short debounce** before
  respawning. Documented as a known minor edge for mixed-version concurrent
  `down`; daemon-aware binaries never hit it because they route through `Down`.

Liveness stays pid-based: supervised children are recorded with their real pid
(`record_pid`), so a daemon-unaware `snapshot`/`prune` in another terminal sees
them correctly and leaves live pids alone.

## 6a. Memory tracking & soft warn cap

Supervised dev servers (Next/Turbopack especially) can balloon to 8–20 GB. The
daemon already owns the process and runs a supervision loop, so memory is its job
to watch.

- **Tracking (always on).** On each supervision tick the daemon sums RSS across
  each server's **process tree** — the recorded child plus every descendant —
  because the dev server forks worker processes and the direct child's RSS
  undercounts. RSS is read from `/proc/<pid>` (no privilege needed). The total is
  surfaced as a `MEM` column in `status` output and logged periodically to
  `portd.log`.
- **Soft cap (warn).** Two thresholds, both off when `0`:
  - past `memory_warn_mb`, the daemon logs a loud line naming the server and its
    current tree RSS;
  - past `memory_limit_mb`, it takes `memory_action`. For v1 the only action is
    `"warn"` (a louder, rate-limited log line). `"restart"` is specified but
    **deferred** — see `docs/next-features.md`.
- **Shared restart budget (when `restart` lands).** A memory-triggered restart
  must count against the same crash-loop guard as a crash restart
  (`max_restarts` / `restart_window_secs`); a server that re-balloons immediately
  must not be restart-looped forever — exhaust the budget and fall back to warn.

Hard enforcement (a per-server cgroup-v2 memory cap at spawn) is **out of scope
for v1** and tracked as a delegated feature in `docs/next-features.md`. The
poll-based warn here works everywhere, including WSL2 without cgroup delegation.

## 7. Health probing (optional, phase 2)

Core supervision is **exit-based** (waitpid). A follow-on can add a periodic
TCP readiness probe (reusing `supervise::wait_ready`'s connect logic): if a
server that was once ready refuses connections for K consecutive probes, treat it
as hung and restart it. Kept out of the core scope to avoid false-positive
restarts; the design leaves room for it (the supervision thread already loops).

## 8. Crash & upgrade resilience (adoption)

Children are spawned `setsid`-detached (as today) **and** kept as the daemon's
direct children, so `waitpid` works while the daemon lives, and the servers
survive the daemon dying (they reparent to init).

When a fresh daemon starts (after crash, upgrade, or a suppressed idle-exit it
never took), it **adopts** survivors: read `ports.json`, and for each row with a
live pid that it did not spawn, monitor via `pid_alive` polling instead of
`waitpid` (you cannot `waitpid` a non-child). The restart rule is unchanged
("pid dead + row present → restart"); a *restarted* child becomes a real child
again and reverts to `waitpid`. This is the asymmetry: **own → waitpid, adopt →
poll.**

## 9. Client fallback is safe because the facade is idempotent

If a daemon RPC fails mid-call (connection drops after the request landed), the
client falls back to the flock path. That can't double-execute harmfully because
every facade op is idempotent:

- `alloc` is idempotent per `(holder, app, role)` (returns the existing port);
- `release` removing an absent row is a no-op;
- `record_pid` is an upsert.

So: `NotAvailable` → silent flock fallback; any other daemon error → log with
context, then retry on flock. No double-alloc, no orphaned rows.

## 10. Error handling & reporting

- The daemon installs `report::install_panic_hook("devkit-portd")` and logs to
  `paths::daemon_log()` = `~/.claude/state/devkit/logs/portd.log`. `anyhow`
  context chains throughout; `RUST_BACKTRACE=1` adds backtraces, as elsewhere.
- Daemon *unavailability* is never an error on the client (it's the default).
- A child that exhausts its restart budget is reported in `status` (pid `-`) and
  logged, not silently dropped.

## 11. Config

```toml
[daemon]
enabled            = false   # run gate; autostart only when true (or --supervise / DEVKIT_DAEMON=1)
idle_timeout_secs  = 1800    # exit when idle AND supervising nothing
max_restarts       = 5       # crash-loop guard: restarts allowed within the window
restart_window_secs = 60
memory_warn_mb     = 6000    # log a loud line past this tree-RSS (0 = off)
memory_limit_mb    = 12000   # take memory_action past this tree-RSS (0 = off)
memory_action      = "warn"  # "warn" only in v1; "restart" deferred (next-features.md)
```

All fields `#[serde(default)]` so existing configs keep working untouched.

## 12. Testing strategy

- **Backend parity.** Run the registry operation suite against *both* backends:
  a harness spins up a real `devkit-portd` on a temp `HOME`/state dir + temp
  socket, drives `alloc`/`release`/`snapshot`/`prune` through the client, and
  asserts results identical to the flock path. This is what keeps the two paths
  from drifting (the explicit cost of optionality).
- **Single-instance race.** Two clients autostart concurrently → exactly one
  daemon survives (`portd.lock`), mirroring the existing multiprocess flock test.
- **Restart-on-crash.** Supervise a child that exits → asserted respawn; a
  crash-looping child → backoff then give-up with `pid=None` + log.
- **Stop coordination.** `Down` RPC → no restart; external flock `down` removing
  the row → daemon does not respawn (debounce re-check).
- **Adoption.** Pre-seed `ports.json` with a live non-child pid → new daemon
  monitors it by poll; kill it → restart fires.
- **Idle-exit.** Daemon with zero children and no clients exits after the
  (test-shortened) timeout; suppressed while a child is supervised.
- **Handshake/version skew.** `Ping` proto mismatch → client decides to
  `Shutdown` + respawn (unit-testable decision).
- **Memory tracking.** Supervise a child that forks a worker; assert the reported
  tree-RSS includes the worker (sum over the process tree, not just the child),
  and that crossing `memory_warn_mb` emits exactly one warn line per breach.

## 13. Out of scope (YAGNI) — see `docs/next-features.md` for deferred features

- Cross-machine / TCP transport — unix socket only.
- Multi-user / shared-daemon — one daemon per `HOME`/state dir.
- A registry-only daemon with no supervision — rejected in §1.
- Health-probe restarts — deferred to phase 2 (§7).
- `memory_action = "restart"` — v1 warns only (§6a).
- Hard cgroup-v2 memory cap at spawn — delegated (§6a, `next-features.md`).

## 14. Resolved decisions

1. **Idle timeout** — 1800s (30 min), counted only with zero callers AND zero
   supervised children. Suppressed while supervising.
2. **Legacy `down` race** — Option A (in-daemon debounce re-check, §6). The
   `down` "stops then releases" invariant in CLAUDE.md is left untouched.
3. **Plain `devrun status`** — never autostarts a daemon; uses one if already up,
   else flock.
4. **Memory** — track tree-RSS always; soft `warn` cap in v1; hard cgroup cap
   delegated to `next-features.md`.
