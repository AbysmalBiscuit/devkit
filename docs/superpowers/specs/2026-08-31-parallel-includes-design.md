# Parallel worktree includes

## Problem

`worktree_include` copying is single-threaded in both of its phases, and both
phases are slow on the trees people actually include.

Building an `IncludePlan` walks the filesystem twice over, through two different
walkers, and which one does the work depends entirely on the pattern:

| Pattern | `glob::glob_with` | `Walk::plan_dir` |
|---|---|---|
| `.godot/` | one stat, returns the directory | walks the whole subtree |
| `apps/*/.env.local` | walks `apps/` to expand `*` | never runs |
| `**/.env.local` | walks the entire source tree | never runs |
| `.tool-versions` | one stat | never runs |

Neither walker uses more than one thread. `apply_includes_with` then copies the
planned files one at a time.

The workloads that hurt are large directory includes (a Godot project's
`.godot/` is hundreds of megabytes of small files, and a graphify graph adds
more) and `issue sync-includes`, which repeats the whole plan-and-copy cycle
once per worktree on the machine.

`2026-06-30-worktree-include-backfill-design.md` rejected parallelism on the
assumption that includes are "a handful of small files". That assumption was
wrong for the projects devkit is used on. See [Amending the backfill
spec](#amending-the-backfill-spec).

## Goal

One walk implementation, running on a bounded thread pool shared across the
workspace, feeding a copy that runs on the same pool. Every pattern shape
benefits, not only directory includes.

Reported from a separate benchmarking session on a real tree: rayon roughly
halves copy time, and jwalk cuts walk time to about a third. The throughput knee
for the copy was four threads.

## Non-goals

- Changing what any existing `worktree_include` pattern matches. The pattern
  dialect is preserved. The *effective* match options are preserved, which is
  not the same as copying the `MatchOptions` value the code builds today. See
  [Match options](#match-options).
- Parallelising `issue sync-includes` across worktrees. The re-entry guard below
  makes that safe to attempt later, but it is a separate design.
- Parallelising `devkit-docs`. It reuses `devkit_common::pool` under its own
  spec.
- Parallelising `prep_apps` or anything under `devrun`.
- A benchmark in the repository. CI runners measure their own disks.

## Approach

`glob` is two separable halves. `glob::glob_with` is a walker; `glob::Pattern`
is a matcher that answers `matches_path_with(&Path, MatchOptions)` and never
touches the filesystem.

Only the walker is slow. Keeping `Pattern` is what leaves every
`worktree_include` value already in use meaning what it means today: the same
syntax, and the same `PatternError` behind the existing "bad include pattern"
warning. The match options need one correction to stay equivalent, below.

So both of today's walkers go, and jwalk replaces them:

- jwalk finds the files.
- `glob::Pattern` decides which found files count.
- rayon copies them.

The narrower alternative, keeping `glob_with` and swapping only `plan_dir`'s
recursion for jwalk, was rejected. It speeds up directory includes and leaves
`**` and `apps/*/` patterns exactly as slow as they are now, because `glob_with`
finds those files itself.

### Match options

`plan_with_mode` builds `require_literal_separator: false` and hands it to
`glob_with`, which throws it away. glob's own documentation is explicit
(`glob-0.3.3/src/lib.rs:176`): the options reach `matches_with` unchanged "with
the exception that `require_literal_separator` is always set to `true`
regardless of the value passed to this function". The walker also matches one
path component at a time, so a single `*` cannot span a `/` today no matter what
the caller asks for.

That `false` is therefore dead code, and copying it into `matches_path_with`,
which does honour the flag, would silently widen every pattern. `apps/*/.env.local`
would start matching `apps/a/b/.env.local`; `*.local` would match at any depth
instead of the root. A worktree would receive files nobody asked for, and no
existing fixture is deep enough to notice.

Relative matching therefore uses `require_literal_separator: true`. `**` still
recurses, through `AnyRecursiveSequence`, which is exactly the configuration
`glob_with` runs. `case_sensitive: true` and `require_literal_leading_dot: false`
carry over unchanged.

## The shared pool

A new module, `crates/devkit-common/src/pool.rs`, owns one
`rayon::ThreadPool` behind a `OnceLock`, built on first use. Every crate in the
workspace depends on `devkit-common`, so this is the pool later parallel work
uses rather than each feature building its own.

rayon's global pool is never touched. A feature that reaches for
`par_iter` without going through this module gets the global pool and its own
thread count, which is the collision this module exists to prevent.

### Re-entry

A bounded pool re-entered from its own workers can exhaust itself: jwalk blocks
waiting for a thread, and with every thread already inside pool work, none
frees. jwalk names this case in its own documentation, and reports it as
`ThreadpoolBusy` rather than deadlocking.

The module owns the rule so no caller has to remember it:

```rust
/// Run `f` on the shared pool, or on the calling thread when already inside it.
pub fn install<R: Send>(f: impl FnOnce() -> R + Send) -> R;

/// `RayonExistingPool` normally, `Serial` when called from a pool worker.
pub fn jwalk_parallelism() -> jwalk::Parallelism;
```

Nested parallel work degrades to serial. It never blocks and never fails.

### Sizing

Threads resolve from `DEVKIT_THREADS`, then `[parallelism] threads`, then 4.

```toml
[parallelism]
threads = 4
```

```rust
/// Width of the shared worker pool. Machine tuning, so it belongs in the
/// personal layer rather than a project's `devkit.toml`.
#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct ParallelismConfig {
    /// Threads in the shared pool. `DEVKIT_THREADS` wins; unset means 4.
    pub threads: Option<NonZeroUsize>,
}
```

A table of its own, not a key on `[defaults]`. `Defaults` documents itself as
"project-wide paths and branch conventions" and a thread count is neither.
`DaemonConfig` is the precedent: machine tuning, its own table, each key with an
env override.

`Option<NonZeroUsize>` rather than `usize` with a serde default. The default
lives once, in `pool.rs` beside `DEVKIT_THREADS` and the reason it is four, so
`None` honestly means the config did not speak. `NonZeroUsize` also makes
`threads = 0` a deserialize error and a `minimum: 1` in the schema, which matters
because `ThreadPoolBuilder::num_threads(0)` means one thread per core, the exact
opposite of what someone writing `threads = 0` intends. That is a schema
constraint instead of a runtime clamp.

No `deny_unknown_fields`. The workspace reserves it for `[github]` and
`[preserve.*]`, where a silent typo changes which repository is addressed or
leaves files unprotected. A typo here leaves the default of 4 in place, which is
a performance difference, and diluting the convention would make it mean less
where it matters.

`parallelism` joins the `STANDALONE_SECTIONS` const in
`crates/devkit-config/src/lib.rs`, which lists the tables a config may carry
without `[defaults]`. Without that, a personal `~/.config/devkit/config.toml`
holding only `[parallelism]` fails to resolve. Every doc comment enumerating that
list, and `a_config_of_standalone_sections_needs_no_defaults`, name the new table
too.

An unparseable or zero `DEVKIT_THREADS` is ignored, falling through to config and
then to 4. `NonZeroUsize` guards only the config path.

Config reaches the pool through `pool::configure(n)`. `worktree.rs` reads no
config today, and threading one into `copy_includes` would push a machine setting
through five call sites with no other use for it.

The call site is wherever a subcommand's config first resolves, immediately after
`load::load`. Not `main`: `main` parses argv and dispatches, and config loads
inside each subcommand with its own `--config` and start directory, so calling
`configure` from `main` would mean a second, wrongly-anchored config load.
`configure` must run before the first pool use, which that placement satisfies.

## The walk

`Walk::plan_one` and `Walk::plan_dir` collapse into one per-pattern function.
Per pattern:

1. Trim a trailing `/`; drop an empty pattern. Unchanged.
2. Build a `glob::Pattern`. A `PatternError` becomes the existing "bad include
   pattern" warning.
3. Reject a pattern that escapes the source tree, through the existing
   `escapes()`. See [Confining patterns to the source
   tree](#confining-patterns-to-the-source-tree).
4. A pattern with no wildcard needs no walk. One `symlink_metadata` call
   classifies it, and the classification is mode-aware. `Preserve` records a
   link as a link. `Follow` resolves through it with `metadata`, so a link to a
   file is a file and a link to a directory is a walk root. `copy_out`'s comment
   that "a Follow-mode plan holds no links" depends on this, and no existing test
   covers a `copy_out` pattern naming a link directly.
5. A pattern with a wildcard starts a jwalk at the pattern's literal prefix, and
   each walked path is tested with `matches_path_with`.

### The walk root

The literal prefix is the pattern's components up to the first one holding
`*`, `?` or `[`:

| Pattern | Walk root |
|---|---|
| `.godot/` | `.godot`, no wildcard, whole subtree included |
| `apps/*/.env.local` | `apps` |
| `**/.env.local` | the source root |
| `.tool-versions` | none, one stat |

`**/.env.local` walking the whole source tree is what `glob_with` does today. It
is not a regression, and it is where the parallel walk finally applies.

### A matched directory contributes its subtree

Today `.claude/hooks/` matches the directory through glob and `plan_dir`
supplies the files. Inside a single walk the same rule is a property of each
path: a path is included when it matches the pattern, or when any of its
ancestors does.

`rel.ancestors().any(...)` needs no shared state between threads, which is why it
is preferred over tracking matched directories in a set.

Only files and links are ever classified. A directory is never an entry in
`missing`, `existing` or `links`, whether it matched the pattern or merely
contained something that did, which is what
`plan_includes_directory_pattern_enumerates_files_not_the_directory` pins.

### Relative matching

Patterns are matched against paths relative to `source`, not against
`source.join(pattern)` as today. This is what lets one walk serve a pattern
without rejoining the source prefix onto every path it yields. Windows is
unaffected: glob's `chars_eq` treats `/` and `\\` as equivalent separators.

### Where each phase runs

The walk and the classification are two passes, not one.

jwalk's `process_read_dir` and its spawn closures are `'static`, while the event
callback is a borrowed `&(dyn Fn + Sync)` and `dest` is a borrowed `&Path`, so
classification cannot run inside jwalk's own workers without wrapping everything
in `Arc`.

Draining through `par_bridge` instead is worse: the classification consumers
would occupy the same four threads jwalk's readers need, starving the walk.

So the calling thread drains the jwalk iterator into a `Vec`, which costs no
syscalls because `file_type` is cached from the directory read. Matching, the
ancestor test, and the `dest.join(rel).exists()` check then run in one
`par_iter` over that batch on the shared pool. That existence check is a syscall
per candidate, roughly 75µs on a `/mnt/c` mount, so leaving it serial would cost
about 22 seconds on a 300k-file tree and undo the walk's gain.

This is why `found` is an `AtomicUsize` and the walk's warnings are a
`Mutex<Vec<String>>`. Draining and classifying serially instead would keep both
as plain values, and would be the wrong trade.

### Walk errors

jwalk yields `Err` for an unreadable root and for entries mid-walk, and the
mapping is not obvious in one direction.

`NotFound` at the walk root is silence. A wildcard pattern whose literal prefix
does not exist is silent today, because glob's scope check simply yields nothing,
and `apps/*/.env.local` in a repository with no `apps/` is a common benign
configuration. Warning on it would put a warning on every `issue setup`.

Every other walk error becomes a warning in the existing "reading dir" shape.

## The copy

`apply_includes_with` keeps its per-pattern loop sequential, so `EntryStart` and
`EntryDone` still bracket each pattern in configuration order. Parallelism goes
*inside* a pattern: first over its file worklist, then over its links.

This is the shape `2026-08-30-worktree-include-progress-design.md` specified when
it cut the `IncludeEvent` callback as `&dyn Fn + Sync` and noted that `FileDone`
may arrive out of order. That seam is used as designed rather than widened.

`copied` and `linked` become `AtomicUsize`; warnings become a
`Mutex<Vec<String>>`; the per-entry `done` counter is an `AtomicUsize`, as the
progress design anticipated.

Files and links run as two parallel phases rather than one mixed worklist. Link
creation is the failure-prone path on Windows, and keeping it separate keeps its
warnings and its counter easy to reason about.

## Determinism

A parallel walk yields paths in whatever order threads finish. Three things
depend on order, and all three survive.

Per-pattern sorting already runs after the walk in `plan_with_mode`. It stays,
and it becomes the thing that makes a plan deterministic rather than a
formality.

`claim_once` runs over sorted vectors in configuration order. Untouched.

Warnings currently emerge in walk order and will not. Warnings are sorted within
each pattern; patterns keep configuration order; `copy_out`'s escaped-pattern
warnings precede all of them, so
`copy_out_refuses_a_pattern_that_escapes_the_worktree` keeps passing on index.

## Symlinks

`follow_links` maps one-to-one onto the existing `LinkMode`, so the mode stops
being a hand-rolled branch and becomes a walker setting. `Preserve` sets
`follow_links(false)`; `Follow`, used by `copy_out`, sets it true.

`DirEntry::file_type` returns the target's type when following and the link's own
type when not. Under `follow_links(false)` it costs no syscall, because jwalk
caches it from the directory read, and the `is_symlink` helper's
`symlink_metadata` call disappears for every walked entry. Under
`follow_links(true)` jwalk does call `metadata`, plus `read_link` for
directories, so the saving applies to `Preserve` only.

This settles a question the progress design left open. That design listed
swapping `plan_dir`'s `child.is_dir()` for `entry.file_type()` as out of scope,
because `file_type` does not traverse links while `Path::is_dir` does. Setting
`follow_links` per mode removes the tension for entries that match a pattern or
sit under a matched directory.

It does not remove it for a symlink in the *middle* of a pattern, which is a
behaviour change. See [Intermediate symlinked
directories](#intermediate-symlinked-directories).

### Two jwalk defaults must be overridden

Both would be silent bugs.

`skip_hidden` defaults to true. Every include devkit is used for is a dotfile:
`.env.local`, `.claude/`, `.godot/`, `.tool-versions`. It is set to false.

`parallelism` defaults to `RayonDefaultPool`, the global pool this design exists
to avoid. It comes from `pool::jwalk_parallelism()`.

## Behaviour changes

Each gets its own test and its own commit.

### Confining patterns to the source tree

Today `copy_includes` will read outside its source tree. The pattern is built
from `source.join(trimmed)`, and `Path::join` discards the base for a rooted
pattern, on Windows as well as Unix, so `/etc/passwd` walks `/etc`. `copy_out`
filters these through `escapes()` because it deletes its source immediately
afterwards; `copy_includes` never has.

Relative matching does not fix this on its own. A pattern with no wildcard never
reaches `matches_path_with` at all, so `/etc/passwd` and `../outside` would still
be classified by the one `symlink_metadata` call in the no-wildcard path. The
confinement has to be an explicit rejection, applied to both paths, which is why
`escapes()` moves up into step 3 of the walk.

`escapes()` already covers `..` as well as rooted paths, so a `..` pattern is
rejected by the same gate. A rejected pattern produces the warning `copy_out`
already emits and keeps its entry in the plan, so entries still line up
one-to-one with the configured include list.

### Symlink cycles under `copy_out`

`plan_dir` recurses on `is_dir()`, which follows links, so a cyclic symlink loops
forever today.

jwalk detects a cycle by comparing the raw `read_link` output against its
ancestor list of absolute paths (`jwalk-0.9.0/src/core/dir_entry.rs:214`). That
catches an absolute-target cycle and reports it as a `Loop` error, which the
fail-open warning path already handles.

It does not catch a relative-target cycle. `ln -s .. loop` never equals an
absolute ancestor, so the walk keeps descending until the path length hits the OS
limit, planning a deeply duplicated subtree along the way. That is not worse than
today's infinite loop, but it is not fixed either, and the spec should not claim
otherwise.

An own canonicalising ancestor check would close it. Whether to build one is
[unresolved](#unresolved-questions).

### Intermediate symlinked directories

Today `glob_with` descends through a symlinked directory while expanding a
pattern's components, because its `is_directory` test follows links. With
`apps/web` a symlink to `../shared/web`, the pattern `apps/*/.env.local` plans
`apps/web/.env.local`.

Under `follow_links(false)`, jwalk gives a symlink entry no children
(`jwalk-0.9.0/src/lib.rs:365`), and `apps/web` does not itself match the pattern,
so that file silently disappears from the plan.

Silent disappearance is the worst available failure, so this needs a decision
rather than a default. It is [unresolved](#unresolved-questions).

## Testing

Two existing tests assert an order that parallelism removes. Both relax; neither
is protected by serialising the work it covers.

`the_plan_walk_reports_a_running_count_and_a_total` (worktree.rs:1331) asserts
`found == vec![1, 2, 3]`. An atomic counter still hands out exactly 1 through 3
with no gaps or repeats, but the thread holding 2 can push before the thread
holding 1. The assertion's intent is the counter, so it sorts before comparing.

`the_copy_brackets_each_pattern_and_counts_within_it` (worktree.rs:1380) asserts
an exact event log including `"file hooks/ 1/2"` then `"file hooks/ 2/2"`. Those
two can now arrive swapped. `FileDone` events sort within their entry;
`EntryStart` and `EntryDone` keep strict order, because the per-entry bracketing
is the thing the test exists to pin.

Every other test in the module passes untouched.

New tests, in order of what they protect:

1. **A single `*` does not cross a directory separator.** Source holds
   `apps/a/.env.local` and `apps/a/b/.env.local`; pattern `apps/*/.env.local`
   plans the first and not the second. This is the regression that the dead
   `require_literal_separator: false` would introduce, and it is the single most
   important test here. A `**`-only test cannot detect it.
2. **`**` still matches at zero directories.** Source holds `.env.local` at the
   root and `apps/a/.env.local`; pattern `**/.env.local` plans both.
3. **Planning the same tree twice yields an identical plan.** Guards the sorting
   against a later refactor quietly reintroducing walk-order output.
4. **Walk-root extraction**, as unit cases: `.godot/` gives `.godot`,
   `apps/*/.env.local` gives `apps`, `**/.env.local` gives the source root,
   `.tool-versions` gives none.
5. **An escaping pattern is rejected in both paths.** `/etc/passwd` (no wildcard,
   so the literal path) and `../*/x` (the wildcard path) each warn and plan
   nothing, through `copy_includes`, not only `copy_out`.
6. **A `copy_out` pattern naming a symlink directly archives its target's
   contents**, rather than planning a link. Nothing covers this today, and the
   mode-aware step 4 is what makes it pass.
7. **An absolute-target symlink cycle under `copy_out` warns and finishes.**
   `ln -s /abs/path/to/dir cycle`, not `ln -s ..`, because jwalk detects only the
   absolute form.
8. **An include copy invoked from inside pool work completes.** It hangs rather
   than fails when the re-entry guard regresses, so it carries a timeout.

Verification of the performance claim is a one-off `hyperfine` run against a real
tree during implementation, recorded in this document. It does not land in CI.

Windows is where this change is most likely to break and least likely to be
noticed locally: symlink creation needs Developer Mode, path separators differ,
and the CI runner is the slowest of the three. Every test above runs on all three
platforms rather than being gated to Unix.

## Documentation

- `docs/configuration.md` gains a `[parallelism]` section naming the
  `DEVKIT_THREADS` override and recommending the personal layer.
- `schema/devkit-config.json` is regenerated (`DEVKIT_UPDATE_SCHEMA=1 cargo
  test`), since a committed-schema drift test fails otherwise.
- `AGENTS.md` gains `pool` to the `devkit-common` module list, and a note that
  parallel work in this workspace goes through it rather than rayon's global
  pool.
- `docs/commands.md` is untouched. No command's behaviour or output changes.

## Amending the backfill spec

`2026-06-30-worktree-include-backfill-design.md` rejected this work. That
rejection is corrected in place rather than deleted: the paragraph now states
that its workload assumption proved wrong, names what was wrong about it, and
points here. Its out-of-scope entry for parallel directory walking points here
too.

Both edits landed with this design's own commit. Nothing remains to do.

The two later specs need no correction.
`2026-08-30-worktree-include-progress-design.md` cut its callback as `Sync` for
exactly this work, and `2026-08-30-worktree-include-symlinks-design.md` calls it
independent, which it remains.

## Decisions

**`glob` stays a dependency.** Only its walker is replaced. Reaching for
`globset` instead would swap the pattern dialect, changing what every existing
`worktree_include` value means, in exchange for nothing this design needs.

**One shared pool, not one per feature.** Several agent sessions run devkit
concurrently on one machine. Per-feature pools multiply thread count by feature
count with nothing coordinating them.

**Nested pool use degrades to serial rather than erroring.** The alternative is a
`ThreadpoolBusy` failure surfacing as a copy warning, which turns a performance
concern into a correctness one.

**Parallelism goes inside a pattern, not across patterns.** Across-pattern
parallelism would break the per-entry progress grouping and make `claim_once`'s
configuration-order rule race. The measured win is within a pattern anyway, since
a single directory include is the slow case.

**Walking and classifying are two passes.** Classifying inside jwalk's workers
means `Arc`-ing past its `'static` bounds; classifying through `par_bridge` means
consumers competing with readers for four threads. Draining first costs a `Vec`
of entries and buys a clean split.

**Confinement is an explicit rejection, not a consequence.** Relative matching
alone leaves the no-wildcard path free to stat anything `Path::join` resolves to,
so `escapes()` gates both paths. Getting this by accident is how the current
escape survived.

## Unresolved questions

**1. An intermediate symlinked directory silently drops its files.** See
[Intermediate symlinked directories](#intermediate-symlinked-directories). Three
ways out:

- Traverse them, matching today. Set `follow_links(true)` in `Preserve` too, and
  use `path_is_symlink()`, which is unaffected by the setting, to classify a
  matched link. A matched symlinked directory must then have its children
  cleared in `process_read_dir`, or jwalk descends into it before the plan can
  refuse. Exact, and the most code.
- Keep `follow_links(false)` and warn when the walk stops at a symlinked
  directory that did not match. The files are still lost, but not silently.
  Cheap and honest.
- Classify a blocking link as a planned link. The user gets the files through the
  reproduced link, but the worktree also gets everything else under it, which the
  pattern never asked for.

I lean toward warning. It is small, it makes the loss visible, and traversal can
follow if anyone hits it.

**2. Whether to close the relative-target symlink cycle.** jwalk catches only
absolute-target cycles. A canonicalising ancestor check would close the rest, at
one `canonicalize` per directory entered. `copy_out` is the only Follow-mode
caller and it runs on a worktree about to be deleted, so the exposure is narrow.
I lean toward leaving it, documented, since today's behaviour is an infinite loop
and this is already better.

**3. The performance claims come from a separate benchmarking session, not this
branch.** The implementation records its own `hyperfine` numbers here. If the
copy does not improve, the rayon half is worth reconsidering on its own.
