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
  dialect and its `MatchOptions` are preserved exactly.
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

Only the walker is slow. Keeping `Pattern` is what makes this change invisible
to every `worktree_include` value already in use: same syntax, same
`MatchOptions`, same `PatternError` behind the existing "bad include pattern"
warning.

So both of today's walkers go, and jwalk replaces them:

- jwalk finds the files.
- `glob::Pattern` decides which found files count.
- rayon copies them.

The narrower alternative, keeping `glob_with` and swapping only `plan_dir`'s
recursion for jwalk, was rejected. It speeds up directory includes and leaves
`**` and `apps/*/` patterns exactly as slow as they are now, because `glob_with`
finds those files itself.

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

`parallelism` joins `STANDALONE_SECTIONS` (`crates/devkit-config/src/lib.rs:881`),
which lists the tables a config may carry without `[defaults]`. Without that, a
personal `~/.config/devkit/config.toml` holding only `[parallelism]` fails to
resolve. The three doc comments enumerating that list (lines 14, 250 and 878) and
`a_config_of_standalone_sections_needs_no_defaults` all name the new table.

Config reaches the pool through `pool::configure(n)`, which each binary calls
once after loading its config. `worktree.rs` reads no config today, and
threading one into `copy_includes` would push a machine setting through five
call sites that have no other use for it. `configure` must run before the first
pool use or it is ignored, which the binaries satisfy by calling it in `main`.

## The walk

`Walk::plan_one` and `Walk::plan_dir` collapse into one per-pattern function.
Per pattern:

1. Trim a trailing `/`; drop an empty pattern. Unchanged.
2. Build a `glob::Pattern`. A `PatternError` becomes the existing "bad include
   pattern" warning.
3. A pattern with no wildcard needs no walk. One `symlink_metadata` call
   classifies it: a link as a link, a file as a file, a directory as the root of
   a jwalk whose every entry is included.
4. A pattern with a wildcard starts a jwalk at the pattern's literal prefix, and
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
without rejoining the source prefix onto every path it yields.

It carries a behaviour change, taken deliberately. See [Behaviour
changes](#behaviour-changes).

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

Warnings currently emerge in walk order and will not. The walk's warnings and
the copy's warnings are each sorted within their own group before returning.
`copy_out`'s escaped-pattern warnings are produced before any walk and stay
first, so `copy_out_refuses_a_pattern_that_escapes_the_worktree` keeps passing on
index.

## Symlinks

`follow_links` maps one-to-one onto the existing `LinkMode`, so the mode stops
being a hand-rolled branch and becomes a walker setting. `Preserve` sets
`follow_links(false)`; `Follow`, used by `copy_out`, sets it true.

`DirEntry::file_type` returns the target's type when following and the link's own
type when not, and makes no syscall either way because jwalk caches it from the
directory read. The `is_symlink` helper's `symlink_metadata` call disappears for
every walked entry.

This settles a question the progress design left open. That design listed
swapping `plan_dir`'s `child.is_dir()` for `entry.file_type()` as out of scope,
because `file_type` does not traverse links while `Path::is_dir` does, so the
swap would stop recursion into a symlinked directory. Setting `follow_links` per
mode removes the tension: `Preserve` classifies the link before descending, and
`Follow` descends through it. Both match today's behaviour.

### Two jwalk defaults must be overridden

Both would be silent bugs.

`skip_hidden` defaults to true. Every include devkit is used for is a dotfile:
`.env.local`, `.claude/`, `.godot/`, `.tool-versions`. It is set to false.

`parallelism` defaults to `RayonDefaultPool`, the global pool this design exists
to avoid. It comes from `pool::jwalk_parallelism()`.

## Behaviour changes

Two, both deliberate, each with its own test and its own commit.

**An absolute include pattern now matches nothing.** Today the pattern is built
from `source.join(trimmed)`, so `/etc/passwd` replaces the base and glob walks
outside the source tree. `copy_out` filters these through `escapes()`;
`copy_includes` does not. Matching relative paths confines includes to the source
tree.

**A symlink cycle under `copy_out` warns instead of hanging.** `plan_dir`
recurses on `is_dir()`, which follows links, so a cyclic symlink loops forever
today. jwalk detects the cycle and reports it, which the fail-open warning path
already knows how to handle.

## Testing

The existing `worktree.rs` suite is the regression gate and passes unchanged,
with one exception. `the_plan_walk_reports_each_match_and_a_final_count` asserts
`found == vec![1, 2, 3]`. An atomic counter still hands out exactly 1 through 3
with no gaps or repeats, but the thread holding 2 can push before the thread
holding 1. The assertion's intent is the counter, not arrival order, so it sorts
before comparing. Serialising the walk to protect an ordering the test never
meant to assert would be the wrong fix.

New tests, in order of what they protect:

1. A `**` pattern plans the same file set as before. This is the
   dialect-preservation gate and the whole justification for keeping
   `glob::Pattern`.
2. Planning the same tree twice yields an identical plan. Guards the sorting
   against a later refactor quietly reintroducing walk-order output.
3. Walk-root extraction, as unit cases: a directory include, a mid-pattern
   wildcard, a leading `**`, and a literal path.
4. An absolute include pattern matches nothing.
5. A symlink cycle under `copy_out` warns and finishes.
6. An include copy invoked from inside pool work completes rather than blocking.

Verification of the performance claim is a one-off `hyperfine` run against a real
tree during implementation, recorded in this document. It does not land in CI.

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

`2026-06-30-worktree-include-backfill-design.md` rejects this work at lines 99
and 149. That rejection is left in place as history and corrected rather than
rewritten: the paragraph states that its workload assumption proved wrong, names
what was wrong about it, and points here.

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

## Unresolved questions

1. The performance claims come from a separate benchmarking session, not from
   this branch. The implementation records its own `hyperfine` numbers against a
   real tree in this document, and if the copy does not improve, the rayon half
   of the change is worth reconsidering on its own.
2. Whether every binary calls `pool::configure`, or only those that can copy.
   Calling it everywhere is one line each and cannot be forgotten later; calling
   it selectively means a future parallel feature in a binary that skipped it
   silently ignores the config. Leaning toward everywhere.
3. `busy_timeout` for `RayonExistingPool`. With the re-entry guard the pool
   always has a free thread when a walk starts, which is the condition jwalk
   documents `None` as safe for. Settling this needs the guard written, so the
   plan decides it rather than the design.
