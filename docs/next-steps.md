# Next steps

Follow-ups intentionally left out of the `issue` consolidation branch.

## Update external callers to the `issue` binary

`issue-prep`, `issue-end`, and `pr-status` are gone. The concrete old→new command
mapping and the per-file list of callers to update lives in an uncommitted note in
the base repo: `../devkit/docs/issue-binary-migration.md` (i.e. the non-worktree
checkout). Callers include `~/.claude/commands/{issue-setup,issue-end,migration-review}.md`,
`~/.claude/scripts/issue-end-*.sh`, and `~/.local/bin/{pr-status,issue-end}.py`.

## Dashboard archive-cache seam

`issue dashboard` currently fetches live data (Linear GraphQL + `gh` + `git log`)
on every run. The design anticipates an archive/cache layer so historical buckets
don't require a full refetch each time; the fetch lives behind `dashboard/data.rs`,
which is the seam to plug a cache into.

## Route `lock` through the supervisor daemon

`lock` acquire/release/status/check go straight to the flock'd `locks.json` today.
The port registry already has an optional `devkit-portd` fast path
(`devkit-ports::registry::via_daemon`); add the equivalent for the lock registry so
high-frequency lock checks avoid the per-call file-lock + read. The daemon proto and
client live in `crates/devkit-ports/src/daemon/`.

## Unify the two flock'd-JSON stores

`devkit-ports::registry` (private `read`/`write`/`salvage`/`with_lock`) and
`devkit-locks::store` are the same machine over different schemas. Extract a generic
`devkit-common` locked-JSON store (a `with_lock<T>` parameterized by lock path, data
path, and a `Default + Serialize + Deserialize` payload) and have both adopt it.
