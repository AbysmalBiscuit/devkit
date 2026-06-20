# Next steps

Follow-ups intentionally left out of the `issue` consolidation branch.

## Scrub `configs/example.toml` from git history

The personal config is now untracked and gitignored (see `docs/configuration.md`),
so no new commit can re-add it — the historical contamination is **frozen**. There
is no urgency and no required ordering: finishing/merging features and scrubbing
history are independent. The file still exists in past commits:

- `9e988cd`, `d9ce4bc` — on **`origin/main`** (already pushed). Content: worktree
  paths, doppler config name, app ports/env-var names, the local Supabase dev JWT
  default. No live secrets, but not for distribution.
- `d45f58e` — local only (adds `[people.igor]` with a Slack user id + GitHub
  handle). Keep this commit unpushed until the scrub, or strip the file from the
  branch before pushing.

### Branch impact

Every local branch shares the `9e988cd` ancestor, so a history rewrite touches all
of them:

| Branch | Contains the file's history | On remote? |
|---|---|---|
| `main` | yes | yes (`origin/main`) |
| `feat/supervisor-daemon` | yes | local only |
| `feat/devkit-implementation` | yes | yes (`origin/...`, behind local) |
| `issue-binary-consolidation` | yes | local only (held unpushed) |

### Do NOT merge everything just to scrub

Merging unfinished feature branches into `main` only to enable a scrub forces
premature work onto `main` and collapses the branch structure. It is also
unnecessary: `git filter-repo` rewrites the **entire repo's object graph in one
pass** — `main` and every feature branch consistently — so a single run cleans all
branches at once while keeping them separate. No per-branch rebasing, no merging.

### Procedure

```sh
# git-filter-repo is not installed by default
pipx install git-filter-repo            # or: brew install git-filter-repo

# commit/clean every worktree first (filter-repo + live worktrees don't mix).
# run from the base checkout; this rewrites ALL refs (main + every branch):
git filter-repo --path configs/example.toml --invert-paths

# force-push the branches that exist on the remote:
git push --force origin main
git push --force origin feat/devkit-implementation

# each worktree's checked-out branch SHA changed — realign them:
git -C <each-worktree> reset --hard <its-branch>
```

Anyone with an existing clone/fork must re-clone or hard-reset after the force-push.

**Recommended timing:** defer until the in-flight features land on `main`, then run
**one** `filter-repo` on `main` and force-push `main` only — no other branches
outstanding means no reconciliation and a single force-push. Scrubbing earlier works
too (the whole-repo run above), it just means force-pushing two remote branches and
hard-resetting the live worktrees.

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
