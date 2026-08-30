# Preserving files on `issue end` — design

## Goal

Let a project name files inside a worktree that must survive `issue end`, and say
where each of them goes, so work an agent produced during a feature (graphify
reflections, scratch notes, generated reports) is not deleted along with the
worktree.

Preservation is declarative: a named config entry pairs a list of glob patterns
with a destination path template. Both are rendered per worktree, so one entry
can archive into the primary checkout while another writes beside the worktrees
directory.

## Scope

**In:** a `[preserve.<name>]` config table, a fail-closed copy step in
`issue end` before the worktree is removed, a `--no-preserve` escape hatch, and
the docs and JSON schema that follow from a new config table.

### Non-goals

- A `before_worktree_remove` command hook. The motivation for one was routing
  different files to different destinations, which per-entry destination
  templates cover without a shell and without a portable `cp`. A hook stays
  available as a later addition for actions a copy cannot express, such as
  posting notes to the tracker.
- Preserving anything on a removal devkit did not perform. A bare
  `git worktree remove` is outside devkit and stays that way.
- Any MCP surface change. `issue` actions on MCP are read-only, and `issue end`
  is not among them.
- Compression, deduplication, or a retention policy on the destination. Files
  are copied as files.

## Background

Three existing pieces shape the design.

`[hooks] after_worktree_create` in `crates/devkit-config/src/lib.rs` holds argv
arrays rendered as minijinja and run with no shell, fail-open, from both
`issue setup` and `issue checkout-pr`. It is the closest existing thing to a
lifecycle extension point, and its argv-only, fail-open contract is exactly what
makes it wrong for preserving files.

`defaults.worktree_include` copies globs from the primary checkout *into* a new
worktree through `plan_includes` / `apply_includes` in
`crates/devkit-common/src/worktree.rs`. Preservation is the inverse direction
with two different rules: the destination does not mirror the source's relative
path, and a failure must stop rather than warn.

`issue end`'s `cleanup` in `src/bin/devkit/issue/end.rs` is the single function
every removal path goes through, `--clean-worktree` and `--force` included. It
already reads `.devkit/issue.toml` for the recorded summary path before removing
anything, which is the hook point preservation needs.

## Config

A table of named entries, keyed like `[tasks.<name>]`, so a deeper `devkit.toml`
overrides one entry rather than replacing the whole list the way an array key
such as `hooks.after_worktree_create` or `prep_files` does.

```toml
[preserve.graphify]
from = ["graphify-out/**"]
to   = "{{ worktree_root }}/archive/{{ issue }}/graphify"

[preserve.notes]
from = ["docs/notes/*.md", "{{ summary }}"]
to   = "{{ primary }}/.devkit/archive/{{ issue }}"
```

```rust
/// Files copied out of a worktree before `issue end` removes it. Each entry
/// names its own destination, so different files can be archived to different
/// places in one run.
pub struct PreserveConfig {
    /// Glob patterns for the files to copy. Each is rendered as minijinja and
    /// then resolved against the worktree root unless it renders absolute. A
    /// pattern that renders empty is skipped; a pattern that matches nothing is
    /// not an error.
    pub from: Vec<String>,
    /// Destination directory, rendered as minijinja. Created if absent.
    pub to: String,
}
```

`Config` gains `preserve: BTreeMap<String, PreserveConfig>`. A `BTreeMap` rather
than the `HashMap` the sibling `tasks` / `apps` / `people` tables use: entries
are copied in key order and a failure names the entry it stopped on, so the order
must not vary between runs.

### Render context

The context `after_worktree_create` gets (`worktree`, `branch`, `issue`, `slug`,
`apps`, `prefix`), plus `[templates.variables]`, plus three fields preservation
needs in order to address a destination:

| Field | Value |
|---|---|
| `worktree_root` | `expand_tilde(defaults.worktree_root)`, the same value `issue setup` uses to place worktrees |
| `primary` | The primary checkout's root, from `git::primary_checkout` |
| `summary` | The summary path recorded in `.devkit/issue.toml`, or the empty string when the record names none |

`issue`, `slug`, and `apps` come from the record rather than being re-derived, so
a `worktree_dir` or `branch` template edited since setup cannot misname the
destination. A worktree with no record still gets `branch` and `worktree`; the
rest render empty, which is what makes a `{{ issue }}` in a destination path an
empty path segment rather than a failure. An entry that needs an issue id should
be written so an empty one still produces a usable path.

### Path resolution

- A rendered pattern that is relative resolves against the worktree root, and
  each match lands at `<to>/<path relative to the worktree>`.
- A rendered pattern that is absolute (`{{ summary }}`, which points outside the
  worktree) lands at `<to>/<file name>`.
- A match that is a directory is copied recursively, keeping its structure.
- An existing destination file is overwritten. The worktree's copy is the one
  about to be lost, so it wins; skipping would silently keep stale content from
  an earlier run of the same destination template.
- Destination directories are created as needed.

## Where it runs

Inside `cleanup`, in this order:

1. Canonicalize the worktree, refuse if the cwd is inside it.
2. Dirty check, unless `--force`.
3. Read `.devkit/issue.toml`.
4. **Preserve.** Render each entry and copy.
5. `git worktree remove`.
6. Remove the recorded summary file.
7. Delete the branch, sweep legacy `ISSUE_*.md`.

Step 4 sits after the record read because the context needs it, and before the
removal because that is the point. It also precedes step 6, which is what lets a
`{{ summary }}` pattern preserve the summary file `issue end` otherwise deletes.

Preservation runs on every removal path, `--clean-worktree` and `--force`
included. Those are the runs most likely to be discarding work.

`--no-preserve` skips the step entirely, for when the worktree really is
disposable.

## Failure policy

Fail-closed, unlike `[hooks]`.

A pattern that cannot be rendered, a bad glob, an unreadable source, or a copy
that fails aborts that worktree's removal with an error naming the entry. The
worktree, its branch, and its summary are left intact, so a rerun after fixing
the config finds the files still there. `cleanup` already returns `Result` and
`run` already reports a per-worktree failure without stopping the others, since
removals run one thread per worktree.

The contrast with `after_worktree_create` is deliberate and belongs in the docs.
A hook fires after the worktree exists, so warning and continuing costs nothing.
A preserve step fires one line before the files are deleted, so warning and
continuing costs the files.

Two things are not failures: an entry whose patterns match nothing, and a pattern
that renders to the empty string. An empty `graphify-out/` is the normal case for
most worktrees, and `{{ summary }}` renders empty whenever the record names no
summary.

## Code

**`crates/devkit-common/src/preserve.rs`** (new). Filesystem work only, no
minijinja: the caller passes rendered patterns and a rendered destination.

```rust
/// One rendered preserve entry: patterns and destination as they will be used.
pub struct Entry {
    pub name: String,
    pub patterns: Vec<String>,
    pub dest: PathBuf,
}

/// What one entry copied, for reporting.
pub struct Preserved {
    pub name: String,
    pub files: usize,
    pub dest: PathBuf,
}

/// Copy each entry's matches out of `worktree` into its destination. Fail-closed:
/// the first error stops the run and is returned with the entry named, so a
/// caller can abandon a removal rather than lose the files.
pub fn copy_out(worktree: &Path, entries: &[Entry]) -> Result<Vec<Preserved>>;
```

A new module rather than more functions in `worktree.rs`: that file's include
helpers are fail-open by contract, and mixing a fail-closed copier in beside them
invites a future edit that applies the wrong rule to the wrong function. The `glob::MatchOptions` settings match what
`plan_includes` uses.

**`crates/devkit-config/src/lib.rs`.** `PreserveConfig` with `JsonSchema`,
`Deserialize`, `Serialize` derives, and the `Config.preserve` field. No
`deny_unknown_fields`: `[github]` is the one table that carries it, because a
typo there silently targets a different repository, whereas a typo here surfaces
as files not being preserved on a run the user is watching.

**`src/bin/devkit/issue/end.rs`.** A `PreserveSettings` struct built once in
`run` (the entries, `templates.variables`, `worktree_root`, `branch_prefix`),
threaded into `cleanup` beside the existing `branch_lock`. Rendering happens per
worktree inside `cleanup`, since the context is per worktree.

**`src/bin/devkit/issue/tracker.rs`.** `select` loads the config and drops
everything but `tracker.kind` and `github`. It gains a sibling that returns the
loaded `Config` alongside, and `select` is rewritten in terms of it, so the three
other callers are unchanged and `end.rs` does not load the config twice.

## Docs and schema

- `docs/configuration.md`: a `[preserve.<name>]` section covering the two keys,
  the render context table, the path resolution rules, and the fail-closed
  contract with its contrast against `[hooks]`.
- `docs/commands.md`: the `issue end` entry gains the preserve step and
  `--no-preserve`.
- `schema/devkit-config.json`: regenerated with `DEVKIT_UPDATE_SCHEMA=1 cargo test`
  and committed, which the `tests/config_schema.rs` drift test enforces.
- `AGENTS.md`: no new invariant. The fail-closed rule lives in the config docs
  next to the fail-open hooks rule it contrasts with.

## Testing

In `crates/devkit-common/src/preserve.rs`:

- A relative pattern lands at `<dest>/<relative path>`; an absolute one lands at
  `<dest>/<file name>`.
- A directory match is copied recursively with its structure.
- An existing destination file is overwritten.
- A pattern matching nothing yields zero files and no error.
- A pattern that renders empty is skipped.
- A bad glob returns an error naming the entry.

In `src/bin/devkit/issue/end.rs`, over the git fixture the existing `cleanup`
tests already build:

- `cleanup` copies a matching file to the templated destination and then removes
  the worktree.
- A `{{ summary }}` entry preserves the summary file, and the summary is still
  deleted afterward. This is the ordering that is easy to get wrong.
- A copy failure leaves the worktree, its branch, and its summary intact and
  returns an error naming the entry.
- An empty `preserve` table removes the worktree exactly as it does today.
- `--no-preserve` skips a configured entry.
- Template rendering resolves `worktree_root`, `primary`, and `summary`.

`cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
and `cargo fmt --all --check` all stay green.

## Open questions

None outstanding. Two decisions were made rather than deferred, and either can be
revisited before implementation:

- **Fail-closed on a copy error**, aborting that worktree's removal, rather than
  warning and removing anyway.
- **`to` is always a directory.** Naming an exact destination filename, and so
  renaming a file on the way out, is not supported.
