# Daemon-Authoritative Liveness Design

**Status:** approved design, pre-implementation
**Date:** 2026-06-21

**Goal:** Make `devkitd`'s in-memory supervisor table the sole authority on whether
a supervised child's exit is a crash (restart it) or an intentional stop (let it
die), eliminating a race in which the daemon's own registry pruning makes a
just-crashed server look stopped and suppresses the restart.

**Architecture:** The supervision tick stops consulting the `ports.json` row to
classify an exit. Any key returned by `Supervisor::reap_once()` is a crash by
construction, because the only way a supervised key leaves the table is an
explicit `Down` (which removes it *before* killing the child) or a give-up. The
registry row becomes derived state that `record_pid`'s re-establish-if-pruned
invariant rebuilds on respawn.

**Tech stack:** Rust (edition 2024), the existing `devkitd` supervision thread and
`Supervisor` table. No new dependencies, no schema change, no wire-protocol change.

---

## 1. The race being fixed

The original analysis (`docs/next-features.md`, "Crash-restart vs. external
registry prune (race)") described an *external* `registry::snapshot()` writing a
pruned `ports.json` to disk while the daemon read the file raw. The in-memory and
gate work that shipped on 2026-06-21 changed the shape of this race:

- The supervision thread reads the daemon's **in-memory** registry
  (`d.ports.lock().clone()`), not a raw file read.
- Direct flock writers are fenced out by `devkitd.lock`: a daemon-unaware
  `down`/`prune` hard-errors `DaemonHoldsLock` and cannot mutate `ports.json`
  behind a live daemon.

So the external race is gone, but a **daemon-internal** one replaced it and is
easier to trigger:

1. A supervised child crashes; its row in `d.ports` still carries `pid =
   Some(dead_pid)`.
2. `reap_once()` flags the key dead; the tick sleeps 200 ms (debounce).
3. Within that window any client `status`/`snapshot` reaches the daemon. The
   daemon's own `snapshot_with(&port_store())` sees the dead pid, runs its
   best-effort prune, and removes the row from `d.ports` **in memory** (and the
   file).
4. The tick wakes, re-reads `d.ports`, finds the row gone, classifies the exit as
   an intentional stop, and does **not** restart.

Every `status` call is a trigger, so the window is hit routinely under normal use.

## 2. Why the supervisor table is the right authority

The row was made the cross-tool stop signal in the supervisor-daemon design (§6)
so a daemon-unaware tool could say "stop" by removing the row. Two facts make that
premise obsolete:

- **`Down` already removes-before-kill.** The daemon's `down()` handler removes the
  key from the `Supervisor` table (under the `sup` lock) and only then SIGTERMs the
  child. A child stopped through `Down` is gone from the table before it dies, so
  `reap_once()` never returns it.
- **Legacy writers are fenced out.** With a live daemon holding `devkitd.lock`
  exclusive, a daemon-unaware `down` can no longer remove a row at all — it is
  refused with `DaemonHoldsLock`.

Therefore, by the time `reap_once()` reports a key, the child exited on its own
and was not intentionally stopped: it is a crash. Classifying it by re-reading a
prunable registry row adds nothing and is the sole source of the race.

## 3. The change

### 3.1 Supervision tick (`src/bin/devkitd/main.rs`)

Replace the reap-then-classify block (currently: reap → `sleep(200ms)` →
`d.ports.lock().clone()` → row-present? `restart` : `remove`) with a direct
dispatch:

```rust
let dead = d.sup.lock().unwrap().reap_once();
for key in dead {
    restart(&d, &key);
}
```

The 200 ms debounce and the `d.ports` re-read are removed. `restart()` already
enforces the crash-loop budget and drops adopted survivors that have no launch
spec, so no classification logic is lost.

### 3.2 `restart()` case split (`src/bin/devkitd/main.rs`, `supervisor.rs`)

`restart()` currently collapses "key absent" and "key present but no launch spec"
into a single `launch_of(key).is_none()` branch that logs "no launch spec". With
the debounce gone, a `Down` can still race a just-detected crash and remove the
key between `reap_once()` and `restart()`. Distinguish the cases so the log stays
accurate:

- key **absent** from the table (removed by a concurrent `Down` or give-up) →
  return quietly, no log, no restart;
- key **present, no launch spec** (adopted survivor) → drop it and log "no launch
  spec to respawn", as today;
- key **present, has launch spec** → crash-loop budget check → respawn.

Add a small accessor to `Supervisor`:

```rust
pub(crate) fn contains(&self, key: &Key) -> bool {
    self.children.contains_key(key)
}
```

### 3.3 `down()` ordering is load-bearing (`src/bin/devkitd/server.rs`)

`down()` must keep removing the key from the `Supervisor` table before stopping
the child; that ordering is what guarantees a stopped child never reaches the
restart path. No code change — add a comment marking the ordering load-bearing so
a future edit does not reorder it.

## 4. Edge cases

- **Down of a live child.** The child's pid is alive, so `reap_once()` does not
  return it; `down()` removes the key and SIGTERMs. No restart.
- **Down racing a crash.** The crash is detected (`reap_once()` returns the key),
  then `down()` removes the key before `restart()` runs. `restart()` sees the key
  absent → returns quietly. Correct: the operator asked for it to stop.
- **Adopted survivor crashes.** `reap_once()` returns it (polled, pid dead);
  `restart()` finds the key present with no launch spec → drop + log. Unchanged.
- **Crash-loop exhausted.** `restart()` gives up, removes the key, logs. Unchanged.
- **Prune flicker during the gap.** Between crash detection and respawn, a
  concurrent `snapshot` may prune the dead-pid row from `d.ports` and the file.
  `record_pid` on respawn re-establishes it (the existing re-establish-if-pruned
  invariant). The row's brief absence is transient display state and self-heals.

## 5. Testing strategy

- **Regression (RED→GREEN), Unix:** `restart_survives_concurrent_snapshot` —
  supervise a `python3 -m http.server`, capture pid1, then SIGKILL the child while
  a second client connection hammers `Request::Snapshot` in a tight loop for ~2 s
  (each snapshot prunes the dead-pid row inside the daemon). Poll `ports.json`
  until the pid changes; assert pid2 ≠ pid1. On the pre-change code a snapshot
  reliably lands in the 200 ms debounce window → row pruned → no restart → the
  test fails; under the table-authoritative tick it passes.
- **Update existing comments:** `restart_after_kill` and `down_does_not_restart`
  still pass, but their comments describe the retired debounce/row-check mechanism.
  Rewrite them to describe table-authoritative behavior, in timeless terms (no
  "used to", no PR/issue references).

## 6. Docs to update

- `docs/next-features.md` — mark "Crash-restart vs. external registry prune (race)"
  resolved by this spec; keep a one-line pointer rather than deleting the analysis.
- `AGENTS.md` — state the restart-authority invariant in the daemon section if it
  is not already captured (the supervisor table, not the registry row, decides
  crash vs. stop). Verify during planning before adding, to avoid duplication.
- The supervisor-daemon design (`docs/superpowers/specs/2026-06-20-supervisor-daemon-design.md`)
  stays as approved history; this spec records that its §6 "registry row is the
  cross-tool stop signal" and §14 decision 2 (legacy-`down` debounce re-check) are
  superseded for the daemon-authoritative model.

## 7. Out of scope (YAGNI)

- Protecting supervised rows from pruning (Approach A) or marking supervised rows
  in the on-disk schema (Approach C) — the table-authoritative decision needs
  neither; both were considered and rejected.
- Health-probe restarts, `memory_action = "restart"`, and the cgroup memory cap —
  separate deferred daemon phases (`docs/next-features.md`).
- Any change to the wire protocol, the registry schema, or the `devkitd.lock`
  gate.

## 8. Resolved decisions

1. **Authority model** — the `Supervisor` table is the sole authority on crash vs.
   stop; the restart decision no longer reads the registry row (Approach B).
2. **Debounce** — removed entirely. Its only purpose was to let a racing legacy
   `down` land its row removal before the row was read; both the read and the
   legacy-`down` write are gone.
3. **History** — the old supervisor-daemon spec is left intact; supersession is
   recorded here, not by editing the approved document in place.
</content>
</invoke>
