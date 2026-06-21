# Next steps

Follow-ups intentionally left out of the `issue` consolidation branch.

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
