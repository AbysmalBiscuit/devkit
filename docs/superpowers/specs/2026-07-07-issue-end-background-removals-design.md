# Background parallel removals for `issue end` — design

Dispatch each confirmed worktree removal as a background task shown via its own
indicatif spinner, so the interactive `[y/N]` loop never blocks on a slow
`git worktree remove`. You keep walking the candidate list and deciding each
one while removals run concurrently behind you. Only the git-metadata steps that
share mutable main-repo state are serialized; the heavy filesystem deletion runs
in parallel.

## Decisions (confirmed with the user)

- **D1 (execution model):** parallel per-removal, not a serial worker. Each
  confirmed removal gets its own thread. A mutex guards only the steps that can
  conflict on shared main-repo state; a thread that can't take the lock queues on
  it. Optimized for throughput — the user has an SSD and expects concurrent
  deletes to be fine.
- **D2 (concurrency cap):** none. Confirming N worktrees spawns N parallel
  removals. "Finished" worktrees are few in practice; YAGNI on a width limit.
- **D3 (scope):** every removal path inherits the background behavior — the
  default finished-gate loop, `--clean-worktree`, and `--pr-only`. They share
  one loop, so one change covers all three.

## Problem

`issue end` today (`src/bin/issue/end.rs::run`, lines 165-192) is a fully
synchronous loop. For each finished worktree it:

1. prints `label  path`,
2. blocks on a `[y/N]` `confirm` prompt,
3. blocks on `steps.during_result("Removing…", || cleanup(...))`.

Step 3 is the slow one: `cleanup` runs `git worktree remove`, which `rm -rf`s the
entire worktree — including a multi-GB `node_modules`. So after you answer `y`
you stare at one spinner until that deletion finishes before you can even see,
let alone decide, the next candidate. Cleaning several finished worktrees is a
serial slog whose wall-clock is the *sum* of every deletion.

The decisions are instant; the deletions are slow and independent. Nothing about
deciding candidate N+1 depends on candidate N's files being gone.

## Goal UX

You answer `y`/`N` down the whole list at your own pace. Each `y` kicks off a
background removal with its own spinner; each `N` prints `skipped` and moves on.
Removals animate and settle to persistent `✓`/`✗` lines as they finish —
possibly out of order, interleaved above the prompt you're currently answering.
When you reach the end of the list, the command waits for any still-running
removals (their spinners still live) before printing the final tally.

Mid-flight (stderr bars + stdout prompt, TTY only):

```
✓ Removing DBI-1102… (2.3s)
⠹ Removing DBI-1058…
⠹ Removing DBI-1099…

DBI-1103  ../adaptyv-worktrees/dbi-1103-foo
  Remove DBI-1103? [y/N]
```

Off-TTY or `--yes`: no prompts, every removal dispatches immediately and runs in
parallel; `MultiProgress` is auto-hidden, so pipes / MCP / tests stay silent.

## Enabler — thread-safe `Steps` (`crates/devkit-common/src/progress.rs`)

`Steps` already owns a `MultiProgress` and exposes `during_result`, which draws a
spinner, runs a closure, and settles a persistent `✓`/`✗` line. The only thing
blocking concurrent use is the ordinal counter `n: Cell<usize>` — `Cell` is not
`Sync`, so `&Steps` can't cross a thread boundary.

- Replace `n: Cell<usize>` with `n: AtomicUsize`. `label()` swaps
  `self.n.get()` / `self.n.set(i)` for a single `self.n.fetch_add(1, Relaxed)`.
  Every other field (`MultiProgress`, `Option<usize>`, `bool`) is already
  `Send + Sync`, and every method already takes `&self`. `Steps` becomes
  `Send + Sync`, so scoped threads can share one `&steps` and each call
  `during_result` for its own spinner + settled line. indicatif's
  `MultiProgress` is designed for exactly this multi-threaded drawing.
- Add `Steps::suspend<T>(&self, f: impl FnOnce() -> T) -> T` delegating to
  `self.mp.suspend(f)`. Bars draw on stderr, the prompt writes stdout and reads
  stdin; without suspending the bars, a redraw tears the prompt line. Every
  interactive read and every main-thread `println` goes through `suspend`
  (or `mp.println`).

Numbering under concurrency is a monotonic ordinal in dispatch order — fine for
a persistent log; the exact numbers just reflect spawn order.

## Concurrency structure (`src/bin/issue/end.rs::run`)

The gather → render → filter phase is unchanged. Compute the **main-repo path
once** up front (`git rev-parse --path-format=absolute --git-common-dir` from
`start`, then parent) for the single post-join prune. The removal loop becomes a
`std::thread::scope`:

- Shared by reference under the scope (no `Arc`): `&steps`, a `Mutex<()>`
  branch-lock, an `AtomicUsize` success counter.
- **Main thread** iterates `targets`. For each, inside `steps.suspend(...)`, it
  prints `label  path` and runs `yes || confirm(&label)`. This is the only
  step that blocks on you, and it blocks on nothing but your keystroke.
- On `N`: print `    skipped` (via the group) and continue.
- On `y`: `s.spawn(move || { ... })` a removal thread (move a cloned `label`;
  borrow `row` from `targets`, `&steps`, `&branch_lock`, `&removed`), then
  immediately loop to the next candidate.
- **Scope exit** implicitly joins — outstanding removals finish with their
  spinners live before the tally. In-flight work is never abandoned.

After the join, on the main thread: one `git worktree prune` on the main repo,
then `println!("\nRemoved {} of {}.", removed.load(Relaxed), total)`.

## `cleanup` — parallel vs. serialized split

`cleanup(row, force, &branch_lock)` runs per removal thread. What shares mutable
main-repo state is locked; everything else runs concurrently:

- **Parallel, no lock:**
  - cwd guard (`current_dir` is process-global; reads are thread-safe),
  - `git status --porcelain` dirty check (returns the `Dirty` sentinel unless
    `force`, unchanged),
  - read `HEAD` branch + git-common-dir,
  - **`git worktree remove [--force] <wt>`** — the heavy `rm -rf`; each touches
    its own `.git/worktrees/<id>` admin subdir, so distinct worktrees don't
    share a lock,
  - unlink the `ISSUE_*<id>*.md` files in the main repo's parent.
- **Under `branch_lock` (`Mutex<()>`):** `git show-ref --verify` +
  `git branch -D <branch>`. Ref deletion can contend on `packed-refs.lock`;
  it's near-instant, so serializing costs ~nothing. A thread that can't take the
  lock waits its turn.
- **Once, after the join (main thread, single-threaded):** `git worktree prune`.
  This replaces the current per-removal prune and removes that global-scan race
  from the hot path entirely.

**Residual uncertainty (honest):** concurrent `git worktree remove` of *distinct*
worktrees is believed race-free (separate admin subdirs, no shared repo lock),
but this is ~85% confidence, not certain. If one ever does race, git returns a
non-zero exit on that item; the thread reports it per-item (`✗` + message) and
the user reruns. It degrades to a reported failure, never to corruption.

## Output & error handling

- All main-thread prints (`label  path`, `skipped`) route through
  `steps.suspend` / `mp.println` so they never tear a live bar.
- A removal thread's spinner settles `✓` on success, `✗` on failure (via
  `during_result`). On failure it prints the reason through the group: the
  `Dirty` sentinel → `"{label} is dirty — rerun with --force to discard."`,
  any other error → `"cleanup failed for {label}: {e}"`. Same messages as today,
  emitted from the worker thread.
- The success counter is an `AtomicUsize` incremented on `Ok(())`; the final
  `Removed X of Y.` reads it after the join. `Y` is `targets.len()`, including
  skips, exactly as today.

## Testing

`cleanup`'s git/filesystem IO isn't unit-testable without real worktrees, and no
existing test exercises the removal path. The testable surface is the enabler:

- **Compile-time contract:** a `const _: fn() = || { fn a<T: Send + Sync>() {} a::<Steps>(); }`
  (or equivalent `where Steps: Send + Sync`) so a future field that breaks
  thread-safety fails the build.
- **Concurrent smoke test:** drive several `during_result` calls from a
  `std::thread::scope` sharing one `&Steps`, assert each closure's value comes
  back and the ordinal counter lands at the expected total. Bars are hidden
  off-TTY (as the existing `steps_bars_hidden_off_tty` test relies on), so this
  asserts logic, not rendering.

The full `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings`
gate must stay green; both run before any commit.

## Out of scope

- No concurrency cap (D2).
- No change to the finished-gate logic, `--clean-worktree` selection, dirty/force
  semantics, or the `Removed X of Y.` summary line — only *when* and *how
  concurrently* removals run.
- No new MCP surface. `issue end` is not exposed to agents; this changes nothing
  there.
