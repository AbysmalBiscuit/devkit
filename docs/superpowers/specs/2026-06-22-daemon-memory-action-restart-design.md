# Daemon `memory_action = "restart"` — Design

**Status:** approved 2026-06-22. Implements the daemon phase-3 follow-on and
resolves the "`memory_action = "restart"`" entry in `docs/next-features.md`.
Builds directly on the phase-2 health-probe design
(`docs/superpowers/specs/2026-06-21-daemon-health-probe-restarts-design.md`)
and reuses its crash-path-restart invariant.

**Goal:** restart a supervised dev server whose process-tree RSS stays over
`memory_limit_mb` for several consecutive supervision ticks — through the
existing crash path, within the crash-loop budget — and fall back to
warn-and-leave-alive once that budget is exhausted, so a runaway server is
rate-limited but a transient allocation spike is never acted on.

---

## 1. Problem

`memory_limit_mb` and `memory_action` are already config fields, but they are
inert: `memory_breaches` only checks `mem_warn`, `mem_limit()` is an accessor
nobody reads, and `memory_action` is never consulted. The only memory feedback
the daemon gives is the `mem_warn` log line. A server that leaks past a hard
ceiling — degrading the developer's whole machine — is observed but never
acted on.

Phase 2 solved the analogous "hung but alive" case for health: a server the
exit-based reaper can't see is SIGTERM'd and funnelled into the crash path.
Memory is the same shape — the process is alive, so `reap_once` never returns
it — and takes the same remedy.

## 2. Approach

The memory action runs **inside the existing 500 ms supervision tick**, not a
new thread. Reading tree-RSS is a cheap local `sys` call (it already runs
there for `mem_warn`), unlike the health probe's 300 ms blocking connect that
justified its own thread. Each tick, for every owned, live child, the
supervisor reads tree-RSS and maintains a consecutive-breach counter. When the
counter reaches `memory_limit_ticks` *and* the configured action is
`"restart"`, the supervisor decides per child:

- **crash-loop budget remains** → return the pid to `SIGTERM`. The dying
  process is collected by the same tick's `reap_once` on a later cycle and
  respawned by `restart` within the budget — identical to a crash and to the
  health-probe restart.
- **budget exhausted** → emit a one-shot "leaving alive" warning and do
  nothing else. The leaky-but-working server keeps running.

Because the action lives in the one supervision thread, there is no second
mutator and no probe-vs-reap race to reason about (§7). The new surface is:
two `Child` fields, one read-only budget peek, one tick accessor, one config
knob plus the `memory_action` env read, and the docs escape-hatch note.

This honors — and extends — the AGENTS invariant established in phase 2: *a
non-crash restart goes through the crash path, not its own respawn.* The
memory path only ever SIGTERMs; it never calls `restart` or respawns.

### Decisions (resolved during brainstorming)

- **Debounce — require `N` consecutive ticks over the limit.** A single
  reading over `memory_limit_mb` can be a transient, GC-able spike. Acting
  only after `memory_limit_ticks` (default 3 ≈ 1.5 s at the 500 ms tick)
  consecutive breaches mirrors the health probe's consecutive-failure gate.
  Rejected: act on first crossing (a normal GC peak just under the ceiling
  would restart-loop a healthy server).
- **Budget exhaustion — leave alive and warn, not drop.** The peek happens
  *before* the kill: while restarts remain, SIGTERM and let the reap path
  respawn (budgeted); once the budget is exhausted, stop killing and log a
  warn line, leaving the over-limit server running. This matches the written
  *"exhaust the budget and fall back to warn"* constraint and keeps the
  developer's server available rather than vanished. Rejected: health-probe
  parity (SIGTERM unconditionally, let `restart` drop it when exhausted) —
  identical mechanism, but leaves a runaway dev server dead instead of
  warned-and-alive. The crash-loop budget is a sliding window, so once the
  window cools down the server becomes restartable again — give-up is
  transient, tied to the current budget, not a permanent ban.
- **No daemon-side throttle-without-kill.** SIGTERM-then-respawn is the
  daemon's only *portable* lever. cgroup v2 `memory.high` (throttle, no kill)
  is Linux-only; macOS has no equivalent and Windows job objects fail
  allocations (≈ crash). devkit builds and tests on all three and funnels
  platform differences through one `sys` boundary, so a Linux-only throttle is
  rejected. Runtime caps (e.g. Node `--max-old-space-size`) are runtime-
  specific — baking one in violates devkit's project-agnostic stance — and
  don't avoid a restart anyway: the runtime aborts on cap and the existing
  crash path respawns it. They remain available to users as a docs escape
  hatch (§8), set through the app's existing launch `env`.
- **Single global knobs, gated on opt-in.** One `memory_limit_mb`, one
  `memory_limit_ticks`, one `memory_action`. Default `memory_action = "warn"`
  and `memory_limit_mb = 0` keep the feature off until opted into, matching
  every other daemon knob.

## 3. State changes

### 3.1 `Child` (src/bin/devkitd/supervisor.rs)

Two fields added (alongside `warned_mem`, `armed`, `probe_failures`):

```rust
struct Child {
    // ...existing fields...
    /// Consecutive supervision ticks this child's tree-RSS has been at or over
    /// `mem_limit`. Reset to 0 when it drops below, or when a memory action is
    /// decided for it this tick.
    mem_over: u32,
    /// Has the "budget exhausted, leaving over-limit server alive" warning
    /// already fired for the current breach episode? Re-armed (false) when the
    /// child drops back below the limit or is respawned. Edge-triggers that
    /// warning the same way `warned_mem` edge-triggers the warn-threshold line.
    mem_gave_up: bool,
}
```

- `insert_owned` / `insert_adopted` initialise both to `0` / `false`.
- **`set_pid` resets both to `0` / `false`** (it already resets `armed` /
  `probe_failures`). A respawned process starts its breach count clean, and a
  successful respawn clears any prior give-up so a later exhaustion warns
  again.

### 3.2 `Supervisor` — budget peek

`may_restart` records an attempt against the budget. The memory path needs to
*ask* whether a restart is allowed without consuming a slot, because the
authoritative record stays in `restart()` on the reap tick (the sole budget
recorder, single-threaded → peek-then-record is consistent):

```rust
/// Whether a restart is currently allowed for `key` under the crash-loop
/// budget, WITHOUT recording one. Prunes timestamps outside the window (a
/// harmless, idempotent cleanup) but never pushes. Unknown key → false, like
/// `may_restart`. The recording counterpart is `may_restart`, called from the
/// reap path.
pub(crate) fn can_restart(&mut self, holder: &str, app: &str, role: Role) -> bool;
```

### 3.3 `Supervisor` — the per-tick memory action

```rust
/// One memory-limit decision for a child this tick.
pub(crate) enum MemAction {
    /// Budget remains: SIGTERM this pid; the reap tick respawns it.
    Restart { key: Key, pid: u32, rss: u64 },
    /// Budget exhausted: warn once and leave the over-limit server running.
    GiveUp { key: Key, rss: u64 },
}

/// Advance every owned, live child's consecutive-breach counter against
/// `mem_limit` and return the memory actions to take this tick. Each child's
/// tree-RSS is read once. A child whose RSS is below `mem_limit` (or whose
/// `mem_limit` is 0) has its counter and give-up flag cleared and yields no
/// action. A child at or over the limit for `limit_ticks` consecutive ticks
/// yields exactly one action: `Restart` if `can_restart` allows, else `GiveUp`
/// the first time per episode (suppressed by `mem_gave_up` afterwards). The
/// counter resets to 0 on any decision so it re-checks roughly every
/// `limit_ticks` ticks while still over the limit — picking the server back up
/// once the budget window cools down. Returns empty when `mem_limit == 0`.
pub(crate) fn mem_limit_actions(&mut self, limit_ticks: u32) -> Vec<MemAction>;
```

Eligibility matches `probe_targets`: `launch.is_some()` (owned, respawnable)
and `pid != 0`. Adopted survivors and pid-less reservations are skipped — a
SIGTERM with no launch spec would leave a dead port with nothing to respawn.

Implementation note (for the plan, not a contract): the method runs two
passes to satisfy the borrow checker — first an `iter_mut` pass that reads
RSS, updates each `mem_over`/`mem_gave_up`, and collects over-threshold
candidates; then a second pass that calls `can_restart` per candidate
(needs `&mut self` for its prune), builds the `MemAction`, and resets the
candidate's `mem_over`.

`memory_breaches` (the `mem_warn` warn line) is left unchanged and continues to
run every tick regardless of `memory_action`. Warn and limit are independent
thresholds; both read tree-RSS once per child per tick — an accepted cost at
dev scale (a handful of children).

## 4. Supervision-tick integration (src/bin/devkitd/main.rs)

In the existing combined supervision thread, after the `memory_breaches` warn
loop (`main.rs:230-238`), add the limit-action loop, gated on the opt-in:

```text
// existing: reap_once → restart(); memory_breaches() warn loop ...

if mem_restart {                                  // memory_action == "restart"
    for action in d.sup.lock().unwrap().mem_limit_actions(mem_limit_ticks) {
        match action {
            Restart { key, pid, rss } => {
                log_line("memory: {holder}/{app} ({role}) tree-RSS {MB} MB over
                          limit — restarting")
                supervise::stop(pid)              // SIGTERM; reap tick respawns
            }
            GiveUp { key, rss } => {
                log_line("memory: {holder}/{app} ({role}) tree-RSS {MB} MB over
                          limit but crash-loop budget exhausted — leaving alive")
            }
        }
    }
}
```

- The `mem_limit_actions` lock is taken once per tick and released before any
  `supervise::stop`, matching the bound-`let` discipline the reap and probe
  loops already use.
- `supervise::stop` on an already-exited pid is a no-op (same as the probe
  path), so a child that dies between the verdict and the signal is safe.
- The decision is made and acted on in the same thread that reaps, so the
  SIGTERM'd child is respawned by a later cycle of this same loop. No second
  mutator of restart state exists.

## 5. Config / knobs

The daemon binary reads `DEVKIT_DAEMON_*` env vars directly (the existing
pattern); `DaemonConfig` mirrors the fields for completeness and default-tests.

| Env var | `DaemonConfig` field | Default | Meaning |
|---|---|---|---|
| `DEVKIT_DAEMON_MEMORY_ACTION` | `memory_action: String` | `"warn"` | Action on crossing `memory_limit_mb`: `"warn"` (log only — existing) or `"restart"`. Any other value falls back to warn. |
| `DEVKIT_DAEMON_MEM_LIMIT_MB` | `memory_limit_mb: u64` | `0` | Tree-RSS in MB that triggers `memory_action` (`0` = off). Already read into `mem_limit`; becomes live. |
| `DEVKIT_DAEMON_MEM_LIMIT_TICKS` | `memory_limit_ticks: u32` | `3` | Consecutive 500 ms supervision ticks at or over the limit before acting. |

`memory_action` is read in `main` as
`std::env::var("DEVKIT_DAEMON_MEMORY_ACTION").unwrap_or_else(|_| "warn".into())`;
`mem_restart` is `action == "restart"`. `memory_limit_ticks` is read with the
existing `env_u32("DEVKIT_DAEMON_MEM_LIMIT_TICKS", 3)`. `Supervisor::new` is
unchanged — the action and tick count are owned by `main` (the tick loop only
calls `mem_limit_actions` when `mem_restart`), mirroring how `main` only spawns
the probe thread when probing is enabled.

`DaemonConfig::default()` gains `memory_limit_ticks: 3`, and the
`daemon_defaults_when_absent` test gains an assertion for it. The
`memory_action` comment is updated to say `"restart"` is now honored.

### 5.1 Misconfiguration warning

A `memory_limit_mb` at or below `memory_warn_mb` makes the warn threshold
redundant (the limit fires first or together). When `mem_restart` is enabled,
`mem_limit > 0`, `mem_warn > 0`, and `mem_limit <= mem_warn`, `main` logs one
startup warning so the operator notices the inverted thresholds:

```text
memory: limit (N MB) is at or below warn (M MB) — warn threshold is redundant
```

It is advisory only; both thresholds still fire independently.

## 6. Restart path (unchanged, reused)

After SIGTERM the child exits; the 500 ms tick's `reap_once` returns its key;
`restart` respawns it within the crash-loop budget, recording the attempt via
`may_restart`. A server that re-balloons restarts again until `may_restart`
exhausts the window — at which point the *next* `mem_limit_actions` tick sees
`can_restart` return false and emits `GiveUp` instead of `Restart`. No new
restart logic is added; a leaky server is funnelled into the crash path by a
signal, exactly as a hung one is.

## 7. Concurrency correctness

- The memory action runs in the **same** thread as `reap_once`/`restart`, so
  there is no second mutator of restart state and no probe-style two-thread
  race. The peek (`can_restart`) and the eventual record (`may_restart` in
  `restart`) both run on this one thread, so the budget is counted once per
  restart, never doubled.
- A leaking server is *alive*; `reap_once` never returns it. Only the memory
  action acts on it until SIGTERM; after it exits, only the reap path acts on
  it. The two never touch the same child in the same state.
- `mem_limit_actions` reads tree-RSS and takes the `sup` lock for the whole
  call, but the call is a bounded local computation (a `sys` RSS read per
  child, no network, no blocking connect), consistent with the existing
  `memory_breaches` call under the same lock. `supervise::stop` runs after the
  guard drops.
- `mem_over` resets on every decision, so a slow-dying child is not
  re-signalled every tick (it needs `limit_ticks` fresh breaches); a redundant
  SIGTERM on a gone pid is a harmless no-op.
- The peek is **advisory across a window boundary.** `can_restart` may green-light
  a SIGTERM that `restart` then declines, because the sliding window filled between
  the peek and the reap (other restart activity in the same window). In that corner
  case the crashed child is *dropped* rather than left alive — the "leave alive on
  exhaustion" guarantee holds for the budget state at decision time, not across an
  intervening fill. This never over-counts the budget (the record stays solely in
  `restart`) nor leaves a restart unrecorded; it is intentional and matches crash
  semantics for an exhausted child.
- The action never keeps the daemon alive — idle-exit still gates on
  `any_live()`.

## 8. No-kill escape hatch (docs only)

`docs/configuration.md` (and the `memory_action` config comment) gain a note:
to have the runtime or OS enforce a hard cap *without* the daemon restarting,
set it through the app's existing launch `env` — e.g.
`NODE_OPTIONS=--max-old-space-size=<MB>`, or a `ulimit -v` wrapper in the app's
launch command. The runtime/OS aborts the process on breach and the daemon's
existing crash → reap → respawn recovers it; the engine hardcodes no runtime.
This is documentation, not code.

## 9. Testing

### 9.1 Unit (deterministic, no processes) — supervisor.rs

- `mem_over` increments per consecutive over-limit tick and resets when RSS
  drops below the limit (drive via a child whose pid resolves to a known RSS,
  or by seeding `mem_limit` low against the test process's own tree, as the
  existing `tree_rss_bytes` test does);
- no action below `limit_ticks`; exactly one `Restart` at the `limit_ticks`-th
  consecutive breach when budget allows, and `mem_over` resets after it;
- with the budget exhausted (seed `restarts` to `max_restarts` within the
  window), the threshold tick yields `GiveUp` exactly once, then is suppressed
  by `mem_gave_up` until RSS drops below the limit;
- `can_restart` returns the same verdict as `may_restart` would *without*
  consuming a slot (call it N+1 times, confirm a following `may_restart` still
  succeeds when it should);
- `set_pid` resets `mem_over` and `mem_gave_up`;
- `mem_limit_actions` skips adopted survivors and pid-less reservations, and
  returns empty when `mem_limit == 0`.

### 9.2 Integration (Unix-gated, like tests/supervision.rs)

Spawn the daemon with `DEVKIT_DAEMON_MEMORY_ACTION=restart`,
`DEVKIT_DAEMON_MEM_LIMIT_MB=<small>`, `DEVKIT_DAEMON_MEM_LIMIT_TICKS=2`, a
generous `max_restarts`, and a long idle timeout. Supervise a **balloon
fixture**: a process that opens a listener, signals ready, then allocates and
*holds* memory past the limit (a large heap buffer it touches so the pages are
resident), staying alive. The test:

1. waits until `ports.json` shows the server with a pid and the port accepts;
2. waits for the fixture to balloon past the limit;
3. polls `ports.json` for a **pid change**, asserting the daemon SIGTERM'd the
   over-limit process and respawned it.

A budget-exhaustion variant sets `max_restarts` low (e.g. 1) with a fixture
that re-balloons immediately on each start; the test asserts the pid stops
changing (restarts cease) while the server **remains present** in `ports.json`
(left alive, not dropped), distinguishing `GiveUp` from health-probe's drop.

Both tests are `#![cfg(unix)]`, compiled out on Windows; RED→GREEN observed on
WSL, as for prior daemon phases. Poll for state, never fixed sleeps (loaded
Windows/CI runners exit children later than a short sleep allows — and the
balloon fixture must touch its pages so RSS, not just virtual size, grows).

## 10. Out of scope (YAGNI)

- Per-app memory limits, actions, or tick counts — one global trio.
- Throttle-without-kill / cgroup / job-object enforcement — not portable
  (§2 decisions); the docs escape hatch covers the runtime-cap case.
- A memory-specific backoff — the crash-loop budget already rate-limits
  repeated restarts, and `GiveUp` is the fallback.
- RSS smoothing / moving averages — the `limit_ticks` consecutive-breach gate
  is the agreed debounce.
- Wiring `DaemonConfig` through to the daemon binary — the binary reads env
  vars today; bridging config→env is a separate, pre-existing gap.

## 11. Files touched

- `src/bin/devkitd/supervisor.rs` — two `Child` fields, `insert_*` init,
  `set_pid` reset, `can_restart`, `MemAction`, `mem_limit_actions`, unit tests.
- `src/bin/devkitd/main.rs` — read `DEVKIT_DAEMON_MEMORY_ACTION` and
  `DEVKIT_DAEMON_MEM_LIMIT_TICKS`; the limit-action loop in the supervision
  tick; the §5.1 misconfiguration warning.
- `crates/devkit-ports/src/config.rs` — `memory_limit_ticks` field + default,
  updated `memory_action` comment, default-test assertion.
- `tests/supervision.rs` (+ `tests/common/mod.rs` harness) — the Unix-gated
  restart and budget-exhaustion integration tests + balloon fixture.
- `docs/configuration.md` — the no-kill escape-hatch note.
- `docs/next-features.md` — mark the phase-3 entry resolved.
- `AGENTS.md` — generalize the phase-2 invariant to cover the memory restart
  (a memory-triggered restart goes through the crash path, not its own).
