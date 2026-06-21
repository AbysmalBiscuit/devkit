# Daemon Health-Probe Restarts — Design

**Status:** approved 2026-06-21. Implements supervisor-daemon spec §7
(`docs/superpowers/specs/2026-06-20-supervisor-daemon-design.md`), the
phase-2 health-probe follow-on, and resolves the "Health-probe restarts
(daemon phase 2)" entry in `docs/next-features.md`.

**Goal:** restart a supervised dev server that was once ready but has gone
hung — accepting no TCP connections — without ever killing a server that is
merely slow to start.

---

## 1. Problem

Core supervision is exit-based: `reap_once` restarts a child only when its
process has *exited* (`waitpid` for owned children, `pid_alive` for adopted
ones). A server that wedges — process alive, event loop stuck, listener no
longer accepting — is invisible to that path. The pid is live, so nothing
reaps it and nothing restarts it; the port stays bound to a dead-in-fact
server until a human notices.

Spec §7 deferred the fix to avoid false positives: the obvious naive probe
(restart on any failed connect) would kill a server during its normal slow
startup. The design below arms the probe only *after* a server has proven it
can accept a connection, so startup is never mistaken for a hang.

## 2. Approach

A dedicated **health-probe thread** runs beside the existing 500ms
supervision thread. Each cycle it TCP-probes every eligible server's port and
folds the result into per-child probe state. A server that fails `K`
consecutive probes *after it has once connected* is judged hung and sent
`SIGTERM`. The dying process is then collected by the existing supervision
tick exactly like a crash — `reap_once` returns it, `restart` respawns it
within the crash-loop budget. The probe thread's only mutations are its own
counters and that one signal; it never calls `restart` or respawns, so it
cannot race the reap thread.

This reuses every existing mechanism (crash-loop budget, zombie reaping,
adopted-drop, logging) and keeps the new code to: two `Child` fields, two
`Supervisor` accessors, one thread, and two config knobs.

### Decisions (resolved during brainstorming)

- **Arming gate — arm on first success.** A child is probe-eligible for a
  *hang* verdict only after one successful connect. Failures before that are
  ignored. Self-tuning, no startup timer, matches §7's "once ready" wording.
  Rejected: a fixed startup-grace window (needs tuning, can still kill a slow
  server); arm-on-success-or-max-window (overlaps devrun's launch-time
  readiness timeout).
- **Threading — a separate probe thread.** Its own cadence, isolated from
  reap/idle-exit latency. The race a second thread would normally introduce
  is dissolved by the SIGTERM-only action below.
- **Hung action — SIGTERM only, let the reap path restart.** The probe thread
  signals; the existing tick reaps and respawns. No duplicated machinery, no
  two callers of `restart`. Rejected: probe thread respawns directly
  (duplicates reap/budget/respawn, two mutators racing on one key).
- **Scope — owned children only.** Adopted survivors carry no launch spec, so
  SIGTERMing one would leave a dead port with nothing to respawn. The
  feature's value is auto-restart, which needs a launch spec. Adopted
  children are left untouched.

## 3. State changes

### 3.1 `Child` (src/bin/devkitd/supervisor.rs)

Two fields added:

```rust
struct Child {
    // ...existing fields...
    armed: bool,          // has this process accepted a probe connect at least once?
    probe_failures: u32,  // consecutive failed probes since arming or the last success
}
```

- `insert_owned` / `insert_adopted` initialise both to `false` / `0`.
- **`set_pid` resets both to `false` / `0`.** A respawned process is a new
  server that must re-prove readiness before it can be judged hung; inheriting
  the old process's `armed` flag would let a probe restart-loop a server that
  never finishes starting after a crash.

### 3.2 `Supervisor` accessors

Mirroring the `reap_once` / `memory_breaches` style (snapshot under the lock,
no syscalls held):

```rust
/// Owned children eligible for health probing: those the daemon can respawn
/// (a launch spec) and that have a live pid. Adopted survivors and pid-less
/// reservations are skipped.
pub(crate) fn probe_targets(&self) -> Vec<(Key, u16)>;

/// Fold one probe result into a child's health state. `ok` records a
/// successful connect (arms the child, clears the failure run); a failure on
/// an armed child increments its consecutive-failure count. Returns the pid
/// to SIGTERM when that count reaches `threshold` — and resets the count in
/// the same call, so a hung child is signalled once per K-failure run, not
/// every cycle. Returns `None` for: a child below threshold, an unarmed
/// child, or a key removed since the snapshot.
pub(crate) fn record_probe(&mut self, key: &Key, ok: bool, threshold: u32) -> Option<u32>;
```

`probe_targets` filters on `watch == Owned` (equivalently `launch.is_some()`)
and `pid != 0`. `record_probe` looks the key up and returns `None` if it is
gone (a concurrent `Down`).

## 4. The probe thread (src/bin/devkitd/main.rs)

Spawned from `main` **only when `health_probe_secs > 0`**, after the existing
supervision thread. Per cycle:

```text
loop {
    sleep(health_probe_secs)
    if shutdown { break }
    targets = sup.lock().probe_targets()          // brief lock, then released
    for (key, port) in targets {
        ok = tcp_connect_ok(port)                  // OUTSIDE the lock, ~300ms
        if let Some(pid) = sup.lock().record_probe(&key, ok, threshold) {
            supervise::stop(pid)                    // SIGTERM; reap tick handles the rest
            log_line("health: {holder}/{app} ({role}) unresponsive — restarting")
        }
    }
}
```

- `tcp_connect_ok(port)` is a single `TcpStream::connect_timeout` to
  `127.0.0.1:port` with the same 300ms timeout `supervise::wait_ready` uses —
  factored into a shared one-shot helper in `supervise` so both call sites use
  identical connect logic. It is **not** the polling `wait_ready` loop; one
  attempt per cycle is the probe.
- The `record_probe` lock is taken once per target and released immediately;
  the connect that precedes it is never under the lock.
- `supervise::stop` on an already-exited pid is a no-op, so a child that dies
  between the verdict and the signal is handled safely.

## 5. Restart path (unchanged, reused)

After SIGTERM the child exits; the 500ms supervision tick's `reap_once`
returns its key; `restart` respawns it within the crash-loop budget. A server
that hangs again after restart re-fails `K` probes, is signalled again, and so
on until `may_restart` exhausts the budget — at which point `restart` drops it
and logs "giving up … crash-loop budget exhausted". No new restart logic is
added; a hung server is funnelled into the crash path by a signal.

## 6. Config / knobs

The daemon binary reads `DEVKIT_DAEMON_*` env vars directly (the existing
pattern in `main.rs`); `DaemonConfig` mirrors the fields for completeness and
default-tests, as it already does for `max_restarts` and the memory knobs.

| Env var | `DaemonConfig` field | Default | Meaning |
|---|---|---|---|
| `DEVKIT_DAEMON_HEALTH_PROBE_SECS` | `health_probe_secs: u64` | `0` | Probe interval in seconds; `0` disables probing (thread not spawned). |
| `DEVKIT_DAEMON_HEALTH_FAIL_THRESHOLD` | `health_fail_threshold: u32` | `3` | `K` consecutive post-arming failures before a hang verdict. Only meaningful when probing is enabled. |

Default-off matches `memory_warn_mb = 0`: a conservative knob the user opts
into. `DaemonConfig::default()` and the `daemon_defaults_when_absent` test
gain assertions for both fields.

## 7. Concurrency correctness

- The probe thread's only state mutations are `armed` / `probe_failures`
  (under the `sup` lock) and a `SIGTERM` (outside the lock). It never calls
  `restart` and never respawns, so it shares no mutable respawn state with the
  reap thread.
- A hung server is *alive*; `reap_once` (waitpid / pid_alive) never returns
  it. Only the probe thread acts on it until SIGTERM. After it exits, only the
  reap thread acts on it. The two threads never touch the same child in the
  same state — no double-restart.
- Probes (300ms connects) run outside the `sup` lock, upholding the
  "no slow syscalls under the lock" invariant; RPC dispatch (which locks
  `sup`) is never stalled by a probe.
- `record_probe` resets the failure count when it returns `Some`, so a
  slow-dying child is not re-signalled on the next cycle (it would need `K`
  fresh failures; a redundant SIGTERM on a gone pid is a harmless no-op).
- The probe thread exits on `shutdown`. It never keeps the daemon alive —
  idle-exit still gates on `any_live()`, not on probe activity.

## 8. Testing

### 8.1 Unit (deterministic, no processes) — supervisor.rs

- failures before the first success return `None` (unarmed → ignored);
- after a success, the `K`-th consecutive failure returns `Some(pid)` exactly
  once, and the count resets (the next failure starts a fresh run);
- a success mid-run resets the count;
- `set_pid` re-disarms: post-respawn failures are ignored until the new
  process connects;
- `record_probe` on a removed key returns `None`;
- `probe_targets` includes owned children with a pid and excludes adopted
  survivors and pid-less reservations.

### 8.2 Integration (Unix-gated, like tests/supervision.rs)

Spawn the daemon with `DEVKIT_DAEMON_HEALTH_PROBE_SECS=1`,
`DEVKIT_DAEMON_HEALTH_FAIL_THRESHOLD=2`, and a long idle timeout. Supervise a
**hung-but-alive fixture**: a process that opens a listener, signals ready,
then on a trigger (a sentinel file appearing, or a short timer) closes its
listening socket and sleeps — pid stays alive, port stops accepting. The
test:

1. waits until `ports.json` shows the server with a pid and the port accepts
   (armed);
2. triggers the hang;
3. polls `ports.json` for a **pid change**, asserting the daemon SIGTERM'd the
   hung process and respawned it.

This test is `#![cfg(unix)]` and compiled out on Windows; RED→GREEN is
observed on WSL, as for the liveness-authority phase.

The fixture stays alive but stops accepting (rather than a `SIGSTOP`, which
leaves SIGTERM pending until `SIGCONT` and would wedge the reap), so SIGTERM
ends it cleanly and the reap path respawns deterministically.

## 9. Out of scope (YAGNI)

- HTTP/path-level health — TCP accept only, per §7.
- Per-app probe interval or threshold overrides — one global pair.
- Probing adopted survivors — they have no launch spec.
- A probe-specific backoff — the crash-loop budget already bounds repeated
  hang-restarts.
- Wiring `DaemonConfig` through to the daemon binary — the binary reads env
  vars today; bridging config→env is a separate, pre-existing gap not widened
  here.

## 10. Files touched

- `src/bin/devkitd/supervisor.rs` — `Child` fields, `set_pid` reset,
  `probe_targets`, `record_probe`, unit tests.
- `crates/devkit-common/src/supervise.rs` — shared one-shot TCP-probe helper
  used by both `wait_ready` and the probe thread.
- `src/bin/devkitd/main.rs` — read the two env knobs; spawn the probe thread
  when enabled.
- `crates/devkit-ports/src/config.rs` — two `DaemonConfig` fields + default
  assertions.
- `tests/supervision.rs` (or a sibling) — the Unix-gated integration test +
  hung-but-alive fixture.
- `docs/next-features.md` — mark the phase-2 entry resolved.
- `AGENTS.md` — note the health-probe knobs / behavior if the invariants list
  warrants it.
