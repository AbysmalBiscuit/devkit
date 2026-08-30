# Worktree include progress design

## Problem

`issue setup` and `issue checkout-pr` copy the `defaults.worktree_include`
files into a new worktree by calling `backfill_includes`
(`src/bin/devkit/issue/setup.rs`). That call sits between the record write and
per-app prep, and it draws nothing. Neither call site counts it in the step
total either.

For a small include list nobody notices. For an include that reaches a large
directory the command sits with no output for the whole copy. On one measured
project the include list resolves to 11,100 files and 363 MB, where
`plan_includes` takes about 1 second and `apply_includes` takes 13 seconds. A
14 second silent gap in the middle of a numbered step log reads as a hang.

Two separate silences are involved: the glob and directory walk that builds the
plan, and the copy that applies it.

## Goal

Give the include backfill a numbered step of its own, with sub-progress fine
grained enough that the user can tell work is happening. Nothing about which
files get copied changes.

## What it looks like

The include step is one numbered step in the parent log. Beneath it sit
sub-steps, one per `worktree_include` entry, plus a leading `discovering files`
sub-step when the pattern list warrants one.

Sub-step lines persist. Their count is bounded by the config list, so a
scrollback record of them is small and useful. The per-file detail inside a
sub-step is unbounded, so it lives in a transient line that is rewritten in
place and cleared when the sub-step ends.

While the plan walk runs:

```
✓ 1. Fetching from origin… (0.8s)
✓ 2. Creating worktree… (1.4s)
[3/10] Copying worktree includes…
         [1/4] discovering files… (340 found)
```

While a pattern is being copied:

```
[3/10] Copying worktree includes…
         [3/4] .claude/hooks/ 128/312
```

Once the step settles:

```
✓ 1. Fetching from origin… (0.8s)
✓ 2. Creating worktree… (1.4s)
     ✓ 1/4 discovering files (340 found)
     ✓ 2/4 .tool-versions
     ✓ 3/4 .claude/hooks/
     ✓ 4/4 apps/*/.env.local
✓ 3. Copying worktree includes… (340 files, 2.1s)
✓ 4. git init after (0.4s)
```

A sub-step line persists at the moment that entry finishes, so the sub-steps
land above the parent's settled line rather than below it. `MultiProgress`
prints above its live bars, and the parent's bar is still live while its
sub-steps complete. Buffering the sub-steps to print them under the parent
would mean nothing persists until the whole step is over, which gives up the
live record of which entries are already done. The group still reads as a
group, closed by its own summary rather than opened by it.

An empty or undefined `worktree_include` mints no step at all, which is what
happens today, and contributes nothing to the parent step total.

## Design

### The discovery sub-step is decided from the pattern strings

The sub-step denominator has to be known before the first sub-step draws, which
rules out deciding anything from what the patterns match. It is decided from
the pattern text instead:

```
needs_discovery = patterns.iter().any(|p| p.ends_with('/') || is_glob(p))
is_glob(p)      = p contains any of '*', '?', '['
```

`?` and `[` are in the predicate because the `glob` crate treats all three as
wildcards; checking only `*` would miss a pattern that is just as expensive.

A pattern ending in `/` is declared a directory by the config convention
already documented in `docs/configuration.md`, and a directory include triggers
the recursive walk that is the expensive half of the plan phase. Leaving it out
of the predicate would skip the discovery sub-step for exactly the case that
needs it.

Sub-step count is therefore `patterns.len() + usize::from(needs_discovery)`,
known from config alone.

A literal file path like `.tool-versions` needs neither, so a project whose
whole include list is literal files sees no discovery sub-step, only one
sub-step per entry.

### `IncludePlan` gains per-pattern provenance

Per-entry copy sub-steps need to know which pattern each matched file came
from. Today the plan flattens everything into two sorted vectors and loses
that.

```rust
pub struct PatternPlan {
    pub pattern: String,
    pub missing: Vec<PathBuf>,
    pub existing: Vec<PathBuf>,
}

pub struct IncludePlan {
    pub patterns: Vec<PatternPlan>,
    pub warnings: Vec<String>,
}

impl IncludePlan {
    pub fn missing(&self) -> impl Iterator<Item = &Path>;
    pub fn existing(&self) -> impl Iterator<Item = &Path>;
    pub fn missing_len(&self) -> usize;
    pub fn existing_len(&self) -> usize;
}
```

Each `PatternPlan`'s vectors stay sorted, as the flat ones are today, so
rendering does not vary with filesystem iteration order. The flattening
iterators yield in pattern order, then sorted within a pattern. That is a
different order than today's globally sorted vectors, which matters only to
`sync.rs` display and is acceptable there.

Sorting stays per-pattern rather than global because a global sort would
require re-grouping to drive the sub-steps.

`plan_includes` and `apply_includes` keep their signatures. `apply_includes`
iterates `plan.patterns` instead of the two flat vectors, which is where a
future parallel copy slots in.

### A progress seam that a parallel copy can drive

```rust
pub enum IncludeEvent<'a> {
    /// Running count of files matched so far, across the whole plan walk.
    Found { files: usize },
    /// The plan walk finished with `files` matched.
    ScanDone { files: usize },
    /// The copy started for `pattern`, entry `index` of `of`, with `files` to copy.
    EntryStart { pattern: &'a str, index: usize, of: usize, files: usize },
    /// One file of `pattern`'s worklist is done: copied, skipped because the
    /// destination already existed, or failed. `done` and `of` are within that
    /// entry, and may arrive out of order. The display advances per file
    /// handled rather than per file written, so a run that skips everything
    /// still moves.
    FileDone { pattern: &'a str, done: usize, of: usize },
    /// The copy finished for `pattern`, having copied `copied` files.
    EntryDone { pattern: &'a str, index: usize, of: usize, copied: usize },
}

pub fn plan_includes_with(
    source: &Path,
    dest: &Path,
    patterns: &[String],
    on: &(dyn Fn(IncludeEvent) + Sync),
) -> IncludePlan;

pub fn apply_includes_with(
    source: &Path,
    dest: &Path,
    plan: &IncludePlan,
    overwrite: bool,
    on: &(dyn Fn(IncludeEvent) + Sync),
) -> (usize, Vec<String>);

pub fn copy_includes_with(
    source: &Path,
    dest: &Path,
    patterns: &[String],
    on: &(dyn Fn(IncludeEvent) + Sync),
) -> (usize, Vec<String>);
```

The three existing functions delegate to these with a no-op closure, so
`sync.rs` and every existing test compile against unchanged signatures.

The callback is `&dyn Fn + Sync` rather than `&mut dyn FnMut` on purpose. A
parallel copy is being built in a separate session, and worker threads must be
able to emit `FileDone` directly. `indicatif::ProgressBar` takes `&self` and is
`Sync`, so the renderer needs no lock of its own; the per-entry `done` counter
is an `AtomicUsize` inside `worktree.rs`.

`index` and `of` are the entry's position in the `worktree_include` list, not a
display number. Adding one for the discovery sub-step is the renderer's job, so
`worktree.rs` stays ignorant of how the step log is laid out.

### Event volume is throttled at the renderer

`Found` and `FileDone` fire per file, so 11,100 files means 11,100 callback
calls, each of which would otherwise allocate a message string. The renderer
keeps its own counter and rewrites the transient line only when the count has
advanced by at least 64, always rendering the final value on `ScanDone` and
`EntryDone`. The emitting side is not throttled, so the event stream stays
exact and testable.

### `Steps` learns a step that owns its bar

`during` and `during_result` build the spinner internally, so nothing can touch
it mid-step. Add:

```rust
pub struct Step<'a> { /* … */ }

impl Step<'_> {
    /// Rewrite the transient line beneath this step. Created on first call.
    pub fn activity(&self, msg: &str);
    /// Persist a finished sub-step line beneath this step, and clear the
    /// transient line.
    pub fn substep(&self, msg: &str);
    /// Text folded into the settled line's parens, ahead of the elapsed time.
    pub fn detail(&self, d: &str);
}

impl Steps {
    pub fn during_step<T>(
        &self,
        msg: &str,
        f: impl FnOnce(&Step<'_>) -> anyhow::Result<T>,
    ) -> anyhow::Result<T>;
}
```

`during_result` becomes `during_step(msg, |_| f())`, so its output is unchanged
when no detail is set.

`Step`'s methods take `&self` so a `Fn` callback can call them. Interior
mutability covers the detail string and the lazily created transient bar.

`finish_line` currently renders `{mark} {label} ({elapsed})`. With a detail set
it renders `{mark} {label} ({detail}, {elapsed})`, which is why the file count
reads `(340 files, 2.1s)` and no new separator is introduced.

Persisted sub-step lines print through the group's `println`, indented, so they
are discarded along with everything else when stderr is not a terminal.

### Call sites

`backfill_includes` takes `&Steps`. It still returns early on an empty pattern
list, before minting a step, so an empty list still costs nothing and still
draws nothing.

Inside the step it computes `needs_discovery`, calls `plan_includes_with` then
`apply_includes_with`, and maps events onto `Step`:

- `Found` and `ScanDone` drive the discovery sub-step, and are ignored for
  display when `needs_discovery` is false.
- `EntryStart` opens a transient line for that entry.
- `FileDone` rewrites it, throttled.
- `EntryDone` persists the entry's sub-step line.

Warnings keep going to stderr as they do today, printed after the step settles
so a live bar does not tear them.

`setup.rs` adds `usize::from(!cfg.defaults.worktree_include.is_empty())` to
`total`. `checkout.rs` uses the unnumbered `Steps::persistent()`, so it only
passes `&steps`.

`sync.rs` reads `plan.missing` and `plan.existing` in about five places. Those
become the flattening iterator calls, and `list()` takes an iterator instead of
a slice. Its grouping and verbose behaviour are unchanged. `sync.rs` does not
gain progress display in this change.

### Fail-open behaviour is preserved

Every existing fail-open path is untouched. A bad pattern, an unreadable
directory, a non-UTF-8 pattern, or a copy error is still collected as a warning
string rather than propagated, and the backfill still never aborts worktree
creation. The step settles as a success even when warnings were collected,
because the warnings are the report and a failure mark would imply the worktree
is unusable.

## Testing

Progress bars are hidden when stderr is not a terminal, so the rendering itself
is not assertable in tests. The event stream is, and that is what the tests
cover. Each test collects events into a `Mutex<Vec<...>>` behind the `Fn`
callback.

1. `plan_includes_with` emits `Found` counts that increase monotonically and a
   final `ScanDone` whose `files` equals the plan's total match count.
2. `apply_includes_with` emits one `EntryStart` and one `EntryDone` per pattern,
   in pattern order, and `EntryDone.copied` equals the files that entry actually
   wrote.
3. `apply_includes_with` emits one `FileDone` per file in an entry's worklist, with `done` ending
   at `of` for each entry.
4. `IncludePlan::missing()` and `existing()` yield exactly what today's flat
   vectors hold, as a set. This is the regression guard on the struct change.
5. `needs_discovery` is true for `apps/*/.env.local`, for `.claude/hooks/`, and
   for a `[abc]` pattern, and false for a list of literal file paths.
6. `backfill_includes` consumes exactly one step with a non-empty pattern list
   and zero with an empty one, asserted through `Steps::started()`, mirroring
   `every_hook_consumes_a_step_even_when_it_cannot_render` in `setup.rs`.
7. `Steps::during_result` output is unchanged after being reimplemented on
   `during_step`, asserted through the existing step-count tests in
   `progress.rs`.

## Out of scope

- Parallelising the copy. A separate session is building that on top of this
  branch, parallelising within a pattern so the per-entry grouping this design
  needs is preserved.
- Swapping `plan_dir`'s `child.is_dir()` for `entry.file_type()`.
  `DirEntry::file_type` does not traverse symlinks and `Path::is_dir` does, so
  the swap stops recursing into a symlinked directory inside an include. That
  is a behaviour change and needs its own test and commit.
- Progress display for `issue sync-includes`. It prints its own per-worktree
  report and prompts, and mixing live bars into that is a separate design.
- `docs/commands.md` and `docs/configuration.md`. This changes display only,
  and neither documents the backfill's output.

## Coordination

`parallel_includes` is building the parallel copy on branch
`parallel-includes`, based on main at 141cc4c, and has not touched
`worktree.rs`. This branch lands first and that one rebases onto it. The
overlap is the body of `apply_includes` and `plan_dir`, not the public
signatures.
