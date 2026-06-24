# `issue info` — design

## Goal

A read-only `issue info` subcommand that reports one worktree's PR number and
Linear id, suitable both for a human glance and for scripting (e.g. naming a
tmux window `#123[ENG-456]`). It caches the PR number per worktree so repeated
calls — such as a shell hook that fires on every directory change — do not hit
`gh` once a PR exists.

## CLI surface

```
issue info [SELECTOR] [--json] [--cache-only]
```

- `SELECTOR` (optional positional): an issue id (`ENG-456`), branch, worktree
  basename, or path. When omitted, the target is the current worktree, resolved
  from `git rev-parse --show-toplevel` against cwd (honoring the global
  `-C/--dir`).
- `--json`: emit the machine-readable object instead of the human summary.
- `--cache-only`: do no network. The PR number comes from the per-worktree
  cache; Linear state and the finished verdict render as `—` (unknown offline).

`--no-fetch` is intentionally omitted. With a cache, the offline mode *is*
`--cache-only`; reading a small local JSON file is already instant, so a
separate cache-ignoring flag would add a mode without adding value.

## Output

- **Default (live):** discover → dirty check → `gh` PR lookup → Linear state →
  render a single-row summary through the existing `triage::render` (issue id,
  branch, tree clean/dirty, PR number + state, Linear, finished verdict). A live
  run writes the resolved PR to the cache (write-through).
- **`--cache-only`:** discover → dirty check → PR from cache → render with
  Linear and verdict shown as `—`.
- **`--json`:** serialize the single `IssueWorktree` struct (already
  `Serialize`). Consumers read `.pr_number` / `.issue_id`. The wrapping
  `StatusReport` is not used — a one-object payload is what scripts want.
- **No matching worktree** (e.g. cwd is `main`): exit non-zero with
  `not in an issue worktree` on stderr, and emit no JSON. Callers that poll on
  every `cd` already tolerate a non-zero exit.

## Cache

A new module `src/bin/issue/info_cache.rs` lives in the binary, not in
`devkit-issue`, so the `devkit-issue` facade stays read-only and mutation-free
(AGENTS.md). It follows the never-fatal philosophy of `dashboard/cache.rs`.

- **Location:** `<worktree>/.devkit/pr.json`, a sibling of `record.rs`'s
  `issue.toml`. One worktree tracks one branch and therefore one PR, so the file
  is a flat object — `{ "number": 123, "state": "OPEN", "url": "…" }` — with no
  map or keying.
- **Writes:** atomic (write to a temp file, then rename). A read failure or
  parse error is a cache miss, never an error.
- **Immutability:** a PR number is fixed once GitHub assigns it, so the cache
  needs no TTL. A live run overwrites the entry, so the cache self-heals; only
  `--cache-only` can observe a stale value, and the next live run repairs it.
- **Cleanup is structural, not coded.** `.devkit/` is in the global git excludes
  (`gitignore.rs`), so `git worktree remove` — what `issue end` runs — deletes
  the cache along with the directory. No `end.rs` change is needed, and manual
  `git worktree remove` is covered too.
- **No file lock.** In practice one session owns a worktree, and the atomic
  rename prevents torn reads, so the flock used by the port/lock registries is
  unnecessary here.

## Facade change

`crates/devkit-issue/src/status.rs` gains `gather_local(start, ids) ->
StatusReport`: discover + dirty + assemble with no network (PR `NO_PR`, Linear
`None`). The `--cache-only` path calls it and overlays the cached PR onto the
row (`IssueWorktree` fields are `pub`). The live path keeps using the existing
`gather` and adds the cache write afterward. The facade performs no mutation;
all cache writes live in the binary.

## Data flow

```
issue info ──► resolve selector ──► discover (local)
   live:       └► gather ──► render ; write .devkit/pr.json
   cache-only: └► gather_local ──► overlay .devkit/pr.json ──► render
   --json:     └► serialize IssueWorktree
```

## Testing (TDD)

- `info_cache`: write→read round-trip; missing and corrupt files return `None`;
  an atomic write leaves no temp file behind.
- `gather_local`: rows carry `NO_PR` / `None` with correct dirty flag and issue
  id, and the call makes no `gh` or Linear request.
- Selector resolution: cwd resolves to the current worktree; an explicit id,
  branch, or path resolves correctly; no match yields an error.
- `--json`: output deserializes to a single `IssueWorktree`.

## Downstream effect

Once `issue info --json` exists, the `tmux-issue-name` script can drop its own
`~/.cache/tmux-issue-name/` cache and read the PR number from devkit, making
devkit the single source of truth.
