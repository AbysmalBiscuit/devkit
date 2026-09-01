# Hooks on `issue end` — design

## Goal

Let a project run commands when `issue end` takes a worktree away, the mirror of
what `hooks.after_worktree_create` already does when `issue setup` and
`issue checkout-pr` create one.

Two events, because the removal has two shapes worth reacting to. Per worktree:
drop it from an editor's project list, remove it from a shell's directory
frecency, notify a tracker. Per run: refresh a tool whose view covers every
worktree at once, where firing it once per removed worktree would be N redundant
calls.

## Scope

**In:** two keys on the existing `[hooks]` table, `after_worktree_remove` and
`after_end`; their firing sites in `issue end`; extraction of the hook runner
out of `setup.rs` so both commands share it; the docs and JSON schema that
follow from new config keys.

### Non-goals

- **A `before_worktree_remove` hook.** The `[preserve.<name>]` table already
  covers copying files out before removal, declaratively and without a shell.
  A pre-removal hook is a separate feature with its own failure question (does a
  non-zero exit block the removal?), and nothing needs it yet.
- **Nested hook subtables** (`[hooks.issue.end]`). Rejected below.
- **`deny_unknown_fields` on `HooksConfig`.** Rejected below.
- **Hooks on a removal devkit did not perform.** A bare `git worktree remove` is
  outside devkit and stays that way, the same boundary `[preserve]` draws.
- **Any MCP surface change.** `issue` actions on MCP are read-only and
  `issue end` is not among them.
- **Parallel hook execution.** Removals run in parallel; hooks do not.

## Background

`HooksConfig` in `crates/devkit-config/src/lib.rs` holds one key today,
`after_worktree_create: Vec<Vec<String>>`. Its runner lives in
`src/bin/devkit/issue/setup.rs`: `render_hook` expands each argv element as
minijinja over a context plus `[templates.variables]`, `run_rendered` calls
`devkit_common::cmd::capture` with an explicit cwd, `hook_label` builds the
progress-step label, and `run_after_worktree_create` ties them together with the
fail-open warning.

`issue end` (`src/bin/devkit/issue/end.rs`) runs in phases: gather and gate,
confirm each worktree, preserve files serially, remove in parallel under a
`std::thread::scope`, then a single `git worktree prune` once every removal has
joined. It counts removals into an `AtomicUsize` and never records which
worktrees the count came from.

## Design

### The keys

| Key | Fires | Cwd |
|---|---|---|
| `after_worktree_create` | unchanged | the new worktree's root |
| `after_worktree_remove` | once per worktree `issue end` actually removed | main repo root |
| `after_end` | once per `issue end` run that removed at least one worktree | main repo root |

```toml
[hooks]
after_worktree_create = [["zoxide", "add", "{{ worktree }}"]]
after_worktree_remove = [["zoxide", "remove", "{{ worktree }}"]]
after_end = [["alacritree.exe", "project", "refresh"]]
```

### Cwd is the main repo root

The create hook runs in the worktree it just made. That path no longer exists
here, so both new hooks run in the main repo root, which `end.rs` already
resolves as `main_repo(start)` for the post-join prune.

Inheriting the caller's cwd is not an option. `issue end` is usually run from
inside the worktree being removed, so the process cwd is a deleted directory by
the time a hook would spawn.

When `main_repo` does not resolve, both hook sets are skipped with one
`warning:` line naming what was skipped. The command still succeeds: a resolution
failure already just skips the prune, and hooks are fail-open.

### Firing conditions

`after_worktree_remove` fires for a worktree whose `cleanup` returned `Ok`.
A worktree kept back by a required `[preserve]` failure, refused as dirty, or
skipped at the confirmation prompt never fires it.

`after_end` fires when at least one worktree was removed. The early returns
("Nothing finished to clean up.", "Nothing to remove.", no matching worktrees)
return before the removal phase and change nothing on disk, so they fire
nothing. A run whose every removal failed also fires nothing, for the same
reason: a consumer refreshing a view has nothing new to see.

### Ordering

Removals stay parallel. Hooks are serial: the removal scope joins, the prune
runs, then every `after_worktree_remove` fires in approval order, then
`after_end` fires once. Serial execution keeps the documented "hooks run in the
order listed" contract and avoids N concurrent spawns of a tool that may not be
reentrant.

### Context

`after_worktree_remove` renders against the same context `[preserve]` entries
get, built by `crate::issue::preserve::context`: `worktree`, `branch`, `issue`,
`slug`, `apps`, `prefix`, `worktree_root`, `primary`, plus
`[templates.variables]`.

`issue`, `slug` and `apps` come from the worktree's `.devkit/issue.toml`, which
is gone once the worktree is. Contexts are therefore built for every approved
worktree *before* the removal phase and looked up afterwards by worktree path.
The existing preserve loop reads the same record but only when `[preserve]`
entries exist, so this is a separate pass, skipped entirely when
`after_worktree_remove` is empty.

`after_end` renders against `removed` (the removed worktree paths, in approval
order), `count`, `prefix`, `worktree_root`, and `primary`, plus
`[templates.variables]`. It gets no `worktree`, `branch`, `issue` or `slug`: a
run may have removed several, and there is no honest single value.

### Fail-open, unchanged

A hook that cannot render, cannot spawn, or exits non-zero prints a `warning:`
line naming its key and continues to the next hook. Neither key can turn a
successful removal into a failed command. Output is captured and discarded.

### The runner moves

`render_hook`, `run_rendered`, `hook_label` and the loop move from `setup.rs`
into a new `src/bin/devkit/issue/hooks.rs`, with the loop generalized to take
the config key name for its warning text. `setup.rs` keeps a thin
`run_after_worktree_create` that calls it, so its existing call site and unit
tests are unchanged in behavior.

## Rejected alternatives

**Nested subtables keyed by command** (`[hooks.issue.end]`). The existing
`after_worktree_create` fires from two commands, so a command-keyed tree either
duplicates it under `[hooks.issue.setup]` and `[hooks.issue.checkout-pr]` or
hoists it to a parent level. The duplicate is not cosmetic: arrays replace
wholesale across config layers rather than appending, so two copies drift
independently and every edit becomes a two-site edit. The mixed tree that avoids
the duplication makes "which subtable does my hook go in?" depend on how many
commands happen to reach the event, which is the confusion nesting was meant to
remove. The leaf key also degrades: `after = [["prog"]]` under a subtable says
less than `after_worktree_remove = [["prog"]]` says on its own, and `devkit
schema` output scatters the when-it-fires answer across three struct levels
instead of listing it as sibling properties with doc comments.

**Renaming `after_end` to `after_cleanup`** to preserve the documented rule that
an event names a state change rather than the caller. The rule exists to stop
one event needing duplicate keys for multiple callers, and a run-level hook has
exactly one caller by construction. `after_cleanup` borrows an internal function
name and still leaves the reader looking up which command cleans up.
`after_end` maps to the command typed. The docs rule is amended instead: the
table holds per-worktree state events and run-level command events, and says
which each key is.

**`deny_unknown_fields` on `HooksConfig`.** It would catch a misspelled hook key,
which today silently never fires. The cost is worse than the bug: `[hooks]` is
designed to grow, and a hard error means an older devkit binary refuses to load a
config naming a newer hook key. Several worktrees on one machine run different
builds, so this trades a silent typo for a broken command. `[github]` and
`[preserve.<name>]` carry the attribute because neither grows this way.

## Testing

No integration test exercises `after_worktree_create` today; its coverage is unit
tests in `setup.rs`. The new work matches that rather than building an
`issue end` harness, which would need a tracker, PR state, and the finished gate.

In `hooks.rs`: the moved render and label tests, plus a test that runs a real
command writing a marker file into a `tempfile::tempdir()` and asserts both the
rendered arguments and the cwd, and a test that a missing program leaves the
remaining hooks running.

In `end.rs`: the removal phase reports which worktrees it removed, not just how
many; contexts built before removal survive it; and the `after_end` context
carries the removed paths in approval order.

## Open questions

None.
