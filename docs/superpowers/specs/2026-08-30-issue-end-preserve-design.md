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

**In:** a `[preserve.<name>]` config table, a serial preservation phase in
`issue end` that runs before any worktree is removed, a `--no-preserve` escape
hatch, a progress step per entry, and the docs and JSON schema that follow from a
new config table.

### Non-goals

- A `before_worktree_remove` command hook. The motivation for one was routing
  different files to different destinations, which per-entry destination
  templates cover without a shell and without a portable `cp`. A hook stays
  available as a later addition for actions a copy cannot express, such as
  posting notes to the tracker.
- Reaching files outside the worktree. Patterns that are absolute or contain a
  `..` component are rejected. The recorded summary file therefore cannot be
  preserved at its default location; a project that wants it kept sets
  `templates.issue_summary_path` to `{{ worktree }}/.devkit/issue.md`, which
  `docs/configuration.md` already documents, and then names it in a pattern like
  any other file.
- A symlink policy. `plan_dir` and `std::fs::copy` follow links, so a symlink
  inside the worktree is archived as its target's content, and a directory
  symlink cycle recurses. This is what `worktree_include` already does on the
  inbound path; preservation inherits it rather than growing a second traversal.
  Documented, not fixed here.
- Atomic replacement of an existing destination file. `std::fs::copy` truncates
  before it writes, so a copy that fails partway leaves a short file. The serial
  phase below is what keeps this from costing data: a failure is known before any
  worktree is removed, and `required = true` stops the removal.
- Preserving anything on a removal devkit did not perform. A bare
  `git worktree remove` is outside devkit and stays that way.
- Any MCP surface change. `issue` actions on MCP are read-only, and `issue end`
  is not among them.

## Background

Two existing pieces carry most of this.

`defaults.worktree_include` copies globs from the primary checkout *into* a new
worktree through `plan_includes` / `apply_includes` in
`crates/devkit-common/src/worktree.rs`. Preservation is the same walk pointed the
other way: same `glob::MatchOptions`, same recursive directory handling, same
`create_dir_all` on the destination parent inside `copy_file`, same fail-open
`Vec<String>` warnings. The behavioral difference is that preservation
overwrites, which `apply_includes` already exposes as its `overwrite` parameter.

`[hooks] after_worktree_create` in `crates/devkit-config/src/lib.rs` sets the
lifecycle-extension precedent, the fail-open contract, and the progress pattern:
`issue setup` sizes its `Steps` total to include the hook count and draws one
labelled step per hook, so a slow hook never looks like a hung terminal.

`issue end`'s `cleanup` in `src/bin/devkit/issue/end.rs` is the single function
every removal path goes through, `--clean-worktree` and `--force` included.

## Config

A table of named entries, keyed like `[tasks.<name>]` / `[apps.<name>]` /
`[people.<alias>]`, so a deeper `devkit.toml` overrides one entry rather than
replacing the whole list the way an array key such as
`hooks.after_worktree_create` or `prep_files` does.

```toml
[preserve.graphify]
from     = ["graphify-out/"]
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
#[serde(deny_unknown_fields)]
pub struct PreserveConfig {
    /// Glob patterns for the files to copy, relative to the worktree root and
    /// rendered as minijinja. A pattern that renders empty is skipped; a
    /// pattern that matches nothing is not a failure.
    pub from: Vec<String>,
    /// Destination directory, rendered as minijinja. Must render to a non-empty
    /// absolute path. Created if absent.
    pub to: String,
    /// Abort this worktree's removal when the entry warns, instead of removing
    /// it anyway. Off by default.
    #[serde(default)]
    pub required: bool,
}
```

`Config` gains `preserve: HashMap<String, PreserveConfig>`, matching its sibling
tables. Entries are copied in sorted key order, the way `brief.rs` sorts the app
and task names it collects from those same maps.

`deny_unknown_fields` is the second table to carry it, after `[github]`. The
reason is the same shape as that one: serde consumes an unknown field as
`IgnoredAny`, so `requred = true` would leave the entry fail-open with no
diagnostic, and the user would believe files were protected that were not. Update
the sentence in `AGENTS.md` that calls `[github]` the only such table.

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
misname the destination. `record::read` returns `None` for a malformed record as
well as an absent one, so both cases take the same typed defaults: `issue` and
`slug` render as the empty string, `apps` as the empty list.

An empty `issue` is a harmless path segment only inside an otherwise absolute
template, and it collapses two worktrees onto one destination. Both are covered
by the destination rules below rather than by special-casing the record.

### Path resolution

Patterns, after rendering:

- A pattern that renders empty is skipped.
- A pattern that is absolute, or contains a `..` component, warns and is skipped.
  Without this, `source.join("../x")` strips lexically to `../x` and the copy
  escapes both the worktree and the destination.
- Everything else resolves against the worktree root, and each match lands at
  `<to>/<path relative to the worktree>`. A directory match is copied
  recursively, keeping its structure.
- An existing destination file is overwritten (`apply_includes` with
  `overwrite: true`). The worktree's copy is the one about to be lost, so it
  wins; skipping would silently keep stale content from an earlier run.

Destinations, after rendering:

- A `to` that renders empty or relative warns and the entry is skipped. An empty
  path resolves against the process cwd, which would write the archive into
  whatever directory the user happened to run from.
- A `to` that resolves inside any worktree in this run's removal set warns and
  the entry is skipped. Preserving into a tree that is about to be deleted
  destroys the copy; preserving into another selected worktree races its removal.

Globbing uses one consistent, non-canonicalized spelling of the worktree path.
`cleanup` canonicalizes its target, and on Windows that yields a verbatim path;
`glob 0.3.3` accepts a `\\?\C:\` prefix but silently matches nothing behind
`\\?\UNC\`, so a repo on a network share would preserve nothing without a
warning.

## Where it runs

`run` becomes three phases instead of a confirm-and-spawn loop.

1. **Confirm.** Prompt for every selected worktree, collecting the approved ones.
   Every prompt now precedes every action, rather than interleaving with removals
   already running in background threads.
2. **Preserve.** Serially, in worktree order and then sorted entry order: read
   `.devkit/issue.toml`, render, validate, copy. One progress step per entry.
   A worktree whose `required` entry fails is dropped from the removal set with
   its error reported; every other worktree continues.
3. **Remove.** The existing parallel `cleanup` threads, then the single
   post-join `git worktree prune`.

Preservation is serial and complete before the first removal, which is what makes
the guarantees above hold: a destination collision resolves in a defined order
rather than between racing threads, a `required` failure is known while every
file still exists, and no cleanup thread has to write warning lines through the
live progress renderer.

`cleanup` itself is unchanged. It takes no preserve settings and keeps its
current signature and order.

Preservation runs on every removal path, `--clean-worktree` and `--force`
included. Those are the runs most likely to be discarding work. `--no-preserve`
skips phase 2 entirely.

## Failure policy

Fail-open, like `[hooks]` and like `copy_includes`. A pattern that cannot render,
a rejected pattern or destination, a bad glob, an unreadable source, or a copy
that fails prints a `warning:` line naming the entry, and the worktree is removed
anyway. Most worktrees carry nothing worth preserving, and holding a cleanup
command hostage to an archive that was never going to have contents is the wrong
default.

Two things do not even warn: an entry whose patterns match nothing, and a pattern
that renders to the empty string. An empty `graphify-out/` is the normal case.

`required = true` flips one entry to fail-closed. Its warnings become an error,
that worktree is dropped from the removal set, and its worktree, branch, and
summary stay intact so a rerun after fixing the config still finds the files.
`required` governs errors only, never emptiness, so an entry marked required on a
worktree that produced no reflections still removes cleanly.

`run` currently returns `Ok(())` whatever happens. It gains a failure tally and
returns an error after the removals join and the prune runs, so a `required`
failure is visible to a script through the exit code rather than only in the
printed output.

A config that fails to load is fatal to `issue end` unless `--no-preserve` is
passed. `tracker::select` turns every load error into `None`, which for a
read-only command degrades gracefully but here would silently produce an empty
preserve table and remove the worktree having preserved nothing. `devkit-config`
already distinguishes the cases through `Health::{Absent, Broken}`: absent is
fine and means no preservation is configured, broken stops the command.

## Progress output

`issue end` today draws a spinner for the status fetch and one per removal.
Preservation adds one labelled step per entry per worktree, drawn from the main
thread during phase 2, matching how `issue setup` draws one step per hook:

```
  Preserving graphify for ENG-1234…
  Preserving notes for ENG-1234…
  Removing ENG-1234…
```

The final line reports what was archived alongside what was removed, so a run
that preserved nothing says so rather than looking identical to one that did:

```
Preserved 14 file(s) across 2 entries. Removed 3 of 3.
```

`Steps` keeps its current untotaled `Steps::persistent()` construction. A total is
computable now that confirmations precede the work, but `persistent_with_total`
would need the fetch step counted before the confirm phase knows the total, and a
labelled step per entry already answers the "is this hung?" question that the
counter would.

## Code

**`crates/devkit-common/src/worktree.rs`.** Two changes.

`plan_includes` skips a pattern that is empty after its `trim_end_matches('/')`.
This is a latent bug on the existing inbound path too: `source.join("")` is the
source directory, glob matches it, `strip_prefix` yields `""`, and `plan_dir`
then plans every file under the root. Guarding the shared function fixes
`worktree_include` and preservation in one place. (Escaping patterns are *not*
guarded here — that policy is preservation's and lives in `copy_out`, because the
inbound path has a different risk profile and is out of scope.)

A `copy_out` beside `copy_includes`:

```rust
/// Copy files matching `patterns` (globs relative to `source`) out of a worktree
/// into `dest`, at the same relative path, overwriting what is already there.
/// The outbound counterpart to `copy_includes`, and fail-open the same way:
/// returns (files_copied, warnings). Patterns that are absolute or contain a
/// `..` component are rejected, so a copy cannot escape either root.
pub fn copy_out(source: &Path, dest: &Path, patterns: &[String]) -> (usize, Vec<String>);
```

It differs from `copy_includes` in the overwrite flag and the pattern guard, but
the two names carry different policies at their call sites (a backfill never
clobbers and stays inside by convention; an archive always clobbers and enforces
containment), and putting the policy in the name is what keeps a future edit from
applying the wrong one.

**`crates/devkit-config/src/lib.rs`.** `PreserveConfig` with `JsonSchema`,
`Deserialize`, `Serialize`, and `deny_unknown_fields` derives, plus the
`Config.preserve` field.

**`src/bin/devkit/issue/end.rs`.** `run` restructured into the three phases. A
`preserve` module function owns phase 2: it takes the approved rows, the preserve
entries in sorted key order, `templates.variables`, `worktree_root`,
`branch_prefix`, and the removal set (for the destination containment check), and
returns per-worktree outcomes plus the archived-file tally the summary line
prints.

**`src/bin/devkit/issue/tracker.rs`.** `select` loads the config and drops
everything but `tracker.kind` and `github`. It gains a sibling returning the load
outcome — the `Config` plus which `Health` it came from — and `select` is
rewritten in terms of it, so the three other callers keep their current
degrade-to-detection behavior while `end` can refuse on `Broken`.

## Docs and schema

- `docs/configuration.md`: a `[preserve.<name>]` section covering the three keys,
  the render context table, the pattern and destination rules, the fail-open
  contract with `required` as the opt-out, and the symlink and non-atomic-copy
  caveats.
- `docs/commands.md`: the `issue end` entry gains the preservation phase,
  `--no-preserve`, and the broken-config refusal.
- `AGENTS.md`: correct the sentence naming `[github]` as the only table with
  `deny_unknown_fields`.
- `schema/devkit-config.json`: regenerated with `DEVKIT_UPDATE_SCHEMA=1 cargo test`
  and committed, which the `tests/config_schema.rs` drift test enforces.

## Testing

In `crates/devkit-common/src/worktree.rs`:

- An empty pattern plans nothing, asserted through `plan_includes` so it covers
  the inbound path, and again through `copy_out`. This is the catastrophic case:
  without the guard it copies the whole tree.
- A slash-only pattern is likewise skipped.
- `copy_out` lands a match at `<dest>/<relative path>` and copies a directory
  match recursively.
- `copy_out` overwrites an existing destination file where `copy_includes` leaves
  it alone. Both assertions in one test, so the policy difference is visible.
- `copy_out` rejects an absolute pattern and a `..` pattern, warning on each and
  writing nothing outside `dest`.
- A pattern matching nothing yields zero files and no warnings.

In `src/bin/devkit/issue/end.rs`, over the git fixture the existing `cleanup`
tests already build:

- Phase 2 copies a matching file to the templated destination, and the worktree
  is then removed.
- A failing entry that is not required warns and the worktree is still removed.
- A failing entry that *is* required leaves the worktree, its branch, and its
  summary intact, drops only that worktree from the removal set, and makes `run`
  return an error.
- A required entry whose patterns match nothing removes the worktree cleanly.
- A `to` that renders empty is skipped, and nothing is written to the process
  cwd.
- A `to` resolving inside a worktree in the removal set is skipped.
- Two worktrees whose destinations collide resolve in worktree order, and the
  result is the same across repeated runs.
- An empty `preserve` table removes worktrees exactly as it does today.
- `--no-preserve` skips a configured entry.
- A broken config makes `issue end` refuse, and `--no-preserve` lets it through.
- Template rendering resolves `worktree_root` and `primary`, and a malformed
  record renders `issue` empty without failing.

`cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
and `cargo fmt --all --check` all stay green on ubuntu, macos, and windows.

## Open questions

None outstanding. One decision was made rather than deferred:

- **`to` is always a directory.** Naming an exact destination filename, and so
  renaming a file on the way out, is not supported. Two worktrees archiving the
  same filename to the same `to` collide, and worktree order decides; templating
  `{{ issue }}` into `to` is how a project avoids that.
