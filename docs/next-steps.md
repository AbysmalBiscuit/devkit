# Next steps

Follow-ups intentionally left out of the `issue` consolidation branch.

## Daemon phase 3 — `memory_action = "restart"` (brainstorm in progress)

Restart a supervised server whose tree-RSS crosses `memory_limit_mb` by SIGTERM,
letting the existing reap path respawn it within the crash-loop budget — the
health-probe pattern (daemon phase 2) applied to memory. Tracked in
`docs/next-features.md` (`## memory_action = "restart"`). Resume with the
brainstorming → spec → writing-plans → subagent-driven flow used for phases 1–2.

**Recreate the workspace:** worktree branch `feat/devkitd-memory-action-restart`
off `main` (the local worktree from the first session was not carried over).

**What the codebase already provides (verified — little new machinery needed):**

- `DaemonConfig` already has `memory_warn_mb`, `memory_limit_mb`, and
  `memory_action` (default `"warn"`) — `crates/devkit-ports/src/config.rs:27-32`.
- `memory_limit_mb` (env `DEVKIT_DAEMON_MEM_LIMIT_MB`) is read, stored on
  `Supervisor`, and exposed via `mem_limit()` — but currently **inert**: breach
  detection (`memory_breaches`) only checks `mem_warn`
  (`src/bin/devkitd/supervisor.rs:230-250, 252-254`).
- `memory_action` is **not read by the daemon binary yet** (no
  `DEVKIT_DAEMON_MEMORY_ACTION` env read). Follow the phase-2 precedent: read the
  env var directly in `main.rs`, default `"warn"`; the config field mirrors it.
- `may_restart()` is already documented as *"Shared by crash- and memory-triggered
  restarts"* and enforces `max_restarts` / `restart_window`
  (`src/bin/devkitd/supervisor.rs:146-168`).
- The reap→`restart()`→`respawn()` path already records the budget and respawns
  (`src/bin/devkitd/main.rs:225-228, 375-`). Memory restart should funnel through
  it (SIGTERM-only), matching the AGENTS invariant established for health-probe.

**Design sketch (two independent thresholds):** `memory_warn_mb` keeps logging an
early heads-up (unchanged); crossing `memory_limit_mb` takes the action. The
memory path runs in the **existing 500 ms supervision tick** (memory already lives
there; a `tree_rss_bytes` /proc read is cheap, unlike health-probe's 300 ms
connect — no separate thread needed). Edge-trigger the limit breach (a per-child
flag, like `warned_mem`) so a child is SIGTERM'd once per breach, not every tick
during its death/respawn window.

**OPEN QUESTION (paused here — decide before writing the spec):** on exhausting
the restart budget, what happens?
  1. *(recommended)* **Leave alive, warn only** — check the budget *before*
     killing; while restarts remain, SIGTERM → reap respawns (budgeted); once
     exhausted, stop killing and just log a warn line, leaving the leaky-but-
     working server running. Matches the written *"exhaust the budget and fall
     back to warn"* constraint, and implies a non-recording budget *peek* in the
     memory path (the actual record stays in `restart()`), so the budget is
     counted once per restart, not double.
  2. **Kill, then drop (health-probe parity)** — SIGTERM unconditionally on
     breach; the reap path respawns within budget and drops the server when
     exhausted. Identical mechanism to health-probe, but leaves a runaway server
     dead rather than warned-and-alive.

Secondary open question for the spec: restart on the first reading over
`memory_limit_mb`, or require it to persist a couple of ticks (avoid a transient
allocation spike killing a server)? Leaning immediate — a hard cap set above
normal usage should act on first crossing.

## Update external callers to the `issue` binary

`issue-prep`, `issue-end`, and `pr-status` are gone. The concrete old→new command
mapping and the per-file list of callers to update lives in an uncommitted note in
the base repo: `../devkit/docs/issue-binary-migration.md` (i.e. the non-worktree
checkout). Callers include `~/.claude/commands/{issue-setup,issue-end,migration-review}.md`,
`~/.claude/scripts/issue-end-*.sh`, and `~/.local/bin/{pr-status,issue-end}.py`.

## Authoritative in-memory mode for the lock registry

`devkitd` serves both the port and lock registries from memory over `ports.sock`
and `locks.sock`, write-through to the files, gated by `devkitd.lock`. Delivered by
`docs/superpowers/plans/2026-06-21-authoritative-in-memory-locks.md` and the spec at
`docs/superpowers/specs/2026-06-21-authoritative-in-memory-locks-design.md`.

## MCP server for devkit

v1 is implemented (`crates/devkit-mcp` + `src/bin/devkit-mcp`): a meta-MCP stdio
server (`devkit_describe` + `devkit_call`) exposing the 9 port + lock actions over
the library facades. Design: `docs/superpowers/specs/2026-06-21-devkit-mcp-server-design.md`.
Plan: `docs/superpowers/plans/2026-06-21-devkit-mcp-server.md`.

Deferred follow-ups:

- **Daemon-aware locks.** v1 lock writes go straight through `FlockStore` and will
  hard-error (`DaemonHoldsLock`) under a live `devkitd`. Full cooperation needs
  daemon-aware resolved-context lock facade variants — owned by the "Authoritative
  in-memory mode for the lock registry" section above. Until then, run lock actions
  without a daemon, or wire that work first.
- **`devrun` + `issue` actions.** Phase 2 of the surface (process supervision, the
  issue/PR lifecycle). Higher blast radius; add as new registry entries — the tool
  shape does not change.
- **Live MCP registration for Codex and Cursor.** Only the Claude Code `.mcp.json`
  is provided. Confirm the MCP-server registration field each host expects, install
  the plugin in Codex and Cursor, and confirm `devkit_describe`/`devkit_call` appear.
- **`initialize` protocol-version negotiation.** The server returns a fixed
  `protocolVersion`; confirm it against the versions the target hosts send and
  negotiate if a host requires it.
- **Ports holder is the project root.** Ports actions use `root` as the registry
  holder (the liveness path), while locks use the minted session-token holder.
  Confirm this matches how an agent expects to address its allocations across
  multiple worktrees.

## Verify multi-agent plugin packaging

The packaging is shipped but not yet exercised end-to-end. Two things still need
a live test:

- **Claude marketplace install.** `github.com/AbysmalBiscuit/devkit` is private for
  now and will be made public later. Once it is public, run
  `/plugin marketplace add AbysmalBiscuit/devkit` then `/plugin install devkit` in a
  fresh session and confirm the `using-devkit` skill resolves from the plugin. Until
  then the relative-path marketplace source cannot be added via git.
- **Codex/Cursor SessionStart hook.** The context-injection envelopes
  (`hookSpecificOutput.additionalContext` for Codex, `additional_context` for Cursor)
  are verified against current docs but neither agent has been run yet. Install the
  plugin in Codex and Cursor, start a session, and confirm the "A 'using-devkit'
  skill is available" notice appears. On Windows, confirm `run-hook.cmd` locates Git
  Bash. If an envelope is rejected, adjust `hooks/announce-skill`.
- **Cursor hook command path resolution.** `hooks/hooks-cursor.json` invokes the
  runner as the relative `./hooks/run-hook.cmd` (matching the obra/superpowers
  reference), whereas `hooks/hooks-codex.json` uses the root-anchored
  `${PLUGIN_ROOT}/hooks/run-hook.cmd`. The relative form only resolves if Cursor runs
  the hook with its working directory set to the plugin root; if it runs from the
  session's repo instead, the hook silently no-ops (and looks like an envelope bug).
  Confirm Cursor's cwd on the first live install. If it is not the plugin root, switch
  the command to a root-anchored variable (e.g. `${CURSOR_PLUGIN_ROOT}/...`) once its
  expansion in command position is confirmed.
