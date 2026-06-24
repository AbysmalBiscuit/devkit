# Next Features — delegated / deferred work

Features intentionally deferred out of current scope, kept here so they aren't
lost. Each entry records *why* it was deferred and *what* it would take, so a
future implementation starts from the analysis rather than from scratch.

---

## Hard cgroup-v2 memory cap for supervised servers

**Status:** RESOLVED 2026-06-22 — see
`docs/superpowers/specs/2026-06-22-daemon-hard-cgroup-memory-cap-design.md`.
**Want:** start each supervised dev server under a hard per-server memory ceiling
so a runaway (Next/Turbopack ballooning to 8–20 GB) is contained instead of
eating all RAM. Daemon v1 ships only the portable poll-based **track + warn**
(spec §6a); this is the hard-enforcement follow-on.

**Why a prior shell attempt (`~/.local/bin/capmem`) was unreliable.** It wrapped
the command as `sudo bash … runuser -u $user -- <cmd>`. The decisive bug:
**`sudo` scrubs the environment** to a sanitized minimum, and `runuser` inherits
that scrubbed env rather than the server's. So the dev server launched **without
its config** — doppler-injected secrets, `NODE_ENV`/`NODE_OPTIONS`/`PORT`, and a
correct `PATH` were all missing. The process *started* (so it looked alive) but
ran misconfigured → "the servers just didn't work properly." Aggravators:
`memory.swap.max = 0` plus `memory.high` at 7/8 of the cap meant bursty compiles
hit hard reclaim throttling with no swap relief (apparent hangs), and a genuine
spike past `memory.max` OOM-killed the whole tree mid-build.

**Why the daemon fixes it.** The env-scrub bug disappears: the daemon already
builds the correct child environment and spawns the server **directly as the
user with that env** — no `sudo`/`runuser` hop to strip anything. The only thing
needing privilege is moving the pid into a delegated cgroup-v2 subtree, done
**once at startup** (or via systemd user-delegation), after which each child's
pid is written into a subtree the daemon already owns — **no per-spawn `sudo`**.
And because the daemon *owns* the child, an OOM-kill is observed and handled by
the existing restart + crash-loop-backoff path instead of leaving a dead
terminal.

**Implementation notes for when this is picked up:**
- Set a generous `memory.max` as the true runaway ceiling; keep `memory.high`
  *near* max (not far below) so normal bursty compiles aren't throttled into a
  coma; allow some swap headroom so spikes spill rather than OOM-kill.
- Detect cgroup-v2 delegation at daemon startup; if unavailable (e.g. WSL2
  without delegation), log that hard caps are off and fall back to the v1
  poll-based warn — never fail a spawn because the cgroup couldn't be set up.
- Add config under `[daemon]`: e.g. `memory_max_mb` (hard) alongside the existing
  `memory_warn_mb` / `memory_limit_mb`; document the privilege/delegation
  requirement.

---

## `memory_action = "restart"`

**Status:** RESOLVED 2026-06-22 — see
`docs/superpowers/specs/2026-06-22-daemon-memory-action-restart-design.md`. A
server whose tree-RSS stays over `memory_limit_mb` for `memory_limit_ticks`
consecutive supervision ticks is SIGTERM'd and respawned through the crash path
within the crash-loop budget; once the budget is exhausted the daemon warns and
leaves the server alive (`memory_action = "restart"`, default off).

---

## Crash-restart vs. external registry prune (race)

**Status:** RESOLVED 2026-06-21 — see
`docs/superpowers/specs/2026-06-21-daemon-authoritative-liveness-design.md`. The
daemon now decides crash vs. stop from its supervisor table, not the registry row,
so a concurrent prune can no longer suppress a restart. The analysis below is kept
for context.

**The race:** when a supervised child crashes, the supervision thread reaps it, waits a
short debounce, then reads `ports.json` raw (no liveness prune) to tell a crash (row
still present → restart) from an intentional `down` (row removed → let die). That raw
read deliberately avoids `registry::snapshot()`, which prunes dead-pid rows. But an
*external* `registry::snapshot()` (e.g. a concurrent `devrun status`) running inside the
sub-second crash window prunes the dead row to disk first; the daemon's raw read then
sees it absent and treats the crash as an intentional stop — so the server is not
restarted.
**Why deferred:** the window is ~200–700 ms and needs a concurrent external snapshot;
unlikely interactively, plausible in tight CI loops. A proper fix makes the daemon the
authority on supervised-process liveness (e.g. snapshot does not prune rows a live daemon
owns, or the daemon marks supervised rows so external prunes skip them) — which belongs
with the future daemon-managed liveness path rather than the v1 poll loop.

---

## Health-probe restarts (daemon phase 2)

**Status:** RESOLVED 2026-06-21 — see
`docs/superpowers/specs/2026-06-21-daemon-health-probe-restarts-design.md`. A
separate probe thread arms each owned server on its first successful connect and
SIGTERMs it after K consecutive failures; the reap path respawns it within the
crash-loop budget. Enabled via `DEVKIT_DAEMON_HEALTH_PROBE_SECS` (0 = off). The
analysis below is kept for context.

**Want:** a periodic TCP readiness probe (reusing `supervise::wait_ready`); if a
server that was once ready refuses connections for K consecutive probes, treat it
as hung and restart it. Kept out of core supervision to avoid false-positive
restarts; the supervision thread already loops, so there is room for it.

---

## Configurable per-app prep step (generalize `.env.local` writing)

**Status:** RESOLVED 2026-06-24 — see
`docs/superpowers/specs/2026-06-24-configurable-per-app-prep-files-design.md`.
Per-app prep is now a configurable `prep_files` list (per file: `path`, verbatim
`content`, `overwrite`); the hardcoded `.env.local` filename, dotenv format, and
write-if-absent-only strategy are gone. Format/template generation and symlink mode
stay deferred to the messages-templates feature. The analysis below is kept for context.
**Want:** make the `issue setup` per-app prep step fully configurable instead of
hardcoding the `.env.local` filename, dotenv format, and write-if-absent strategy.

**Current state (not symlinking — a written file).** `issue setup`
(`src/bin/issue/setup.rs:92-110`) writes a `<app>/.env.local` file with
`key=value` lines drawn from each app's `prep_env` map, only when `prep_env` is
non-empty, and skips if the file already exists. The *content* is config-driven
(`prep_env` in `devkit.toml`); a generic per-app `setup` command list handles
everything else. So the behavior is already optional — but three mechanics are
hardcoded: the filename `.env.local`, the `key=value` (dotenv) format, and the
write-if-absent logic. A project that wants a different prep filename/location, a
different format, or a symlink to shared secrets instead of a written file cannot
express that today.

**Design questions for when this is picked up:**
- Add a per-app `prep_file` field (target path, default `.env.local`) so the
  name/location is configurable.
- Decide whether `prep_env`-writing should collapse into the generic `setup`
  task list (one uniform mechanism) or stay a typed step (clearer intent,
  cross-platform write without shelling out).
- Symlink-vs-write: support linking an app's prep file to a shared secrets file,
  not just writing one. Settle the format question (dotenv only, or other).
- Keep it cross-platform — symlinks and file writes behave differently on Windows.
