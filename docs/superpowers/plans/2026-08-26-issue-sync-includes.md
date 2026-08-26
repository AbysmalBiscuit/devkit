# `issue sync-includes`

Re-run the `defaults.worktree_include` backfill against worktrees that already
exist, instead of only at `git worktree add` time.

## Problem

`worktree_include` copies untracked local files (`.env.local`, `.tool-versions`)
from the monorepo into a worktree. It runs in exactly two places, both at
creation time: `setup.rs:432` and `checkout.rs:423`. Edit one of those files in
the main checkout afterwards and every existing worktree keeps the old copy,
with no command to push the change out.

`worktree::copy_includes` also never clobbers — `copy_file` returns early when
the destination exists. That is right for a fresh worktree and wrong for a
sync, so the library needs an overwrite path before the command can exist.

## Design

### CLI

```
issue sync-includes [SELECTOR...] [--overwrite] [--yes] [--dry-run]
```

- `SELECTOR...` — issue ids, branches, or worktree paths, matched with
  `crate::select::matches`, the same way `issue end` and `issue info` do. Omit
  for every worktree.
- `--overwrite` — lift the never-clobber rule. Prompts per worktree.
- `--yes` — skip the prompt. Only meaningful with `--overwrite`.
- `--dry-run` — report what would change, write nothing.

Source is always the main repo path (`worktree::discover` returns it first),
never the current worktree. The main repo is never a target. The worktree you
run from is a target like any other.

Confirmation matches `issue end` exactly: `yes || confirm(&label)` per worktree,
no `is_terminal` check. `issue end` has no TTY gate, and this command should not
invent a third confirmation tier for the repo to be inconsistent about.

Empty `worktree_include` is a no-op that says so and exits 0.

### Library

`crates/devkit-common/src/worktree.rs` gains a plan/apply split, because
prompting requires knowing what would be clobbered before anything is written:

```rust
pub struct IncludePlan {
    pub missing: Vec<PathBuf>,   // relative to dest
    pub existing: Vec<PathBuf>,  // relative to dest
    pub warnings: Vec<String>,
}

pub fn plan_includes(source: &Path, dest: &Path, patterns: &[String]) -> IncludePlan;
pub fn apply_includes(source: &Path, dest: &Path, plan: &IncludePlan, overwrite: bool)
    -> (usize, Vec<String>);
```

`copy_includes` keeps its signature and becomes `plan_includes` followed by
`apply_includes(.., overwrite = false)`, so `setup` and `checkout` behave
identically and there is one traversal to maintain rather than two.

Both new functions stay fail-open: a glob or IO error becomes a warning string,
never an `Err`.

## Tasks

### Task 1 — `plan_includes`

`crates/devkit-common/src/worktree.rs`. Walk the same globs `copy_includes`
walks, recursing into directory matches the way `copy_dir` does, and sort each
matched file into `missing` or `existing` by whether the destination is there.
Paths in the plan are relative to `dest`.

Tests in the file's existing `mod tests`, using `tempfile::tempdir()`:
- a pattern matching one new file lands in `missing`
- a pattern whose destination already exists lands in `existing`
- a directory pattern enumerates its files recursively, not the directory
- a pattern matching nothing yields an empty plan and no warning
- a bad glob yields a warning and no panic

### Task 2 — `apply_includes`, and `copy_includes` on top of it

Same file. `apply_includes` copies `missing` always and `existing` only when
`overwrite`, returning `(copied, warnings)`. Then rewrite `copy_includes` as the
two calls with `overwrite = false`.

The existing `copy_includes` tests must stay green untouched — that is the
regression gate for the rewrite. Add:
- `overwrite = false` leaves an existing destination byte-identical
- `overwrite = true` replaces it with the source bytes

### Task 3 — the subcommand

`src/bin/issue/sync.rs` (new) and a `SyncIncludes` variant in
`src/bin/issue/main.rs`.

Flow: load config, bail early on an empty `worktree_include`, `discover` from
the start dir, drop the main repo, filter by selectors, `plan_includes` per
worktree, then per worktree either report (`--dry-run`), copy missing and warn
per existing file (default), or prompt and copy (`--overwrite`). Print a
summary of files copied per worktree.

Tests in `tests/` following `cli_ergonomics.rs`'s shape (real git repo in a
`tempfile::tempdir()`, private `HOME`/`XDG_STATE_HOME`, `CARGO_BIN_EXE_issue`):
- a missing file is copied into a second worktree
- an existing file is left alone and named in a warning
- `--dry-run` writes nothing
- `--overwrite --yes` replaces an existing file
- a selector limits the run to one worktree
- empty `worktree_include` exits 0 without touching anything

### Task 4 — docs

`docs/configuration.md` where `worktree_include` is documented, and the `issue`
section of `README.md`. Say that creation-time backfill and this command share
one include list, and that overwriting is opt-in.

## Gate

`cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo fmt --all`.

## Unresolved

None. Naming (`sync-includes` over `sync`), the flag split, and the
confirmation tier are all settled.
