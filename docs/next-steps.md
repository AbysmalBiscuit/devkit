# Next steps

Follow-ups intentionally left out of the `issue` consolidation branch.

## Scrub `configs/example.toml` from git history

The personal config is now untracked and gitignored (see `docs/configuration.md`),
but it still exists in past commits:

- `9e988cd`, `d9ce4bc` — on **`origin/main`** (already pushed). Content: worktree
  paths, doppler config name, app ports/env-var names, the local Supabase dev JWT
  default. No live secrets, but not for distribution.
- `d45f58e` — local only (adds `[people.igor]` with a Slack user id + GitHub
  handle). Keep this commit unpushed until the scrub, or strip the file from the
  branch before pushing.

Removing it from **all** history rewrites the `main` lineage and requires a
force-push of the remote default branch — a deliberate, coordinated operation:

```sh
# git-filter-repo is not installed by default
pipx install git-filter-repo            # or: brew install git-filter-repo

# from a fresh clone/mirror of the repo:
git filter-repo --path configs/example.toml --invert-paths

# rewrites every commit that touched the file (new SHAs)
git push --force origin main
```

Anyone with an existing clone/fork must re-clone or hard-reset after the force-push.

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
