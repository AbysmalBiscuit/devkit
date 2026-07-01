# Next Features — delegated / deferred work

Features intentionally deferred out of current scope, kept here so they aren't
lost. Each entry records *why* it was deferred and *what* it would take, so a
future implementation starts from the analysis rather than from scratch.

---

## Explore `gix` (gitoxide) for git operations

**Status:** OPEN — prototyped and benchmarked 2026-07-01; **no solution
adopted** (decision pending). Shelling out to the `git` CLI stays the default.
A working reads-only prototype exists on the unmerged branch `perf/gix-reads`
(worktree `../devkit-worktrees/gix-reads`); the findings from it are below.
**Want:** evaluate replacing the `git` subprocess calls in
`devkit-common::cmd` (and the per-worktree `status`/`worktree list`/`rev-parse`
reads behind `devkit-issue::status`) with the pure-Rust `gix` crate, to drop
process-spawn overhead and parse structured data instead of porcelain text.

**Why deferred — the spawn is not the cost.** Profiling the slow path
(`issue status` over ~34 worktrees) showed each `git status --porcelain` is
~30 ms, almost entirely the working-tree walk (`sys` time); process spawn is
negligible. Two dependency-free changes already captured the available local
win: the per-worktree dirty checks now run on a bounded thread pool
(`status::dirty_many`, ~1.2 s → ~0.16 s for 34 worktrees), and enabling
`core.untrackedCache` on a repo cuts a single warm `status` to ~16 ms — a perk
we get *for free* by shelling out, since the CLI honors the user's git config
(untracked-cache, `fsmonitor`, index v4, sparse-checkout). After both,
`issue status` is network-bound (`gh` + Linear), not git-bound, so a git library
cannot move the headline number.

**Prototype findings (2026-07-01, branch `perf/gix-reads`).** Built a working
`gix` reads-only port — a `devkit-common::git` module with typed reads
(`toplevel`, `current_branch`, `branch_exists`, `common_dir`, `resolve`,
`config_get`, `global_config_get`, `remote_url`, and a structured `discover`
replacing the `worktree list --porcelain` parser), ~17 read call sites migrated,
mutations + `fetch`/`push`/`gh` left shelled. Full gate green (clippy, fmt,
tests). Then measured, which surfaced two decisive facts:

- **`status` regresses hard and was reverted.** In-process `is_dirty` vs. a
  warm shelled `git status --porcelain` on a synthetic clean tree: **24 ms vs.
  2.8 ms at 2k files, 114 ms vs. 6.4 ms at 10k files** — `gix` is 8–18× slower
  because it re-walks the whole tree every call while the CLI uses
  `core.untrackedCache`. Confirms the thesis above; the 3 dirty-check sites stay
  shelled.
- **The cheap reads *are* faster in-process, but the absolute win is tiny.**
  `resolve HEAD` 0.12 ms vs. 1.16 ms; `current_branch` 0.10 ms vs. 1.15 ms
  (~10–12× faster) — but that is ~1 ms saved per call, and an interactive command
  issues only a handful. Not a headline mover.
- **Dependency cost is the real sticking point.** Even trimmed to
  `features = ["revision", "sha1"]` (`default-features = false`), `gix` pulls in
  **95 new crates** (~1180 `Cargo.lock` lines). That cost is fixed the moment you
  depend on `gix` *at all* — porting fewer functions does not reduce it. So it is
  effectively all-or-nothing. `gix` is pure-Rust (no C toolchain, cleanest Windows
  CI); `git2` would be ~10 crates but a vendored `libgit2` C build needing a C
  toolchain on all three CI OSes. Neither is lighter on both axes.

**Undecided:** the cost/benefit is poor on today's usage — 95 crates for a
sub-millisecond-per-call read speedup plus a modest structured-worktree-list
maintainability nicety. Left open rather than adopted; revisit if future work
would lean on an in-process git library more heavily (justifying the fixed dep
cost), or if the structured/typed reads become worth it for their own sake.

**What it would take / open questions for when this is picked up:**
- `gix status` vs. warm-cache CLI is now benchmarked (see findings above): it
  regresses 8–18×, so any future adoption must keep `status` shelled — the
  demonstrated loss, not an assumption.
- The reverse also held: `worktree list` and `rev-parse`-style ref reads are the
  low-risk wins (structured, no porcelain parsing, faster) — but see the fixed
  95-crate dependency cost, which is the actual blocker, not the code.
- Cross-platform parity still needs checking beyond the Linux prototype: CI runs
  ubuntu/macos/windows; `gix` behavior on Windows working trees is unverified.
- Adjacent CLI win intentionally *not* taken: `--no-optional-locks` on the
  read-only status calls conflicts with `core.untrackedCache` (it suppresses the
  index write that persists the cache), so untracked-cache was preferred.

### `gix` reads-only port — `diff --stat` and `log --format` left shelled

**Status:** DEFERRED — excluded from the scope of a `gix` reads-only port
(`rev-parse`, `status`→dirty, `config`, `remote get-url`, `show-ref`,
`worktree list`). These two reads stay on the `git` CLI because `gix` has no
equivalent one-liner and reproducing git's text output is high-effort,
high-fidelity-risk work for two cold, once-per-command sites where the spawn
cost is irrelevant.

- **`git diff <ref>...HEAD --stat`** (`src/bin/devrun/main.rs:442`, display-only
  diffstat). The `...` (three-dot) form diffs from `merge-base(ref, HEAD)`, so it
  needs a merge-base + tree-to-tree diff, then a **per-file blob diff** (via
  `gix_diff`/`imara-diff`) to get the `+X −Y` line counts (tree diff only names
  the changed files). Then git's diffstat text is reproduced by hand: column
  alignment, the width-scaled `+++---` bar, and the summary line with correct
  pluralization (`1 file changed` vs `2 files changed`,
  `insertion(+)`/`insertions(+)`). ~80–100 lines, the highest fidelity risk in
  the whole port, for zero measurable perf.
- **`git log --author=<a> --format=%aI`** (`src/bin/issue/dashboard/data.rs:179`,
  author-timeline analytics). A `gix` revwalk from HEAD reading `commit.author()`
  is straightforward (~20–30 lines), but two fidelity hazards remain: git's
  `--author` is a case-insensitive regex over `"Name <email>"` (a `gix` port
  approximates with substring/case-fold matching — close but not identical), and
  `%aI` strict-ISO-8601-with-offset formatting must match git's rendering exactly.
  Borderline-portable; deferred with `diff` since the win is nil.

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
Per-app prep is now a configurable `prep_files` list (per file: `path`,
`content`, `overwrite`); the hardcoded `.env.local` filename, dotenv format, and
write-if-absent-only strategy are gone. Content is now rendered as a minijinja template
(shipped 2026-06-24 — see `docs/superpowers/specs/2026-06-24-prep-file-templating-design.md`);
symlink mode stays deferred. The analysis below is kept for context.
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
