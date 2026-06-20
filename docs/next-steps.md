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
the file, with `portd.lock` enforcing the daemon-vs-direct boundary (see
`docs/superpowers/specs/2026-06-21-authoritative-in-memory-portd-design.md`). Give
the lock registry the same treatment: it needs a daemon path built from scratch
(proto variants, client, server dispatch) plus resolved-context facade variants —
the lock facade resolves the project root from CWD and the holder from process
identity client-side, so the server can't reuse the high-level functions directly.
Reuse the `Store` seam and extract the daemon framing/transport/client into
`devkit-common` at that point (a second daemon consumer makes it pay off).
