# Next steps

Follow-ups intentionally left out of the `issue` consolidation branch.

## Update external callers to the `issue` binary

`issue-prep`, `issue-end`, and `pr-status` are gone. The concrete old→new command
mapping and the per-file list of callers to update lives in an uncommitted note in
the base repo: `../devkit/docs/issue-binary-migration.md` (i.e. the non-worktree
checkout). Callers include `~/.claude/commands/{issue-setup,issue-end,migration-review}.md`,
`~/.claude/scripts/issue-end-*.sh`, and `~/.local/bin/{pr-status,issue-end}.py`.

## Authoritative in-memory mode for the lock registry

SHIPPED — `devkitd` now serves both the port and lock registries from memory over
`ports.sock` and `locks.sock`, write-through to the files, gated by `devkitd.lock`.
See `docs/superpowers/plans/2026-06-21-authoritative-in-memory-locks.md` and the
spec at `docs/superpowers/specs/2026-06-21-authoritative-in-memory-locks-design.md`.

## MCP server for devkit

Expose devkit's capabilities to coding agents over the Model Context Protocol so
an agent can drive port allocation, dev-server supervision, the issue lifecycle,
and file locks directly instead of shelling out to the CLIs. Tools mirror the
existing binaries (`portm`, `devrun`, `issue`, `lockm`) over the library crates
rather than reinvent them. Open questions to settle in its own brainstorming pass:
which surfaces to expose first, read-only vs. mutating tool boundaries, and how it
relates to the daemon (an MCP server is a natural long-lived host that could keep
the in-memory registries warm).
