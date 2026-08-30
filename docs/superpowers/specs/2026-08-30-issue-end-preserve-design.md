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

**In:** a `[preserve.<name>]` config table, a fail-open copy step in `issue end`
before the worktree is removed, a `--no-preserve` escape hatch, and the docs and
JSON schema that follow from a new config table.

### Non-goals

- A `before_worktree_remove` command hook. The motivation for one was routing
  different files to different destinations, which per-entry destination
  templates cover without a shell and without a portable `cp`. A hook stays
  available as a later addition for actions a copy cannot express, such as
  posting notes to the tracker.
- Reaching files outside the worktree. Patterns are worktree-relative, because
  `plan_includes` warns `match outside source` on anything else. The recorded
  summary file therefore cannot be preserved at its default location; a project
  that wants it kept sets `templates.issue_summary_path` to
  `{{ worktree }}/.devkit/issue.md`, which `docs/configuration.md` already
  documents, and then names it in a pattern like any other file.
- Preserving anything on a removal devkit did not perform. A bare
  `git worktree remove` is outside devkit and stays that way.
- Any MCP surface change. `issue` actions on MCP are read-only, and `issue end`
  is not among them.
- Compression, deduplication, or a retention policy on the destination. Files
  are copied as files.

## Background

Two existing pieces carry almost all of this.

`defaults.worktree_include` copies globs from the primary checkout *into* a new
worktree through `plan_includes` / `apply_includes` in
`crates/devkit-common/src/worktree.rs`. Preservation is the same walk pointed the
other way: same `glob::MatchOptions`, same recursive directory handling, same
`create_dir_all` on the destination parent inside `copy_file`, same fail-open
`Vec<String>` warnings. The one behavioral difference is that preservation
overwrites, which `apply_includes` already exposes as its `overwrite` parameter.

`[hooks] after_worktree_create` in `crates/devkit-config/src/lib.rs` sets the
lifecycle-extension precedent and the fail-open contract: a hook that cannot
render, spawn, or exit zero warns on stderr and the command carries on.

`issue end`'s `cleanup` in `src/bin/devkit/issue/end.rs` is the single function
every removal path goes through, `--clean-worktree` and `--force` included. It
already reads `.devkit/issue.toml` before removing anything, which is where the
render context comes from.

## Config

A table of named entries, keyed like `[tasks.<name>]` / `[apps.<name>]` /
`[people.<alias>]`, so a deeper `devkit.toml` overrides one entry rather than
replacing the whole list the way an array key such as
`hooks.after_worktree_create` or `prep_files` does.

```toml
[preserve.graphify]
from     = ["graphify-out/**"]
to       = "{{ worktree_root }}/archive/{{ issue }}/graphify"
required = true

[preserve.notes]
from = ["docs/notes/*.md"]
to   = "{{ primary }}/.devkit/archive/{{ issue }}"
```

```rust
/// Files copied out of a worktree before `issue end` removes it. Each entry
/// names its own destination, so different files can be archived to different
/// places in one run.
pub struct PreserveConfig {
    /// Glob patterns for the files to copy, relative to the worktree root and
    /// rendered as minijinja. A pattern that renders empty is skipped; a
    /// pattern that matches nothing is not a failure.
    pub from: Vec<String>,
    /// Destination directory, rendered as minijinja. Created if absent.
    pub to: String,
    /// Abort this worktree's removal when the entry warns, instead of removing
    /// it anyway. Off by default.
    pub required: bool,
}
```

`Config` gains `preserve: HashMap<String, PreserveConfig>`, matching its sibling
tables. Entries are copied in sorted key order, the way `brief.rs` sorts the app
and task names it collects from those same maps, so a warning naming an entry
reads the same across runs.

### Render context

The context `after_worktree_create` gets (`worktree`, `branch`, `issue`, `slug`,
`apps`, `prefix`), plus `[templates.variables]`, plus two fields a destination
needs in order to address a place outside the worktree:

| Field | Value |
|---|---|
| `worktree_root` | `expand_tilde(defaults.worktree_root)`, the same value `issue setup` uses to place worktrees |
| `primary` | The primary checkout's root, from `git::primary_checkout` |

`issue`, `slug`, and `apps` come from `.devkit/issue.toml` rather than being
re-derived, so a `worktree_dir` or `branch` template edited since setup cannot
misname the destination. A worktree with no record still gets `branch` and
`worktree`; the rest render empty, which makes a `{{ issue }}` in a destination
path an empty path segment rather than a failure. An entry that needs an issue id
should be written so an empty one still produces a usable path.

### Path resolution

Whatever `plan_includes` already does, which is:

- A pattern resolves against the worktree root, and each match lands at
  `<to>/<path relative to the worktree>`.
- A match that is a directory is copied recursively, keeping its structure.
- A match resolving outside the worktree warns and is skipped.
- Destination directories are created as needed.

Plus one rule preservation sets: an existing destination file is overwritten
(`apply_includes` with `overwrite: true`). The worktree's copy is the one about
to be lost, so it wins; skipping would silently keep stale content from an
earlier run of the same destination template.

## Where it runs

Inside `cleanup`, in this order:

1. Canonicalize the worktree, refuse if the cwd is inside it.
2. Dirty check, unless `--force`.
3. Read `.devkit/issue.toml`.
4. **Preserve.** Render each entry and copy.
5. `git worktree remove`.
6. Remove the recorded summary file.
7. Delete the branch, sweep legacy `ISSUE_*.md`.

Step 4 sits after the record read because the context comes from it, and before
the removal because that is the point.

Preservation runs on every removal path, `--clean-worktree` and `--force`
included. Those are the runs most likely to be discarding work.

`--no-preserve` skips the step entirely, for a worktree whose scratch is not
worth archiving.

## Failure policy

Fail-open, like `[hooks]` and like `copy_includes`. A pattern that cannot render,
a bad glob, an unreadable source, or a copy that fails prints a `warning:` line
naming the entry, and the worktree is removed anyway. Most worktrees carry
nothing worth preserving, and holding a cleanup command hostage to an archive
that was never going to have contents is the wrong default.

Two things do not even warn: an entry whose patterns match nothing, and a pattern
that renders to the empty string. An empty `graphify-out/` is the normal case.

`required = true` flips one entry to fail-closed. Its warnings become an error
that aborts that worktree's removal, leaving the worktree, its branch, and its
summary intact so a rerun after fixing the config still finds the files.
`cleanup` already returns `Result` and `run` already reports a per-worktree
failure without stopping the others, since removals run one thread per worktree.
`required` governs errors only, never emptiness, so an entry marked required on a
worktree that produced no reflections still removes cleanly.

## Code

**`crates/devkit-common/src/worktree.rs`.** A `copy_out` beside `copy_includes`,
built from the same two helpers:

```rust
/// Copy files matching `patterns` (globs relative to `source`) out of a worktree
/// into `dest`, at the same relative path, overwriting what is already there.
/// The outbound counterpart to `copy_includes`, and fail-open the same way:
/// returns (files_copied, warnings).
pub fn copy_out(source: &Path, dest: &Path, patterns: &[String]) -> (usize, Vec<String>) {
    let plan = plan_includes(source, dest, patterns);
    let (copied, apply_warnings) = apply_includes(source, dest, &plan, true);
    let mut warnings = plan.warnings;
    warnings.extend(apply_warnings);
    (copied, warnings)
}
```

It differs from `copy_includes` in one argument, but the two names carry
different policies at their call sites (a backfill never clobbers; an archive
always does), and putting the policy in the name is what keeps a future edit from
applying the wrong one.

**`crates/devkit-config/src/lib.rs`.** `PreserveConfig` with `JsonSchema`,
`Deserialize`, `Serialize` derives, and the `Config.preserve` field. No
`deny_unknown_fields`: `[github]` is the one table that carries it, because a
typo there silently targets a different repository, whereas a typo here surfaces
as a warning on a run the user is watching.

**`src/bin/devkit/issue/end.rs`.** A `PreserveSettings` struct built once in `run`
(the entries in sorted key order, `templates.variables`, `worktree_root`,
`branch_prefix`), threaded into `cleanup` beside the existing `branch_lock`.
Rendering happens per worktree inside `cleanup`, since the context is per
worktree.

**`src/bin/devkit/issue/tracker.rs`.** `select` loads the config and drops
everything but `tracker.kind` and `github`. It gains a sibling that returns the
loaded `Config` alongside, and `select` is rewritten in terms of it, so the three
other callers are unchanged and `end.rs` does not load the config twice.

## Docs and schema

- `docs/configuration.md`: a `[preserve.<name>]` section covering the three keys,
  the render context table, the worktree-relative constraint with the
  `issue_summary_path` workaround, and the fail-open contract with `required` as
  the opt-out.
- `docs/commands.md`: the `issue end` entry gains the preserve step and
  `--no-preserve`.
- `schema/devkit-config.json`: regenerated with `DEVKIT_UPDATE_SCHEMA=1 cargo test`
  and committed, which the `tests/config_schema.rs` drift test enforces.
- `AGENTS.md`: no new invariant.

## Testing

In `crates/devkit-common/src/worktree.rs`, beside the existing include tests:

- `copy_out` lands a match at `<dest>/<relative path>` and copies a directory
  match recursively.
- `copy_out` overwrites an existing destination file, where `copy_includes`
  leaves it alone. The two assertions belong in one test so the policy
  difference is visible.
- A pattern matching nothing yields zero files and no warnings.

In `src/bin/devkit/issue/end.rs`, over the git fixture the existing `cleanup`
tests already build:

- `cleanup` copies a matching file to the templated destination and then removes
  the worktree.
- A failing entry that is not required warns and the worktree is still removed.
- A failing entry that *is* required leaves the worktree, its branch, and its
  summary intact and returns an error naming the entry.
- A required entry whose patterns match nothing removes the worktree cleanly.
- An empty `preserve` table removes the worktree exactly as it does today.
- `--no-preserve` skips a configured entry.
- Template rendering resolves `worktree_root` and `primary`, and a missing
  record renders `issue` empty without failing.

`cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
and `cargo fmt --all --check` all stay green.

## Open questions

None outstanding. One decision was made rather than deferred:

- **`to` is always a directory.** Naming an exact destination filename, and so
  renaming a file on the way out, is not supported. Two worktrees archiving the
  same filename to the same `to` collide, and the second wins; templating
  `{{ issue }}` into `to` is how a project avoids that.
