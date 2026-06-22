# Daemon hard cgroup-v2 memory cap — design

**Status:** approved design (brainstorm 2026-06-22). Successor to the soft
poll-based `memory_action = "restart"`
(`docs/superpowers/specs/2026-06-22-daemon-memory-action-restart-design.md`).

**Goal:** give `devkitd` a kernel-enforced per-server memory ceiling so a runaway
dev server (Next/Turbopack ballooning to 8–20 GB) is contained by the kernel
instead of only by the 500 ms poll loop — while degrading cleanly to today's soft
behavior everywhere the kernel feature is unavailable.

## Summary of decisions

| Decision | Choice |
|---|---|
| Target platforms | All; hard cap is Linux-only, every other platform (and Linux without delegation) falls back to the existing soft `memory_action` path. |
| Delegation | Auto-detect (nest under the daemon's own cgroup) **plus** an opt-in `devkitd install-service` that writes a `systemd --user` unit with `Delegate=yes` (no sudo). |
| Layering | `memory.max` only, set **above** `memory_limit_mb`. The soft poll restart stays the graceful first responder; the kernel cap is a pure backstop for spikes too fast for the poll. No `memory.high`, no throttle tuning. |
| Placement | Approach A: the child writes its own pid into `cgroup.procs` in `pre_exec`, **before `exec`**, so it is inside the cap before running any of its own code (structural, not probabilistic). |
| Restart path | None new. A `memory.max` breach is a kernel OOM-kill → observed as a crash → existing reap → crash → respawn path, within the existing crash-loop budget. |

## Non-goals

- `memory.high` / reclaim throttling (the soft poll restart already provides the
  "act before death" layer; throttling risks the reclaim-coma documented in the
  prior `capmem` analysis).
- Hard caps on macOS / Windows (no cgroups; Windows Job Objects are a separate,
  much larger effort).
- Auto-enabling `loginctl enable-linger` (may require privilege; documented as a
  manual step for headless persistence).
- Re-caging adopted servers inherited from a previous daemon run (see §2).

## Architecture

The hard cap is a Linux-only enhancement layered behind the single `sys`
platform boundary (`crates/devkit-common/src/sys/`), with the existing soft
poll-based `memory_action` as the always-present fallback. With `memory_max_mb`
at its default `0`, or on any non-enforcing platform, the daemon behaves exactly
as it does today; the soft path is never disabled by this feature.

### New `sys` primitives

Real in `unix.rs`, no-op (returning "unsupported") in `windows.rs`. macOS uses
the unix file but its cgroup probe returns `enforce: false` (no cgroup-v2).

- `cgroup_caps() -> CgroupCaps`, a tri-state:
  - `Enforce { base: PathBuf }` — hard caps available.
  - `Unavailable { reason: String }` — the platform *has* cgroups but this daemon
    can't enforce (cgroup-v1, missing memory controller, or non-writable /
    non-delegated subtree). The daemon warns once with the reason.
  - `Unsupported` — the platform has no cgroups at all (macOS / Windows). The
    daemon stays silent (`memory_max_mb` is meaningless here).

  Startup probe. Linux: read `/proc/self/cgroup`, resolve to
  `/sys/fs/cgroup/<rel>`, verify cgroup-v2, that the `memory` controller is
  available, and that the subtree is writable; prepare the base by enabling
  `+memory` in `<base>/cgroup.subtree_control` and — to satisfy cgroup-v2's
  *no-internal-processes* rule — moving the daemon's own pid into a
  `<base>/supervisor/` leaf so server leaves can sit beside it. Returns
  `Enforce { base }` on success, `Unavailable { reason }` on cgroup-v1 / missing
  controller / non-writable subtree. The Windows (and macOS) impl returns
  `Unsupported`.
- `cgroup_create_leaf(base, key, max_bytes) -> Result<PathBuf>` —
  `mkdir <base>/servers/<key>/`, write `memory.max`, write
  `memory.oom.group = 1` (a breach kills the whole leaf together, not just the
  largest task → clean tree death). Leaves `memory.high` and swap at kernel
  defaults (pure backstop).
- `cgroup_open_procs(leaf) -> Result<OwnedFd>` — open `<leaf>/cgroup.procs` for
  the placement write.
- `cgroup_remove_leaf(leaf) -> Result<()>` — `rmdir` (succeeds only when empty).

### Placement (Approach A)

Extends the existing `pre_exec` setup in `sys::detach`. Alongside the current
`setsid`, the child writes its own pid (`getpid`, formatted into a stack buffer
by a hand-rolled async-signal-safe itoa, then a raw `write` to the pre-opened
`cgroup.procs` fd) **before `exec`**. Every descendant the server forks after
joining inherits the leaf (cgroup membership is inherited), so the whole tree is
capped.

`spawn_detached` gains an optional cgroup-procs fd parameter:
`spawn_detached(argv, cwd, env, logfile, cgroup_procs_fd: Option<OwnedFd>)`.
`devrun`'s callers pass `None`; only the daemon's spawn path passes `Some`.

### Ownership

The daemon owns one cgroup leaf per supervised server, keyed by the existing
`Key { holder, app, role }`. The key is sanitized to a single safe directory
name (`holder__app__role`, with slashes/dots escaped). Leaf lifecycle is bound
to the supervisor-table entry — created at spawn, reused on respawn, removed on
stop / give-up / startup reconcile.

## Data flow

### Spawn and respawn (daemon spawn path)

1. Compute leaf path `<base>/servers/<key>/`.
2. `sys::cgroup_create_leaf(base, key, memory_max_bytes)` — mkdir, set
   `memory.max` + `memory.oom.group=1`. On a respawn the leaf already exists and
   is empty (old process gone) → reuse, rewriting `memory.max`.
3. `sys::cgroup_open_procs(leaf)` → fd.
4. `spawn_detached(argv, cwd, env, log, Some(fd))` — child joins the cgroup in
   `pre_exec`, then `exec`s.
5. Record pid as today (`record_pid_with`).

When hard caps are inactive (`memory_max_mb == 0`, or `cgroup_caps()` is not
`Enforce`), steps 1–3 are skipped and step 4 passes `None` — identical to
today's spawn.

### Enforcement → respawn

The kernel enforces `memory.max` on the leaf. A breach OOM-kills the leaf (whole
tree, via `oom.group`). The 500 ms supervision tick's `reap_once` sees the dead
child → `restart()` → `respawn()`, reusing the now-empty leaf with a fresh
`memory.max`. No new code in the restart decision — it is just a crash, charged
against the existing crash-loop budget. A server that repeatedly balloons past
the hard cap exhausts its budget and is dropped, exactly like any crash loop.

### Teardown

The leaf is removed (`rmdir`) when the server leaves supervision for good — an
intentional `Down`, or a crash-loop give-up. Respawn does **not** tear down (it
reuses the leaf).

### Startup reconcile (adopt path)

After the daemon adopts live servers from `ports.json`, it enumerates existing
`<base>/servers/*` leaves and `rmdir`s any whose key isn't adopted — clearing
leaves orphaned by a previous daemon's unclean exit. Adopted servers stay in
their previous daemon's leaves and are **not** re-caged: the daemon can't move a
running adopted pid without the same delegation, and an adopted survivor has no
launch spec to respawn anyway (it is dropped on exit, per the existing
invariant). Adopted servers get capped naturally if they ever crash-respawn.

### Concurrency

Leaf create/teardown happens on the spawn and reap paths. cgroup file writes are
fast (no network, no flock), but the design keeps the `mkdir`/`write`/`rmdir`
syscalls outside the `sup` critical section where the existing code already
drops the guard before slow calls (mirroring how `supervise::stop` is called
after the bound `let` releases the lock).

## Config

New `[daemon]` key in `crates/devkit-ports/src/config.rs`:

```rust
/// Hard kernel memory ceiling per supervised tree, in MB (0 = off, Linux-only).
/// Enforced via a cgroup-v2 leaf with memory.max; a breach OOM-kills the tree
/// and the crash path respawns it. Set above memory_limit_mb so the soft
/// poll-based action stays the graceful first responder.
pub memory_max_mb: u64, // default 0
```

Env override `DEVKIT_DAEMON_MEM_MAX_MB`, read in `main.rs` like the others
(`env_u64`, ×1024×1024). Activation is resolved once at startup into an
`Option<CgroupBase>` the spawn path consults.

### Activation predicate

Hard caps are active iff `memory_max_mb > 0` **and** `cgroup_caps()` is
`Enforce { base }`. Resolved once at startup into the `Option<CgroupBase>` the
spawn path consults.

### Misconfiguration warnings

Log a line, don't reject (matches the existing `mem_limit <= mem_warn` warning):

- `memory_max_mb > 0` and `cgroup_caps()` is `Unavailable { reason }` →
  `"hard memory cap requested (N MB) but cgroup-v2 enforcement unavailable: {reason} — using soft memory_action only"`, logged **once** at startup. (`Unsupported`
  is silent — `memory_max_mb` is meaningless off-Linux.)
- `memory_max_mb > 0 && memory_limit_mb > 0 && memory_max_mb <= memory_limit_mb` →
  `"hard cap (N MB) at or below soft limit (M MB) — soft restart will never get to act first"`.

### Fallback matrix

| Environment | `cgroup_caps()` | Behavior |
|---|---|---|
| Linux, daemon in a delegated writable cgroup-v2 subtree with memory controller | `Enforce` | Hard caps active |
| Linux, no delegation (ad-hoc / login-session daemon) | `Unavailable` | Soft path only; one startup warn |
| Linux, cgroup-v1 or memory controller absent | `Unavailable` | Soft path only; one startup warn |
| macOS / Windows | `Unsupported` | Soft path only; **silent** (`memory_max_mb` is meaningless off-Linux, documented) |

`docs/configuration.md` gains a `memory_max_mb` row and a short subsection:
Linux-only, requires delegation (point to `install-service`), sits above
`memory_limit_mb`, falls back silently to the soft action otherwise. The
existing `static_env` / `ulimit -v` escape-hatch note gets a cross-reference now
that there is a first-class option.

## `install-service` and lifecycle

Auto-detect alone rarely enforces, because the autostart path
(`ensure_running` → `daemon::spawn`, direct `exec` in
`crates/devkit-ports/src/daemon/client.rs`) lands the daemon in a login-session
cgroup that isn't delegated. To actually get caps, the live daemon must be the
systemd-launched one. Two pieces.

### `devkitd install-service` (new subcommand, Linux + systemd only)

1. Resolve `current_exe()`; write `~/.config/systemd/user/devkitd.service`:

   ```ini
   [Unit]
   Description=devkit supervisor daemon

   [Service]
   Type=simple
   ExecStart=<abs>/devkitd
   Delegate=yes
   Restart=on-failure

   [Install]
   WantedBy=default.target
   ```

2. `systemctl --user daemon-reload`.
3. Stop any running ad-hoc daemon — send `Shutdown` over the socket (reuse the
   existing client handshake), wait for it to release `devkitd.lock`. Otherwise
   the lock-holding ad-hoc daemon blocks the systemd one. Stopping the *daemon*
   does not kill its supervised servers; the new systemd daemon re-adopts them
   on startup.
4. `systemctl --user enable --now devkitd.service`.
5. Print guidance: how to verify (`systemctl --user status devkitd`) and the
   headless-persistence note (`loginctl enable-linger`, may need privilege — not
   done automatically).

Idempotent. `uninstall-service` reverses it (`disable --now`, remove unit,
reload). On non-Linux the subcommand exits with a clear "requires Linux with
systemd --user" message.

### Autostart routing

`ensure_running` checks whether the unit file exists before its direct-`exec`
spawn. If present → `systemctl --user start devkitd.service` then the existing
5 s socket-readiness poll; if absent → direct `exec`, exactly as today. Once
installed, every autostart goes through systemd → delegated cgroup → caps;
uninstalled or non-systemd → unchanged. The single-instance `devkitd.lock`
guarantees one daemon either way.

### Idle-exit

Unchanged. A clean idle-exit is `exit(0)`; `Restart=on-failure` does not fight
it. The daemon idle-exits when idle and the routed autostart brings it back (in
the delegated cgroup) on the next supervision need. No special-casing.

## Error handling and invariants

**Fail-open on the cap, never on the spawn.** Any cgroup operation failure
(mkdir denied, `memory.max` write fails, fd open fails) must never block or kill
a server spawn. The spawn proceeds uncapped, logging the failure once per leaf.
A broken cgroup setup degrades to today's soft behavior.

`anyhow` `.context()` on every cgroup syscall, surfacing the path and operation.

Platform gating stays at the single `sys` boundary — no `#[cfg]` scattered
through `devkitd`; the daemon calls `sys::cgroup_*` unconditionally and the
Windows/macOS impls return "unsupported".

### New AGENTS.md invariants

- *A hard-cap breach is a crash, not a restart path.* `memory.max` +
  `memory.oom.group=1` OOM-kills the leaf; the reap → crash → respawn path
  handles it within the crash-loop budget. No path makes the hard cap restart
  directly — the same rule already established for health-probe and the soft
  memory restart.
- *Cap setup is fail-open.* A cgroup error degrades to the soft path; it never
  fails a spawn.
- *`memory_max_mb` sits above `memory_limit_mb`* so the soft action is the
  graceful first responder and the kernel cap is the backstop.

## Testing

### Unit (cross-platform; run on Windows + WSL)

- The async-signal-safe pid formatter (stack-buffer itoa) — pure function,
  exhaustive small / boundary cases.
- Leaf-key sanitization (`holder/app/role` → safe dir name) — round-trip and
  collision cases.
- Config: `memory_max_mb` parse + default `0`; both misconfiguration-warning
  predicates.
- Activation predicate (`memory_max_mb > 0 && enforce`).

### Integration (`#![cfg(unix)]`; run on WSL — Linux-only, like the phase-3 tests)

- *Cap enforced:* under a real delegated cgroup the test creates/owns, spawn a
  balloon fixture with `memory_max_mb` below its target; assert the kernel
  OOM-kills it, the daemon respawns it (poll for a new pid), and the leaf is
  reused.
- *Fallback when no delegation:* with `cgroup_caps()` returning `Unavailable`,
  assert the daemon spawns uncapped and logs the one-time warn — no spawn
  failure.
- *Teardown:* leaf `rmdir`'d on intentional `Down`; orphaned-leaf reconcile on
  startup.
- *Detection:* `cgroup_caps()` returns `Unavailable` cleanly on cgroup-v1 /
  missing controller (skip-or-assert depending on the runner).

Tests poll for the expected state rather than sleeping a fixed interval (a
loaded Windows/WSL runner reaps children late), using the WSL invocation
validated in phase 3.

### `install-service`

Unit-test the unit-file content generation (pure string) and the
unit-file-exists routing predicate. The actual `systemctl` round-trip is manual
verification (documented), since it mutates the user's systemd state.

## Open questions

None outstanding — all design forks were resolved during the brainstorm
(target platforms, delegation model, layering, placement mechanism, adopted-server
handling, autostart routing, ad-hoc-daemon stop on install).
