# Background Parallel Removals for `issue end` — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `issue end` dispatch each confirmed worktree removal as a background task with its own spinner, so the `[y/N]` loop never blocks on a slow `git worktree remove` and removals run concurrently.

**Architecture:** Make `Steps` (the indicatif progress group) thread-safe by swapping its `Cell<usize>` ordinal for an `AtomicUsize`, then rewrite `issue end`'s removal loop as a `std::thread::scope`: the main thread runs the interactive prompt (bars paused via `MultiProgress::suspend`) and `spawn`s a removal thread per confirmation. Inside `cleanup`, the heavy `git worktree remove` (`rm -rf` of the worktree) and file unlinks run in parallel; only `git branch -D` is serialized behind a `Mutex<()>`, and a single `git worktree prune` runs once after the join.

**Tech Stack:** Rust (edition 2024), `indicatif` (`MultiProgress`/`ProgressBar`), `std::thread::scope`, `std::sync::{Mutex, atomic::AtomicUsize}`, `anyhow`.

**Design:** `docs/superpowers/specs/2026-07-07-issue-end-background-removals-design.md`

---

## File Structure

- **Modify `crates/devkit-common/src/progress.rs`** — swap `Steps.n: Cell<usize>` → `AtomicUsize`; add `Steps::suspend`; add a compile-time `Send + Sync` assertion and a concurrent smoke test. This is the enabler that lets `&Steps` cross a thread boundary.
- **Modify `src/bin/issue/end.rs`** — change `cleanup`'s signature to take `&Mutex<()>` and serialize only `git branch -D`; drop the per-removal `git worktree prune`; add a `main_repo` helper; rewrite the removal loop in `run` as a `thread::scope` that spawns one removal thread per confirmation and prunes once after the join.

---

## Task 1: Make `Steps` thread-safe and add `suspend`

**Files:**
- Modify: `crates/devkit-common/src/progress.rs` (imports line 2; `Steps` struct 61-66; four constructors 69-108; `label` 117-130; add `suspend` method; tests module 223-305)
- Test: same file (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Add the compile-time contract immediately after the `Steps` struct definition (after line 66, non-test so it guards every build):

```rust
// `Steps` must stay `Send + Sync` so scoped worker threads can share one
// `&Steps` — `issue end` dispatches concurrent removals, each drawing its own
// bar through the shared `MultiProgress`.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Steps>();
};
```

Add this test inside `#[cfg(test)] mod tests` (after the existing `persistent_bars_hidden_off_tty` test):

```rust
#[test]
fn during_result_runs_concurrently_across_threads() {
    let steps = Steps::persistent();
    let done = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for i in 0..8 {
            let steps = &steps;
            let done = &done;
            s.spawn(move || {
                let out: anyhow::Result<usize> =
                    steps.during_result(&format!("task {i}"), || Ok(i));
                assert_eq!(out.unwrap(), i);
                done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            });
        }
    });
    assert_eq!(done.load(std::sync::atomic::Ordering::Relaxed), 8);
    // The ordinal advanced exactly once per step regardless of interleaving:
    // 8 steps ran, so the next label is the 9th.
    assert_eq!(steps.label("next"), "9. next");
    steps.clear();
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd crates/devkit-common && cargo test --lib progress 2>&1 | tail -20`
Expected: FAIL to compile — `Cell<usize>` is not `Sync`, so `assert_send_sync::<Steps>()` and the `&steps` capture in the scoped threads both error with `Steps: Sync` / `Cell<usize>: Sync` not satisfied.

- [ ] **Step 3: Write minimal implementation**

Change the import on line 2 from:

```rust
use std::cell::Cell;
```

to:

```rust
use std::sync::atomic::{AtomicUsize, Ordering};
```

Change the `n` field in the `Steps` struct (line 64) from `n: Cell<usize>,` to:

```rust
    n: AtomicUsize,
```

In all four constructors (`new`, `with_total`, `persistent`, `persistent_with_total`), change each `n: Cell::new(0),` to:

```rust
            n: AtomicUsize::new(0),
```

Replace the `label` method body (lines 117-130) with:

```rust
    fn label(&self, msg: &str) -> String {
        match self.total {
            Some(total) => {
                let i = self.n.fetch_add(1, Ordering::Relaxed) + 1;
                format!("[{i}/{total}] {msg}")
            }
            None if self.persist => {
                let i = self.n.fetch_add(1, Ordering::Relaxed) + 1;
                format!("{i}. {msg}")
            }
            None => msg.to_string(),
        }
    }
```

Add this method inside `impl Steps` (e.g. right after `clear`, before the closing brace near line 191):

```rust
    /// Run `f` with every bar in the group hidden, then redraw them. Use around
    /// a stdin prompt or any stdout write that would otherwise be torn by a live
    /// bar redrawing on stderr.
    pub fn suspend<T>(&self, f: impl FnOnce() -> T) -> T {
        self.mp.suspend(f)
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd crates/devkit-common && cargo test --lib progress 2>&1 | tail -20`
Expected: PASS — including `during_result_runs_concurrently_across_threads`, `persistent_mode_numbers_steps`, `numbered_mode_advances_counter`, and `unnumbered_mode_passes_through` (the `None` branch still never increments the counter).

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-common/src/progress.rs
git commit -m "feat(progress): make Steps thread-safe and add suspend" \
  --trailer "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 2: Dispatch removals in parallel in `issue end`

**Files:**
- Modify: `src/bin/issue/end.rs` (imports 1-5; `cleanup` 56-115; `run` removal loop 165-192)
- Test: none new (see Step 4 — the removal path is git/filesystem IO with no existing unit test; verification is the workspace gate plus a manual smoke run)

- [ ] **Step 1: Add imports and the `main_repo` helper**

Change the top-of-file imports (lines 1-5) from:

```rust
use anyhow::{Context, Result};
use devkit_common::cmd::git;
use devkit_common::progress::Steps;
use std::io::{self, Write};
use std::path::Path;
```

to:

```rust
use anyhow::{Context, Result};
use devkit_common::cmd::git;
use devkit_common::progress::Steps;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
```

Add this helper just above `fn cleanup` (before line 56, after the `impl std::error::Error for Dirty {}` block):

```rust
/// The main repo's root path, derived once so a single post-removal
/// `git worktree prune` can run after all removals join. `start` is the base
/// dir (`.` by default); from inside a worktree or the primary clone,
/// `--git-common-dir` resolves to the main repo's `.git`, whose parent is the
/// root.
fn main_repo(start: &str) -> Result<String> {
    let common = git(
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        start,
    )?
    .trim()
    .to_string();
    let main = Path::new(&common)
        .parent()
        .context("git-common-dir has no parent")?;
    main.to_str()
        .map(str::to_string)
        .context("main path not UTF-8")
}
```

- [ ] **Step 2: Rework `cleanup` — serialize only `branch -D`, drop the per-call prune**

Replace the `cleanup` function signature and the tail of its body. Change the signature (line 56) from:

```rust
fn cleanup(worktree_path: &str, issue_id: &str, force: bool) -> Result<()> {
```

to:

```rust
fn cleanup(worktree_path: &str, issue_id: &str, force: bool, branch_lock: &Mutex<()>) -> Result<()> {
```

Also update the doc comment above it (lines 53-55) to state the new locking behavior:

```rust
/// Remove a finished worktree, delete its branch, and remove its ISSUE_*<id>*.md
/// files in the parent of the main repo. Refuses if cwd is inside the worktree,
/// or (without `force`) if the tree is dirty. Serializes `git branch -D` behind
/// `branch_lock` so concurrent removals never contend on `packed-refs.lock`; the
/// worktree removal and file unlinks touch per-worktree state and run in
/// parallel. Pruning the stale worktree entry is left to a single caller-side
/// `git worktree prune` after all removals finish.
```

Replace the block from line 90 (`let _ = git(&["worktree", "prune"], main_s);`) through the `branch -D` block (line 104) with:

```rust

    // Ref deletion can rewrite packed-refs, so concurrent branch deletes contend
    // on packed-refs.lock. Serialize just this step; a thread that can't take the
    // lock queues on it. (A poisoned lock still yields the guard — the critical
    // section is a git call with no invariant to corrupt.)
    {
        let _guard = branch_lock.lock().unwrap_or_else(|e| e.into_inner());
        if git(
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
            main_s,
        )
        .is_ok()
        {
            let _ = git(&["branch", "-D", &branch], main_s);
        }
    }
```

Note: the `git(&rm, main_s)?;` line (the `git worktree remove`) stays exactly as-is directly above this block; only the immediately-following `worktree prune` line and the old unguarded `branch -D` block are replaced.

- [ ] **Step 3: Rewrite the removal loop in `run` as a `thread::scope`**

Replace lines 165-192 (from `let mut removed = 0;` through `Ok(())`) with:

```rust
    let total = targets.len();
    let removed = AtomicUsize::new(0);
    let branch_lock = Mutex::new(());
    // Resolved before the scope so the single post-join prune has a path even if
    // every removal fails; a resolution error just skips the prune.
    let main = main_repo(start).ok();

    std::thread::scope(|s| {
        for row in &targets {
            let label = if row.issue_id != "UNKNOWN" {
                row.issue_id.clone()
            } else {
                row.branch.clone()
            };
            // The interactive decision is the only step that blocks the main
            // thread, and it blocks on nothing but a keystroke. Bars pause during
            // the prompt so a redraw never tears the stdout line.
            let go = steps.suspend(|| {
                println!("\n{label}  {}", row.worktree);
                yes || confirm(&label)
            });
            if !go {
                steps.suspend(|| println!("    skipped"));
                continue;
            }
            let steps = &steps;
            let branch_lock = &branch_lock;
            let removed = &removed;
            s.spawn(move || {
                match steps.during_result(&format!("Removing {label}…"), || {
                    cleanup(&row.worktree, &row.issue_id, force, branch_lock)
                }) {
                    Ok(()) => {
                        removed.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        let msg = if e.downcast_ref::<Dirty>().is_some() {
                            format!("    {label} is dirty — rerun with --force to discard.")
                        } else {
                            format!("    cleanup failed for {label}: {e}")
                        };
                        steps.suspend(|| eprintln!("{msg}"));
                    }
                }
            });
        }
    });

    // Every removal has joined; a single prune reclaims any stale worktree
    // entries without racing a concurrent removal.
    if let Some(main) = main {
        let _ = git(&["worktree", "prune"], &main);
    }
    println!("\nRemoved {} of {}.", removed.load(Ordering::Relaxed), total);
    Ok(())
```

- [ ] **Step 4: Verify build, lint, and the full test gate**

Run:
```bash
cargo build -p devkit --bin issue 2>&1 | tail -5
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo test --workspace 2>&1 | tail -15
```
Expected: `issue` builds; clippy reports zero warnings; the full suite (327+ tests) stays green. No new automated test — the removal path is git/filesystem orchestration with no existing unit coverage; the concurrency enabler is covered by Task 1's smoke test, and correctness of the loop is confirmed by the manual smoke below.

- [ ] **Step 5: Manual smoke test (parallel spinners + non-blocking prompt)**

In a throwaway git repo, create two worktrees, then remove them via the built binary and confirm the prompt returns immediately while spinners animate concurrently:

```bash
tmp=$(mktemp -d); git -C "$tmp" init -q && git -C "$tmp" commit -q --allow-empty -m init
git -C "$tmp" worktree add -q "$tmp/wt-a" -b wt-a
git -C "$tmp" worktree add -q "$tmp/wt-b" -b wt-b
# From the primary repo, force-clean both by branch name (bypasses the finished gate):
./target/debug/issue end --dir "$tmp" --clean-worktree wt-a wt-b
```
Expected: you are prompted for `wt-a` and `wt-b` back-to-back without waiting for `wt-a`'s deletion to finish; each removal shows a `Removing …` spinner that settles to a `✓` line; final output is `Removed 2 of 2.`; `git -C "$tmp" worktree list` shows only the main worktree. Clean up: `rm -rf "$tmp"`.

- [ ] **Step 6: Commit**

```bash
git add src/bin/issue/end.rs
git commit -m "feat(issue): run issue end removals in parallel background tasks" \
  --trailer "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage:**
- Enabler (thread-safe `Steps` + `suspend`) → Task 1. ✓
- `thread::scope` dispatch, `suspend`ed prompt, per-confirmation `spawn`, post-join join → Task 2 Step 3. ✓
- Parallel `git worktree remove` + file unlinks, `branch -D` under `Mutex`, single post-join `prune` → Task 2 Steps 2-3. ✓
- No concurrency cap (D2) → unbounded `s.spawn`. ✓
- All paths inherit (D3) → the rewritten loop is the shared tail after both the `--clean-worktree` and finished-gate branches, so `--pr-only` and `--clean-worktree` get the behavior for free. ✓
- Off-TTY / `--yes` → `MultiProgress` hidden off-TTY (unchanged), `yes` short-circuits `confirm`. ✓
- Error handling (dirty → `--force` hint, else error), `Removed X of Y.` tally via atomic → Task 2 Step 3. ✓
- Testing surface (compile-time `Send+Sync` + concurrent smoke) → Task 1 Steps 1-4. ✓

**Placeholder scan:** none — every code step shows complete code; no TBD/TODO/"similar to".

**Type consistency:** `cleanup(&str, &str, bool, &Mutex<()>)` defined in Step 2 matches its call in Step 3; `main_repo(&str) -> Result<String>` defined in Step 1 matches its use in Step 3; `Steps::suspend` / `during_result` signatures match Task 1; `AtomicUsize` + `Ordering::Relaxed` used consistently.
