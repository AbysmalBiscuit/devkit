# Next steps

Follow-ups intentionally left out of the `issue` consolidation branch.

## Update external callers to the `issue` binary

`issue-prep`, `issue-end`, and `pr-status` are gone. The concrete old→new command
mapping and the per-file list of callers to update lives in an uncommitted note in
the base repo: `../devkit/docs/issue-binary-migration.md` (i.e. the non-worktree
checkout). Callers include `~/.claude/commands/{issue-setup,issue-end,migration-review}.md`,
`~/.claude/scripts/issue-end-*.sh`, and `~/.local/bin/{pr-status,issue-end}.py`.

## Authoritative in-memory mode for the lock registry

The port registry now serves reads from the daemon's memory and writes through to
the file, with `devkitd.lock` enforcing the daemon-vs-direct boundary (see
`docs/superpowers/specs/2026-06-21-authoritative-in-memory-portd-design.md`). Give
the lock registry the same treatment: it needs a daemon path built from scratch
(proto variants, client, server dispatch) plus resolved-context facade variants —
the lock facade resolves the project root from CWD and the holder from process
identity client-side, so the server can't reuse the high-level functions directly.
Reuse the `Store` seam and extract the daemon framing/transport/client into
`devkit-common` at that point (a second daemon consumer makes it pay off).

## MCP server for devkit

Expose devkit's capabilities to coding agents over the Model Context Protocol so
an agent can drive port allocation, dev-server supervision, the issue lifecycle,
and file locks directly instead of shelling out to the CLIs. Deferred; this
section captures the analysis so the brainstorm starts ahead.

**Surface area (~19 actions).** `portm` (status, alloc, release, prune), `lockm`
(acquire, check, release, status, prune), `devrun` (up, down, status, logs),
`issue` (setup, status, end, prs, dashboard, review). `completions` is shell-only
and not exposed.

**Host-agnostic, assume eager loading.** Targets include Codex, Zed, Cursor, and
generic MCP clients, many of which load every tool schema up front — so keep the
top-level tool count low. (Claude Code lazy-loads via deferred-tool search, so the
count matters less there, but design for the worst case.)

**Three exposure shapes, meta-or-dispatch favored:**

1. *Flat* — one MCP tool per action (~19 tools); best discoverability and arg
   validation, heaviest footprint.
2. *Per-binary dispatch* — four tools (`portm`/`devrun`/`issue`/`lockm`), each
   taking an `action` + args; ~5x fewer tools, natural groupings, looser
   per-action schemas.
3. *True meta-MCP / progressive disclosure* — a `list`/`describe` tool plus a
   `call` tool; minimal footprint, scales as devkit grows, at the cost of a
   discovery round-trip per novel action.

Lean toward dispatch or meta given the agnostic, eager-loading hosts. Settle the
exact choice in the MCP's own brainstorm.

**Implementation stance.** Tools mirror the existing CLIs over the library crates
(`devkit-ports`, `devkit-locks`, `devkit-common`) rather than reinvent them. The
MCP server ships in the same multi-agent plugin as the `using-devkit` skill (see
`docs/superpowers/specs/2026-06-21-multi-agent-skill-packaging-design.md`).

**Daemon relationship.** An MCP server is a natural long-lived host that could keep
the in-memory registries (`devkitd`) warm.

**Open boundaries.** Which surfaces to expose first; the read-only vs. mutating
tool split.

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
