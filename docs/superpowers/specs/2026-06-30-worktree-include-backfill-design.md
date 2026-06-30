# Worktree local-file backfill — design

**Goal:** When `issue setup` or `issue checkout-pr` creates a worktree, copy
gitignored/untracked local files (matching configured globs) from the monorepo
primary clone into the new worktree, so a fresh worktree starts with the local
config it needs (`.env.local`, `.tool-versions`, local hook directories, etc.).

This is creation-time only. The existing `worktree-sync-local.sh` backfill script
stays as-is and is out of scope.

## Problem

Today the only files materialized into a new worktree are the `.devkit/` record
and per-app `prep_files` (config-authored, template-rendered content written by
`write_prep_files`). Nothing copies *pre-existing untracked files* from the
monorepo primary clone into the new worktree. Because those files are gitignored,
`git worktree add` never brings them over, so a fresh worktree is missing local
config an app's `setup` step may depend on.

## Configuration

A new field on `Defaults` in `crates/devkit-ports/src/config.rs`:

```rust
/// Glob patterns (relative to the monorepo root) for untracked local files to
/// copy into a newly created worktree. Each match is copied to the same relative
/// path. A trailing slash, or a match that is a directory, copies recursively.
/// Empty by default — the backfill is opt-in.
#[serde(default)]
pub worktree_include: Vec<String>,
```

Example `devkit.toml`:

```toml
[defaults]
worktree_include = [
    "apps/*/.env.local",
    ".tool-versions",
    ".claude/my_hooks/",
]
```

Patterns are relative to the monorepo root and copied to the same relative path in
the worktree. Empty list → the feature is a no-op.

### Pattern semantics

- A pattern is a path glob (`*`, `?`, `**`), expanded against the monorepo root.
- A match that is a **file** is copied directly.
- A match that is a **directory** (or a pattern with a trailing `/`) is copied
  **recursively** (`cp -r` semantics).
- `dir/` and `dir` both mean "the directory, recursively"; `dir/*` means "each
  child". The trailing-slash-means-directory convention follows gitignore syntax,
  which devkit already lives alongside.

### Footgun note (documented, not enforced)

An unbounded `**/.env.local` descends into `node_modules`, `.git`, and `target`
and will copy dependency-local files too. Anchor patterns (`apps/*/.env.local`)
rather than scanning the whole tree. The implementation does not prune vendor
directories; that is the user's responsibility via anchored patterns.

## The copy unit — `devkit-common::worktree`

A single function, decoupled from config (it takes patterns as a slice, so
`devkit-common` keeps no dependency on `devkit-ports`):

```rust
/// Copy files matching `patterns` (path globs relative to `source`) into `dest`
/// at the same relative path. A match that is a directory is copied recursively.
/// Skips patterns that match nothing and dest paths that already exist (never
/// clobbers). Fail-open: a copy or glob error is collected as a warning, not
/// propagated. Returns (copied_file_count, warnings).
pub fn copy_includes(source: &Path, dest: &Path, patterns: &[String]) -> (usize, Vec<String>)
```

Behavior:

- Glob-expand each pattern against `source`. A non-UTF-8 or invalid pattern is a
  warning; the loop continues with the rest.
- For each match: a file is copied (parent dirs created); a directory is copied
  recursively, per-file.
- **Skip-existing** at the file level — an existing destination file is left
  untouched, so a partially-present directory backfills only what is missing.
- Never aborts. Errors (permission denied, non-UTF-8 path, unreadable source)
  become warnings in the returned vec.

The binary reads `cfg.defaults.worktree_include`, calls `copy_includes`, and
prints each warning to stderr (`warning: …`). The copied count is not added to
the JSON output.

## Glob dependency

Add the `glob` crate (single-threaded, ~zero transitive deps, supports `**` and
dotfiles). The existing `glob_match` in `crates/devkit-issue/src/prs.rs` matches
flat CI-check *names* — no filesystem walk, no `**` — and is not reusable here.

Parallelism (`jwalk`/`rayon`) is explicitly rejected: the workload is a handful of
small files, the only cost that scales is the walk for `**` patterns, and worktree
creation is already dominated by `git fetch` and per-app installs. A creation-time
one-shot does not justify a global threadpool. If recursive scans over a huge tree
ever become a real need, the correct fix is vendor-dir pruning, not threads.

## Insertion points

Call `copy_includes` immediately after `git worktree add`, **before** `prep_apps`,
in both commands. Ordering matters: local files must land before an app's `setup`
commands run, since those may read `.env.local`.

- `src/bin/issue/setup.rs` — after the worktree-creation step (currently around
  line 168), before `prep_apps`.
- `src/bin/issue/checkout.rs` — after worktree creation, **unconditionally** (not
  gated on `--setup`; backfilling local files is independent of app prep).

Source root is the monorepo primary clone: `monorepo = <worktree_root>/monorepo`
(already computed in both commands). Destination is the new `worktree` path.

## Error handling

Fail-open throughout, consistent with devkit's "setup is fail-open" rule:

- A pattern that matches nothing is silent (mirrors the bash script's `[ -e ]`
  guard).
- A copy failure (permission, non-UTF-8 path) is a warning; the loop continues.
- The backfill never aborts or rolls back worktree creation.

## Testing (TDD)

Unit tests on `copy_includes` over temp directories:

- Copies a matching file, preserving its relative path under `dest`.
- A `**` pattern matches a nested file.
- A directory pattern (and a trailing-slash pattern) copies the directory
  recursively.
- A pattern matching nothing is silently skipped (no warning, no error).
- An existing destination file is not clobbered (skip-existing).
- A source that cannot be read yields a warning, and the other matches still copy.
- Empty `patterns` → `(0, [])`, no filesystem touch.

The full gate (`cargo test --workspace`, `cargo clippy --workspace --all-targets
-- -D warnings`, `cargo fmt --all --check`) must stay green.

## Out of scope

- Changes to `worktree-sync-local.sh` (kept as the backfill-for-old-worktrees tool).
- A dedicated `issue backfill` subcommand.
- Vendor-directory pruning / `.gitignore`-aware walking.
- Parallel directory walking.
