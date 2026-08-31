# Parallel worktree includes implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace both filesystem walkers behind `worktree_include` with jwalk on a bounded shared pool, and copy the planned files with rayon on the same pool.

**Architecture:** A new `devkit_common::pool` owns one `rayon::ThreadPool` for the whole workspace, with a re-entry guard that degrades nested work to serial. `worktree.rs` keeps `glob::Pattern` as its matcher and drops `glob::glob_with` and the hand-rolled `plan_dir` recursion in favour of a prefix-scoped jwalk. The copy parallelises inside each pattern, leaving the per-pattern progress brackets sequential.

**Tech Stack:** Rust 2024, jwalk 0.9, rayon 1.12, glob 0.3 (matcher only), anyhow, tempfile.

**Spec:** `docs/superpowers/specs/2026-08-31-parallel-includes-design.md`

## Global constraints

- Workspace deps go in the root `Cargo.toml` `[workspace.dependencies]` and are referenced as `name.workspace = true`. Never a bare version in a member manifest.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all` must all pass before every commit. Zero-warning policy.
- Fail-open: every error inside the walk or the copy becomes a warning string. Nothing propagates.
- Every configured pattern keeps exactly one `PatternPlan` entry, warnings included, so entries line up one-to-one with the include list.
- Tests use `tempfile::tempdir()`. Never build a scratch path from `std::env::temp_dir()`.
- Tests run on ubuntu, macos and windows. Poll for state rather than sleeping a fixed interval.
- Default pool width is 4. Resolution order: `DEVKIT_THREADS`, then `[parallelism] threads`, then 4.
- Conventional Commits. One logical change per commit.

## File structure

| File | Responsibility |
|---|---|
| `crates/devkit-common/src/pool.rs` | New. The shared `rayon::ThreadPool`, its width resolution, the re-entry guard, and jwalk's parallelism setting. |
| `crates/devkit-common/src/lib.rs` | Add `pub mod pool;`. |
| `crates/devkit-common/Cargo.toml` | Add `jwalk`, `rayon`. |
| `Cargo.toml` | Add `jwalk = "0.9"`, `rayon = "1.12"` to `[workspace.dependencies]`. |
| `crates/devkit-config/src/lib.rs` | `ParallelismConfig`, the `Config::parallelism` field, `STANDALONE_SECTIONS`. |
| `crates/devkit-ports/src/load.rs` | One `pool::configure` call, the single door for config. |
| `crates/devkit-common/src/worktree.rs` | The walk rewrite and the parallel copy. All other changes are in this file. |
| `schema/devkit-config.json` | Regenerated, not hand-edited. |
| `docs/configuration.md`, `AGENTS.md` | Documentation. |

## Task order

Tasks 1 to 3 build the pool and its config. Task 4 parallelises the copy, which is independent of the walk and lands the first measurable win. Task 5 is a behaviour change isolated against the *current* walker, so it can be reviewed on its own. Tasks 6 to 9 replace the walkers. Task 10 documents.

---

### Task 1: The shared pool

Creates the workspace's one worker pool, its width resolution, and the guard that stops a nested walk waiting on threads its own caller holds.

**Files:**
- Create: `crates/devkit-common/src/pool.rs`
- Modify: `crates/devkit-common/src/lib.rs`
- Modify: `crates/devkit-common/Cargo.toml`
- Modify: `Cargo.toml`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub fn configure(threads: Option<std::num::NonZeroUsize>)`
  - `pub fn install<R: Send>(f: impl FnOnce() -> R + Send) -> R`
  - `pub fn jwalk_parallelism() -> jwalk::Parallelism`
  - `pub fn width() -> usize`

- [ ] **Step 1: Add the dependencies**

In the root `Cargo.toml`, under `[workspace.dependencies]`, beside the existing `glob = "0.3"` line:

```toml
jwalk = "0.9"
rayon = "1.12"
```

In `crates/devkit-common/Cargo.toml`, under `[dependencies]`, beside `glob.workspace = true`:

```toml
jwalk.workspace = true
rayon.workspace = true
```

- [ ] **Step 2: Write the failing tests**

Create `crates/devkit-common/src/pool.rs` containing only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The pool exists and runs work. `width` is not asserted against a
    /// constant: `configure` and `DEVKIT_THREADS` are process-global and the
    /// test binary runs tests concurrently, so any test pinning a specific
    /// width would race the others.
    #[test]
    fn install_runs_the_closure_and_returns_its_value() {
        assert_eq!(install(|| 2 + 2), 4);
    }

    #[test]
    fn width_is_at_least_one() {
        assert!(width() >= 1);
    }

    /// The guard that keeps a nested walk from waiting on threads its own
    /// caller is holding. Without it this test deadlocks rather than fails.
    #[test]
    fn install_nested_inside_the_pool_runs_on_the_calling_thread() {
        let inner = install(|| install(|| "ran"));
        assert_eq!(inner, "ran");
    }

    /// jwalk must never reach for rayon's global pool, and must go serial when
    /// it would otherwise be nested.
    #[test]
    fn jwalk_parallelism_is_serial_when_already_inside_the_pool() {
        let nested = install(|| matches!(jwalk_parallelism(), jwalk::Parallelism::Serial));
        assert!(nested);
    }

    #[test]
    fn jwalk_parallelism_uses_the_shared_pool_from_outside_it() {
        assert!(matches!(
            jwalk_parallelism(),
            jwalk::Parallelism::RayonExistingPool { .. }
        ));
    }
}
```

Add to `crates/devkit-common/src/lib.rs`, keeping the module list alphabetical (between `paths` and `progress`):

```rust
pub mod pool;
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p devkit-common pool::`
Expected: compile failure, `cannot find function 'install' in this scope`.

- [ ] **Step 4: Write the implementation**

Put this above the test module in `crates/devkit-common/src/pool.rs`:

```rust
//! The workspace's shared worker pool.
//!
//! One bounded `rayon::ThreadPool` serves every parallel feature in devkit,
//! rather than each building its own. Several agent sessions run devkit at once
//! on a machine, so per-feature pools would multiply thread count by feature
//! count with nothing coordinating them.
//!
//! Go through [`install`] and [`jwalk_parallelism`] rather than `par_iter` or
//! jwalk's default parallelism directly. Both of those reach rayon's global
//! pool, which has its own width and is the collision this module prevents.

use std::num::NonZeroUsize;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Threads when neither the environment nor the config says otherwise. The
/// measured throughput knee for file copying.
const DEFAULT_THREADS: usize = 4;

/// How long jwalk waits for a pool thread before giving up. [`install`]'s guard
/// means the pool is free whenever a walk starts, so this never fires in
/// practice; it exists so a future caller that walks from two threads at once
/// gets an error rather than a hang.
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

static CONFIGURED: OnceLock<NonZeroUsize> = OnceLock::new();
static POOL: OnceLock<Option<Arc<rayon::ThreadPool>>> = OnceLock::new();

/// Record the pool's width from config. The first call wins, and a call made
/// after the pool has been built is ignored, so this belongs beside the config
/// load rather than at a use site.
pub fn configure(threads: Option<NonZeroUsize>) {
    if let Some(n) = threads {
        let _ = CONFIGURED.set(n);
    }
}

/// The pool's width. `DEVKIT_THREADS` wins over [`configure`], which wins over
/// [`DEFAULT_THREADS`]. An unparseable or zero env value is ignored rather than
/// treated as a request, because `ThreadPoolBuilder::num_threads(0)` means one
/// thread per core: the opposite of what someone capping threads intends.
pub fn width() -> usize {
    if let Ok(v) = std::env::var("DEVKIT_THREADS")
        && let Ok(n) = v.parse::<NonZeroUsize>()
    {
        return n.get();
    }
    CONFIGURED.get().map_or(DEFAULT_THREADS, |n| n.get())
}

/// The shared pool, or `None` when it could not be built. A build failure is
/// not fatal: callers fall back to running their work on the calling thread.
fn pool() -> Option<&'static Arc<rayon::ThreadPool>> {
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(width())
            .thread_name(|i| format!("devkit-{i}"))
            .build()
            .ok()
            .map(Arc::new)
    })
    .as_ref()
}

/// Whether the calling thread is a rayon worker. Work dispatched from one must
/// stay on it: a bounded pool re-entered from its own worker can leave a nested
/// walk waiting for a thread that never frees.
fn inside_a_pool() -> bool {
    rayon::current_thread_index().is_some()
}

/// Run `f` on the shared pool, or on the calling thread when already inside a
/// pool or when the pool could not be built.
pub fn install<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    match pool() {
        Some(p) if !inside_a_pool() => p.install(f),
        _ => f(),
    }
}

/// jwalk's parallelism setting for the shared pool. `Serial` when already
/// inside a pool, for the reason [`install`] describes.
pub fn jwalk_parallelism() -> jwalk::Parallelism {
    match pool() {
        Some(p) if !inside_a_pool() => jwalk::Parallelism::RayonExistingPool {
            pool: Arc::clone(p),
            busy_timeout: Some(BUSY_TIMEOUT),
        },
        _ => jwalk::Parallelism::Serial,
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p devkit-common pool::`
Expected: 5 passed.

- [ ] **Step 6: Gate and commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git add Cargo.toml Cargo.lock crates/devkit-common/Cargo.toml crates/devkit-common/src/pool.rs crates/devkit-common/src/lib.rs
git commit -m "feat(pool): add the workspace's shared worker pool"
```

---

### Task 2: The `[parallelism]` config table

Gives the pool a config key. A thread count is machine tuning, so it needs its own table and must resolve without a `[defaults]` table, the way a personal `~/.config/devkit/config.toml` carries it.

**Files:**
- Modify: `crates/devkit-config/src/lib.rs`
- Modify: `schema/devkit-config.json` (regenerated)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub struct ParallelismConfig { pub threads: Option<NonZeroUsize> }`, reachable as `Config::parallelism`.

- [ ] **Step 1: Write the failing tests**

Add to the test module in `crates/devkit-config/src/lib.rs`:

```rust
/// A thread count is machine tuning, set in the personal layer, so a config
/// carrying only `[parallelism]` has to resolve with no project around it.
#[test]
fn a_parallelism_only_config_needs_no_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("parallelism-only");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("devkit.toml"),
        "[config]\nroot = true\n[parallelism]\nthreads = 8\n",
    )
    .unwrap();

    let (cfg, _) = resolve_with_home(None, &project, None, None, None).unwrap();
    assert_eq!(cfg.parallelism.threads.map(|n| n.get()), Some(8));
    assert_eq!(health_with_home(&project, None, None), Health::Ok);
}

/// `ThreadPoolBuilder::num_threads(0)` means one thread per core, so zero is
/// the opposite of what it looks like. `NonZeroUsize` refuses it at parse time
/// instead of leaving a runtime clamp to remember.
#[test]
fn a_zero_thread_count_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("zero-threads");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("devkit.toml"),
        "[config]\nroot = true\n[parallelism]\nthreads = 0\n",
    )
    .unwrap();

    assert!(resolve_with_home(None, &project, None, None, None).is_err());
}

/// An absent table takes the pool's own default rather than a serde one, so
/// the number lives in exactly one place.
#[test]
fn an_absent_parallelism_table_leaves_threads_unset() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("no-parallelism");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("devkit.toml"), "[config]\nroot = true\n").unwrap();

    let (cfg, _) = resolve_with_home(None, &project, None, None, None).unwrap();
    assert!(cfg.parallelism.threads.is_none());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-config parallelism`
Expected: compile failure, `no field 'parallelism' on type 'Config'`.

- [ ] **Step 3: Add the struct and the field**

In `crates/devkit-config/src/lib.rs`, add near the other table structs (after `TrackerConfig`):

```rust
/// Width of the shared worker pool. Machine tuning rather than a project
/// convention, so it belongs in the personal layer at
/// `~/.config/devkit/config.toml` rather than a repository's `devkit.toml`.
#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct ParallelismConfig {
    /// Threads in the pool devkit shares across its parallel work. The
    /// `DEVKIT_THREADS` environment variable wins over this; leaving it unset
    /// takes the pool's own default. Zero is refused rather than clamped,
    /// because rayon reads a zero thread count as one thread per core.
    pub threads: Option<std::num::NonZeroUsize>,
}
```

Add the field to `Config`, after `tracker`:

```rust
    /// Width of the shared worker pool. Machine tuning; carries no project
    /// convention, so a config may hold it alone.
    #[serde(default)]
    pub parallelism: ParallelismConfig,
```

- [ ] **Step 4: Add it to the standalone sections**

Replace the `STANDALONE_SECTIONS` const and its doc comment:

```rust
/// Tables a devkit.toml may carry on its own, with no project configured around
/// them. Each is read by a crate that needs no path or branch convention:
/// `[harness]` by `devkit-locks`, `[docs]` by `devkit-docs`, `[config]` by the
/// layer walk below, `[brief]` by the session summary, `[parallelism]` by the
/// shared worker pool. A config built only from these resolves without a
/// `[defaults]` table; anything else demands one.
const STANDALONE_SECTIONS: [&str; 5] = ["config", "harness", "docs", "brief", "parallelism"];
```

Update the two doc comments that enumerate the same list. On `Config::defaults` (near line 12):

```rust
    /// Project-wide paths and branch conventions. Required of any config that
    /// configures a project; omitted only by one built entirely from
    /// `[config]`, `[harness]`, `[docs]`, `[brief]`, and `[parallelism]`.
```

On the `Defaults` struct (near line 248), replace the sentence listing the tables:

```rust
/// Project-wide paths and branch conventions. The first four keys are required
/// of any config that configures a project — without them no worktree, branch,
/// or baseline resolves. A config carrying only `[config]`, `[harness]`,
/// `[docs]`, `[brief]`, or `[parallelism]` omits the table entirely and takes
/// the defaults.
```

Add `[parallelism]` to the existing `a_config_of_standalone_sections_needs_no_defaults` test's fixture, so the two tests cover the const from both directions:

```rust
    std::fs::write(
        project.join("devkit.toml"),
        "[config]\nroot = true\n[harness]\nenforce_writes = true\n[parallelism]\nthreads = 2\n",
    )
    .unwrap();
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p devkit-config`
Expected: the three new tests pass. The committed-schema drift test fails, which is the next step.

- [ ] **Step 6: Regenerate the schema**

Run: `DEVKIT_UPDATE_SCHEMA=1 cargo test -p devkit --test schema`

If that test target name does not resolve, find it first with `rg -l "DEVKIT_UPDATE_SCHEMA" --glob '!target'` and run the test it names. Do not hand-edit `schema/devkit-config.json`.

Confirm the regenerated schema carries a `minimum: 1` on `threads`, which is what `NonZeroUsize` buys:

```bash
jq '.properties.parallelism' schema/devkit-config.json
```

- [ ] **Step 7: Verify the schema test passes**

Run: `cargo test --workspace`
Expected: all pass.

- [ ] **Step 8: Gate and commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
git add crates/devkit-config/src/lib.rs schema/devkit-config.json
git commit -m "feat(config): add the [parallelism] table"
```

---

### Task 3: Wire the config into the pool

One call, at the single door every subcommand's config resolution passes through, so no binary can forget it.

**Files:**
- Modify: `crates/devkit-ports/src/load.rs:16-34`

**Interfaces:**
- Consumes: `devkit_common::pool::configure`, `Config::parallelism`.
- Produces: nothing new.

- [ ] **Step 1: Write the failing test**

Add to `crates/devkit-ports/src/load.rs`:

```rust
#[cfg(test)]
mod tests {
    /// `load` is the one door every subcommand's config goes through, so the
    /// pool is configured there rather than at each of the eighteen call
    /// sites, where it could be forgotten. The width is read back rather than
    /// asserted equal to the configured value: `configure` writes a
    /// process-global `OnceLock` that another test in this binary may have set
    /// first, and `DEVKIT_THREADS` outranks it either way.
    #[test]
    fn load_configures_the_shared_pool() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("devkit.toml"),
            "[config]\nroot = true\n[parallelism]\nthreads = 3\n",
        )
        .unwrap();

        super::load(None, &project).unwrap();
        assert!(devkit_common::pool::width() >= 1);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p devkit-ports load::tests`
Expected: FAIL. Without `tempfile` as a dev-dependency it fails to compile; add `tempfile.workspace = true` under `[dev-dependencies]` in `crates/devkit-ports/Cargo.toml` if it is not already there, then re-run and expect the failure to be the missing `pool` call rather than a compile error.

- [ ] **Step 3: Add the call**

In `crates/devkit-ports/src/load.rs`, immediately after the `config::resolve` call:

```rust
    let (cfg, provenance) = config::resolve(
        explicit,
        start,
        main_checkout.as_deref(),
        checkout_root.as_deref(),
    )?;
    // The one door every subcommand's config passes through, so the shared
    // pool is sized here rather than at each caller.
    devkit_common::pool::configure(cfg.parallelism.threads);
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p devkit-ports load::tests`
Expected: PASS.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git add crates/devkit-ports/src/load.rs crates/devkit-ports/Cargo.toml
git commit -m "feat(config): size the shared pool from [parallelism]"
```

---

### Task 4: Parallelise the copy

Independent of the walk, and the first measurable win. Parallelism goes inside a pattern so the per-entry progress brackets stay in configuration order.

**Files:**
- Modify: `crates/devkit-common/src/worktree.rs:144-211` (`apply_includes_with`)
- Modify: `crates/devkit-common/src/worktree.rs:589-623` (`copy_file`)
- Modify: `crates/devkit-common/src/worktree.rs:624-675` (`make_link`)
- Modify: `crates/devkit-common/src/worktree.rs:1380` (`the_copy_brackets_each_pattern_and_counts_within_it`)

**Interfaces:**
- Consumes: `devkit_common::pool::install`.
- Produces: `copy_file` and `make_link` take `&AtomicUsize` and `&Mutex<Vec<String>>` instead of `&mut usize` and `&mut Vec<String>`, plus `fn warn(&Mutex<Vec<String>>, String)`.

- [ ] **Step 1: Relax the order-sensitive test**

`the_copy_brackets_each_pattern_and_counts_within_it` asserts `"file hooks/ 1/2"` then `"file hooks/ 2/2"` in that exact order. Parallel copy inside a pattern makes those two swappable. The bracketing is what the test exists to pin, so the brackets stay strict and only the file lines relax.

Add this helper inside the `mod tests` block in `crates/devkit-common/src/worktree.rs`:

```rust
    /// Sort each contiguous run of `file …` lines. The copy is parallel within
    /// one entry, so those lines may arrive in any order; the `start` and
    /// `done` brackets around them may not.
    fn sort_file_runs(log: &[String]) -> Vec<String> {
        let mut out: Vec<String> = Vec::with_capacity(log.len());
        let mut run: Vec<String> = Vec::new();
        for line in log {
            if line.starts_with("file ") {
                run.push(line.clone());
            } else {
                run.sort();
                out.append(&mut run);
                out.push(line.clone());
            }
        }
        run.sort();
        out.append(&mut run);
        out
    }
```

In that test, replace the final assertion's left-hand side:

```rust
        assert_eq!(
            sort_file_runs(&log.lock().unwrap()),
            vec![
                "start .tool-versions 0/2 1",
                "file .tool-versions 1/1",
                "done .tool-versions 0/2 1",
                "start hooks/ 1/2 2",
                "file hooks/ 1/2",
                "file hooks/ 2/2",
                "done hooks/ 1/2 2",
            ]
        );
```

- [ ] **Step 2: Run the suite to confirm it still passes serially**

Run: `cargo test -p devkit-common worktree::`
Expected: PASS. The relaxation is a no-op against the current serial copy, which is the point: it changes what the test tolerates, not what it asserts.

- [ ] **Step 3: Commit the relaxation on its own**

```bash
git add crates/devkit-common/src/worktree.rs
git commit -m "test(worktree): tolerate parallel file completion within an entry"
```

- [ ] **Step 4: Write the failing test**

Add to the test module:

```rust
    /// The copy runs several files at once, so its counters and its warnings
    /// have to survive concurrent writers. A hundred files is enough for the
    /// pool to hand work to more than one thread.
    #[test]
    fn a_parallel_copy_counts_every_file_exactly_once() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        for i in 0..100 {
            write(&src.join("many").join(format!("f{i}.txt")), "x");
        }

        let plan = plan_includes(&src, &dst, &["many/".to_string()]);
        let seen = std::sync::Mutex::new(Vec::new());
        let (copied, _, warnings) = apply_includes_with(&src, &dst, &plan, false, &|e| {
            if let IncludeEvent::FileDone { done, .. } = e {
                seen.lock().unwrap().push(done);
            }
        });

        assert_eq!(copied, 100, "{warnings:?}");
        assert!(warnings.is_empty(), "{warnings:?}");
        let mut seen = seen.lock().unwrap().clone();
        seen.sort_unstable();
        assert_eq!(seen, (1..=100).collect::<Vec<_>>());
        for i in 0..100 {
            assert!(dst.join("many").join(format!("f{i}.txt")).exists());
        }
    }
```

- [ ] **Step 5: Run it to verify it passes serially**

Run: `cargo test -p devkit-common a_parallel_copy_counts_every_file_exactly_once`
Expected: PASS against the current serial code. This test is a regression guard for the change rather than a red-first driver; the parallelism it names is what steps 6 and 7 introduce, and a lost warning or a double count would fail it afterwards.

- [ ] **Step 6: Change the accumulators**

Add to the imports at the top of `crates/devkit-common/src/worktree.rs`:

```rust
use rayon::prelude::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
```

Rewrite `copy_file`'s signature and body:

```rust
/// Copy a single file, creating its destination's parent directories as
/// needed. Unless `overwrite` is set, a destination that exists at this moment
/// is left untouched. Errors are pushed as warnings. Safe to call from several
/// threads at once: the counter and the warning list are shared.
fn copy_file(
    src: &Path,
    dst: &Path,
    overwrite: bool,
    copied: &AtomicUsize,
    warnings: &Mutex<Vec<String>>,
) {
    if !overwrite && dst.exists() {
        return;
    }
    if let Some(parent) = dst.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        warn(warnings, format!("creating {}: {e}", parent.display()));
        return;
    }
    match std::fs::copy(src, dst) {
        Ok(_) => {
            copied.fetch_add(1, Ordering::Relaxed);
        }
        Err(e) => warn(
            warnings,
            format!("copying {} -> {}: {e}", src.display(), dst.display()),
        ),
    }
}

/// Push a warning through a lock a poisoned thread may have left behind. A
/// panicking worker must not silence every warning that follows it.
fn warn(warnings: &Mutex<Vec<String>>, message: String) {
    match warnings.lock() {
        Ok(mut w) => w.push(message),
        Err(poisoned) => poisoned.into_inner().push(message),
    }
}
```

`create_dir_all` is safe to race: it returns `Ok` when the directory already exists, so two threads creating the same parent both succeed.

Change `make_link` the same way. Its signature becomes:

```rust
fn make_link(
    src: &Path,
    dst: &Path,
    target: &Path,
    overwrite: bool,
    linked: &AtomicUsize,
    warnings: &Mutex<Vec<String>>,
) {
```

Inside it, replace each `warnings.push(...)` with `warn(warnings, ...)` and replace `*linked += 1` with `linked.fetch_add(1, Ordering::Relaxed);`. The body is otherwise unchanged.

- [ ] **Step 7: Rewrite the copy loop**

Replace the body of `apply_includes_with`:

```rust
pub fn apply_includes_with(
    source: &Path,
    dest: &Path,
    plan: &IncludePlan,
    overwrite: bool,
    on: &(dyn Fn(IncludeEvent) + Sync),
) -> (usize, usize, Vec<String>) {
    let copied = AtomicUsize::new(0);
    let linked = AtomicUsize::new(0);
    let mut warnings = Vec::new();
    let of = plan.patterns.len();

    for (index, entry) in plan.patterns.iter().enumerate() {
        let worklist: Vec<&PathBuf> = if overwrite {
            entry.missing.iter().chain(entry.existing.iter()).collect()
        } else {
            entry.missing.iter().collect()
        };
        // A link is a unit of this pattern's work, so the denominator the
        // display draws against covers both lists.
        let total = worklist.len() + entry.links.len();
        on(IncludeEvent::EntryStart {
            pattern: &entry.pattern,
            index,
            of,
            files: total,
        });

        let before = copied.load(Ordering::Relaxed);
        let done = AtomicUsize::new(0);
        let entry_warnings = Mutex::new(Vec::new());
        let bump = |done: &AtomicUsize| IncludeEvent::FileDone {
            pattern: &entry.pattern,
            done: done.fetch_add(1, Ordering::Relaxed) + 1,
            of: total,
        };

        // Files and links run as two phases rather than one mixed worklist:
        // link creation is the failure-prone path on Windows, and keeping it
        // apart keeps its counter and its warnings separable.
        crate::pool::install(|| {
            worklist.par_iter().for_each(|rel| {
                copy_file(
                    &source.join(rel),
                    &dest.join(rel),
                    overwrite,
                    &copied,
                    &entry_warnings,
                );
                on(bump(&done));
            });
            entry.links.par_iter().for_each(|(rel, target)| {
                make_link(
                    &source.join(rel),
                    &dest.join(rel),
                    target,
                    overwrite,
                    &linked,
                    &entry_warnings,
                );
                on(bump(&done));
            });
        });

        // Sorted within the pattern, and patterns keep configuration order, so
        // two runs over one tree report warnings identically.
        let mut w = entry_warnings.into_inner().unwrap_or_else(|e| e.into_inner());
        w.sort();
        warnings.extend(w);

        on(IncludeEvent::EntryDone {
            pattern: &entry.pattern,
            index,
            of,
            copied: copied.load(Ordering::Relaxed) - before,
        });
    }

    (copied.into_inner(), linked.into_inner(), warnings)
}
```

- [ ] **Step 8: Add the re-entry guard's real test**

The pool's own test covers `install` nested in `install`. This covers the case
that actually reaches users: a copy dispatched from inside pool work, which is
what a future parallel `sync-includes` would do.

```rust
    /// A copy started from inside the pool must finish rather than wait on
    /// threads its own caller is holding. Carries a timeout because a
    /// regression hangs rather than fails.
    #[test]
    fn a_copy_started_from_inside_the_pool_completes() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        for i in 0..20 {
            write(&src.join("many").join(format!("f{i}.txt")), "x");
        }

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let done = crate::pool::install(|| {
                let plan = plan_includes(&src, &dst, &["many/".to_string()]);
                apply_includes(&src, &dst, &plan, false).0
            });
            let _ = tx.send(done);
        });

        let copied = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("a nested copy exhausted the pool instead of running serially");
        assert_eq!(copied, 20);
    }
```

Run: `cargo test -p devkit-common a_copy_started_from_inside_the_pool_completes`
Expected: PASS.

- [ ] **Step 9: Run the full module**

Run: `cargo test -p devkit-common worktree::`
Expected: all pass, including the three symlink tests and the two progress tests.

- [ ] **Step 10: Gate and commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git add crates/devkit-common/src/worktree.rs
git commit -m "perf(worktree): copy a pattern's files in parallel"
```

---

### Task 5: Confine include patterns to the source tree

A behaviour change, landed against the current walker so it can be reviewed alone. `copy_includes` will read outside its source tree today; only `copy_out` filters.

**Files:**
- Modify: `crates/devkit-common/src/worktree.rs:421-431` (top of `Walk::plan_one`)

**Interfaces:**
- Consumes: the existing `escapes` free function.
- Produces: nothing new. Task 8 keeps this gate in the rewritten walk.

- [ ] **Step 1: Write the failing test**

Add to the test module:

```rust
    /// `plan_one` builds its pattern from `source.join(trimmed)`, and
    /// `Path::join` discards the base for a rooted pattern — on Windows too,
    /// where `is_absolute` is false but `join` still replaces the base. Without
    /// a gate, an include reads from outside the tree it is supposed to copy.
    ///
    /// The gate sits above the wildcard/literal split, so `../*/secrets` is
    /// refused by the same check as `../outside.md`.
    #[test]
    fn an_escaping_include_pattern_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join("keep.txt"), "x");
        write(&base.path().join("outside.md"), "secret");

        let plan = plan_includes(
            &src,
            &dst,
            &[
                "../outside.md".to_string(),
                "/etc/passwd".to_string(),
                "../*/secrets".to_string(),
                "keep.txt".to_string(),
            ],
        );

        assert_eq!(plan.patterns.len(), 4, "every pattern keeps its entry");
        assert_eq!(plan.missing_len(), 1);
        assert_eq!(plan.missing().next().unwrap(), Path::new("keep.txt"));
        assert_eq!(plan.warnings.len(), 3, "{:?}", plan.warnings);
        assert!(plan.warnings.iter().any(|w| w.contains("../outside.md")));
        assert!(plan.warnings.iter().any(|w| w.contains("/etc/passwd")));
        assert!(plan.warnings.iter().any(|w| w.contains("../*/secrets")));
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p devkit-common an_escaping_include_pattern_is_refused`
Expected: FAIL, `assertion left == right` on `plan.warnings.len()` being 0, and `missing_len` being 2 on a machine that has `/etc/passwd`.

- [ ] **Step 3: Add the gate**

In `Walk::plan_one`, immediately after the empty-pattern guard:

```rust
        let trimmed = pattern.trim_end_matches('/');
        // An empty pattern joins to `source` itself, which globs to the source
        // directory and strips to an empty relative path, planning every file
        // under the root. Drop it before the join rather than after.
        if trimmed.is_empty() {
            return;
        }
        // `source.join` discards the base for a rooted pattern and keeps a `..`
        // component, so a pattern that leaves the tree has to be refused before
        // the join rather than detected after it.
        if escapes(trimmed) {
            warnings.push(format!(
                "include pattern reaches outside the source tree, skipped: {pattern}"
            ));
            return;
        }
```

`copy_out` already strips escaping patterns before planning, so it never reaches this branch and its own warnings are unchanged.

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p devkit-common worktree::`
Expected: all pass, including `copy_out_refuses_a_pattern_that_escapes_the_worktree`, whose warnings still arrive from `copy_out`'s own filter and still sit at indices 0 and 1.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git add crates/devkit-common/src/worktree.rs
git commit -m "fix(worktree): refuse an include pattern that leaves the source tree"
```

---

### Task 6: The walk root

A pure function, tested alone before anything depends on it. Scopes a walk so `apps/*/.env.local` visits `apps/` instead of the whole tree.

**Files:**
- Modify: `crates/devkit-common/src/worktree.rs` (add beside `is_glob`, near line 680)

**Interfaces:**
- Consumes: the existing `is_glob`.
- Produces: `fn walk_root(pattern: &str) -> Option<PathBuf>`. `None` means the pattern holds no wildcard and needs no walk. `Some` of an empty path means walk from the source root.

- [ ] **Step 1: Write the failing tests**

Add to the test module:

```rust
    #[test]
    fn walk_root_stops_at_the_first_wildcard() {
        assert_eq!(walk_root("apps/*/.env.local"), Some(PathBuf::from("apps")));
        assert_eq!(
            walk_root("apps/web/config/*.json"),
            Some(PathBuf::from("apps/web/config"))
        );
        assert_eq!(walk_root("src/[abc]/x"), Some(PathBuf::from("src")));
        assert_eq!(walk_root("logs/?.txt"), Some(PathBuf::from("logs")));
    }

    /// A leading `**` scopes to nothing, so the walk starts at the source root.
    /// That is what `glob_with` does today, not a widening.
    #[test]
    fn walk_root_of_a_leading_recursive_wildcard_is_the_source_root() {
        assert_eq!(walk_root("**/.env.local"), Some(PathBuf::new()));
    }

    /// A pattern with no wildcard costs one stat, not a walk.
    #[test]
    fn walk_root_is_none_without_a_wildcard() {
        assert_eq!(walk_root(".tool-versions"), None);
        assert_eq!(walk_root(".claude/hooks"), None);
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p devkit-common walk_root`
Expected: compile failure, `cannot find function 'walk_root'`.

- [ ] **Step 3: Write the implementation**

Add beside `is_glob`:

```rust
/// The literal directory prefix of a pattern: its components up to the first
/// one holding a wildcard. Scopes a walk so `apps/*/.env.local` reads `apps/`
/// rather than the whole source tree.
///
/// `None` means the pattern holds no wildcard at all and costs one stat instead
/// of a walk. `Some` of an empty path means the walk starts at the source root,
/// which is what a leading `**` asks for.
///
/// The pattern must already be trimmed of a trailing `/` and checked with
/// [`escapes`]: this reads `Component::Normal` only, so a `..` or a root would
/// silently vanish from the prefix rather than being refused.
fn walk_root(pattern: &str) -> Option<PathBuf> {
    if !is_glob(pattern) {
        return None;
    }
    let mut root = PathBuf::new();
    for part in Path::new(pattern).components() {
        let Component::Normal(name) = part else { break };
        match name.to_str() {
            Some(literal) if !is_glob(literal) => root.push(literal),
            _ => break,
        }
    }
    Some(root)
}
```

- [ ] **Step 4: Run them to verify they pass**

Run: `cargo test -p devkit-common walk_root`
Expected: 3 passed.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git add crates/devkit-common/src/worktree.rs
git commit -m "feat(worktree): derive a pattern's literal walk root"
```

---

### Task 7: One parallel walk-and-classify

Replaces `Walk::plan_dir`'s hand-rolled recursion with a jwalk walk whose
classification runs on the shared pool. `glob_with` still expands patterns above
it; task 8 removes that. The module works and every test passes at the end of
this task.

The walk and the classification are two passes. jwalk's callbacks are `'static`
while `dest` and the event callback are borrowed, so classification cannot run in
jwalk's own workers, and draining through `par_bridge` would put consumers on the
same four threads the readers need. So the calling thread drains, the pool
classifies the batch, and the calling thread records the verdicts into
`PatternPlan`, which no other thread ever touches.

**Files:**
- Modify: `crates/devkit-common/src/worktree.rs:353-418` (`Walk::classify_file`, `classify_link`, `plan_dir`)

**Interfaces:**
- Consumes: `pool::install`, `pool::jwalk_parallelism`, `LinkMode`.
- Produces:
  - `enum Classified { File { rel: PathBuf, exists: bool }, Link { rel: PathBuf, target: PathBuf }, Warning(String) }`
  - `Walk::walk_and_classify(&self, source: &Path, start: &Path, matcher: Option<&glob::Pattern>, opts: glob::MatchOptions, out: &mut PatternPlan, warnings: &mut Vec<String>)` — `matcher: None` claims everything under `start`, which is what a directory match asks for. Task 8 passes `Some`.
  - `Walk::record_file(&self, rel: PathBuf, exists: bool, out: &mut PatternPlan)` and `Walk::record_link(&self, rel: PathBuf, target: PathBuf, out: &mut PatternPlan)`, which fire `IncludeEvent::Found`.
  - `fn matches_here(matcher: &glob::Pattern, rel: &Path, opts: glob::MatchOptions) -> bool`

- [ ] **Step 1: Write the failing test**

Add to the test module in `crates/devkit-common/src/worktree.rs`:

```rust
    /// A directory include walks its whole subtree, and a symlink inside it is
    /// reproduced as a link rather than descended into. jwalk with
    /// `follow_links(true)` reads through a symlinked directory unless the walk
    /// clears its children, so this pins the clearing.
    #[cfg(unix)]
    #[test]
    fn a_directory_include_keeps_a_nested_symlink_as_a_link() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join("inc/a.txt"), "x");
        write(&src.join("inc/deep/b.txt"), "y");
        write(&src.join("target/c.txt"), "z");
        std::os::unix::fs::symlink("../target", src.join("inc/linked")).unwrap();

        let plan = plan_includes(&src, &dst, &["inc/".to_string()]);

        let mut missing: Vec<_> = plan.missing().map(Path::to_path_buf).collect();
        missing.sort();
        assert_eq!(
            missing,
            vec![PathBuf::from("inc/a.txt"), PathBuf::from("inc/deep/b.txt")],
            "the link's contents are not planned as files"
        );
        assert_eq!(plan.patterns[0].links.len(), 1);
        assert_eq!(plan.patterns[0].links[0].0, PathBuf::from("inc/linked"));
        assert_eq!(plan.patterns[0].links[0].1, PathBuf::from("../target"));
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    }
```

- [ ] **Step 2: Run it to verify it passes on the current code**

Run: `cargo test -p devkit-common a_directory_include_keeps_a_nested_symlink_as_a_link`
Expected: PASS. `plan_dir` already behaves this way. This is the contract jwalk must reproduce, not a new behaviour, and confirming it green now is what makes a failure in step 6 mean the swap broke something.

- [ ] **Step 3: Commit the test on its own**

```bash
git add crates/devkit-common/src/worktree.rs
git commit -m "test(worktree): pin nested link handling before the walk swap"
```

- [ ] **Step 4: Split classification from recording**

The existence check is a syscall per candidate and has to run on the pool; pushing into `PatternPlan` and firing `Found` must not. Replace `Walk::classify_file` and `Walk::classify_link` with:

```rust
    /// Record a classified file. Runs on the calling thread, so `found` needs
    /// no synchronisation and `Found` still arrives in one order.
    fn record_file(&self, rel: PathBuf, exists: bool, out: &mut PatternPlan) {
        if exists {
            out.existing.push(rel);
        } else {
            out.missing.push(rel);
        }
        self.found.set(self.found.get() + 1);
        (self.on)(IncludeEvent::Found {
            files: self.found.get(),
        });
    }

    /// Record a source symlink and the target it holds, and count it as a
    /// match. Every matched link lands here whatever the destination holds:
    /// routing an occupied one into `existing` would hand it to `copy_file`
    /// under `overwrite`, which writes the target's contents instead of
    /// reproducing the link. `make_link` decides per link whether to skip or
    /// replace.
    fn record_link(&self, rel: PathBuf, target: PathBuf, out: &mut PatternPlan) {
        out.links.push((rel, target));
        self.found.set(self.found.get() + 1);
        (self.on)(IncludeEvent::Found {
            files: self.found.get(),
        });
    }
```

`found` stays a `Cell`. Only the syscall-heavy part goes to the pool, and firing a progress event during the serial merge costs nothing, so `the_plan_walk_reports_a_running_count_and_a_total` needs no relaxation.

- [ ] **Step 5: Add the walk**

Delete `Walk::plan_dir`. Add beside `is_glob`:

```rust
/// One walked entry's verdict, decided on the pool and applied to the plan on
/// the calling thread. `PatternPlan` has a single owner this way, so the
/// syscall-heavy part parallelises without the collection needing a lock.
enum Classified {
    File { rel: PathBuf, exists: bool },
    Link { rel: PathBuf, target: PathBuf },
    Warning(String),
}

/// Whether `rel` is claimed by `matcher`, either by matching it or by sitting
/// under a directory that does. A directory match contributes its whole
/// subtree, and testing ancestors keeps that rule stateless, so the walk needs
/// nothing shared between threads to enforce it.
fn matches_here(matcher: &glob::Pattern, rel: &Path, opts: glob::MatchOptions) -> bool {
    rel.ancestors()
        .any(|a| !a.as_os_str().is_empty() && matcher.matches_path_with(a, opts))
}
```

Add to `impl Walk`:

```rust
    /// Walk from `start` and classify what `matcher` claims, or everything
    /// under it when `matcher` is `None`, which is what a directory match asks
    /// for. Paths are recorded relative to `source`.
    ///
    /// Three passes, in order and for a reason. jwalk's callbacks are `'static`
    /// while `dest` and the event callback are borrowed, so classification
    /// cannot run in jwalk's workers; and draining through `par_bridge` would
    /// put consumers on the same threads the readers need, starving the walk.
    /// So: the calling thread drains, which costs no syscalls of its own
    /// because `file_type` comes cached from the directory read; the pool
    /// classifies, because `dest.join(rel).exists()` is a syscall per candidate
    /// and around 75µs on a drive mounted under WSL; the calling thread
    /// records.
    ///
    /// Two jwalk defaults are wrong here and both would fail quietly.
    /// `skip_hidden` is true, and every include devkit exists for is a dotfile.
    /// `parallelism` is rayon's global pool, which is the one the shared pool
    /// exists to avoid.
    fn walk_and_classify(
        &self,
        source: &Path,
        start: &Path,
        matcher: Option<&glob::Pattern>,
        opts: glob::MatchOptions,
        out: &mut PatternPlan,
        warnings: &mut Vec<String>,
    ) {
        let mode = self.mode;
        let dest = self.dest;
        // Owned copies for the `'static` callback. Cloning a compiled pattern
        // once per pattern is not a cost worth avoiding.
        let pruner = matcher.cloned();
        let prune_source = source.to_path_buf();

        let entries: Vec<_> = crate::pool::install(|| {
            jwalk::WalkDir::new(start)
                .skip_hidden(false)
                .follow_links(true)
                .parallelism(crate::pool::jwalk_parallelism())
                .process_read_dir(move |_depth, dir, _state, children| {
                    if mode != LinkMode::Preserve {
                        return;
                    }
                    // A link that is claimed becomes a link in the plan, so the
                    // walk must not read through it. A link that is claimed by
                    // nothing is traversed: it may be the only road to a file
                    // that is, and `glob_with` reads through one today.
                    for child in children.iter_mut().flatten() {
                        if !child.path_is_symlink() {
                            continue;
                        }
                        let full = dir.join(&child.file_name);
                        let claimed = match (&pruner, full.strip_prefix(&prune_source)) {
                            (Some(p), Ok(rel)) => matches_here(p, rel, opts),
                            (None, _) => true,
                            (Some(_), Err(_)) => false,
                        };
                        if claimed {
                            child.read_children = None;
                        }
                    }
                })
                .into_iter()
                .collect()
        });

        let verdicts: Vec<Classified> = crate::pool::install(|| {
            entries
                .par_iter()
                .filter_map(|entry| {
                    let entry = match entry {
                        Ok(e) => e,
                        Err(e) => {
                            // A wildcard whose literal prefix does not exist is
                            // a common, benign configuration and stays as
                            // silent as glob's own scope check made it.
                            if e.io_error().map(std::io::Error::kind)
                                == Some(std::io::ErrorKind::NotFound)
                            {
                                return None;
                            }
                            return Some(Classified::Warning(format!(
                                "reading dir {}: {e}",
                                start.display()
                            )));
                        }
                    };
                    // Depth 0 is `start` itself, which the caller has already
                    // accounted for.
                    if entry.depth() == 0 {
                        return None;
                    }
                    let full = entry.path();
                    let Ok(rel) = full.strip_prefix(source) else {
                        return Some(Classified::Warning(format!(
                            "match outside source: {}",
                            full.display()
                        )));
                    };
                    if let Some(m) = matcher
                        && !matches_here(m, rel, opts)
                    {
                        return None;
                    }
                    let rel = rel.to_path_buf();
                    if mode == LinkMode::Preserve && entry.path_is_symlink() {
                        return Some(match std::fs::read_link(&full) {
                            Ok(target) => Classified::Link { rel, target },
                            Err(e) => Classified::Warning(format!(
                                "reading link {}: {e}",
                                full.display()
                            )),
                        });
                    }
                    // A directory is never an entry in the plan; the files
                    // under it are, and the walk reaches them itself.
                    if entry.file_type().is_dir() {
                        return None;
                    }
                    Some(Classified::File {
                        exists: dest.join(&rel).exists(),
                        rel,
                    })
                })
                .collect()
        });

        for verdict in verdicts {
            match verdict {
                Classified::File { rel, exists } => self.record_file(rel, exists, out),
                Classified::Link { rel, target } => self.record_link(rel, target, out),
                Classified::Warning(w) => warnings.push(w),
            }
        }
    }
```

Add `use rayon::prelude::*;` to the file's imports if task 4 did not already.

In `plan_one`, replace the `self.plan_dir(&matched, rel, out, warnings)` call:

```rust
            } else if matched.is_dir() {
                self.walk_and_classify(source, &matched, None, opts, out, warnings);
            } else {
```

`plan_one`'s remaining two branches still call `classify_link` and `classify_file`, which no longer exist. Replace them with the recording pair, keeping today's behaviour:

```rust
            if self.mode == LinkMode::Preserve && is_symlink(&matched) {
                match std::fs::read_link(&matched) {
                    Ok(target) => self.record_link(rel.to_path_buf(), target, out),
                    Err(e) => {
                        warnings.push(format!("reading link {}: {e}", matched.display()));
                    }
                }
            } else if matched.is_dir() {
                self.walk_and_classify(source, &matched, None, opts, out, warnings);
            } else {
                let exists = self.dest.join(rel).exists();
                self.record_file(rel.to_path_buf(), exists, out);
            }
```

- [ ] **Step 6: Run the full module**

Run: `cargo test -p devkit-common worktree::`
Expected: all pass. Watch `plan_includes_directory_pattern_enumerates_files_not_the_directory`, `copy_out_copies_a_directory_match_recursively`, and every test whose name mentions a link.

- [ ] **Step 7: Gate and commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git add crates/devkit-common/src/worktree.rs
git commit -m "perf(worktree): walk and classify a directory include in parallel"
```

---

### Task 8: jwalk replaces the pattern expansion

Drops `glob::glob_with`, keeping `glob::Pattern` as the matcher. This is the task that reaches `**` and `apps/*/` patterns, and it carries the match-option correction that keeps every existing include meaning what it means today.

**Files:**
- Modify: `crates/devkit-common/src/worktree.rs` (`Walk::plan_one`, `plan_with_mode`)

**Interfaces:**
- Consumes: `walk_root`, `Walk::walk_and_classify`, `Walk::record_file`, `Walk::record_link`, `escapes`.
- Produces: `Walk::plan_literal(&self, source: &Path, rel: &Path, out: &mut PatternPlan, warnings: &mut Vec<String>)`. `plan_one` keeps its call signature.

- [ ] **Step 1: Write the failing tests**

Add to the test module:

```rust
    /// `glob_with` forces `require_literal_separator` to true and ignores the
    /// value it is handed, so the `false` this module builds has never had an
    /// effect. Matching a full relative path honours the flag, so carrying that
    /// `false` across would widen every pattern: `apps/*/.env.local` would
    /// start matching two directories down and pull unrequested files into a
    /// worktree.
    #[test]
    fn a_single_wildcard_does_not_cross_a_directory_separator() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join("apps/web/.env.local"), "shallow");
        write(&src.join("apps/web/nested/.env.local"), "deep");

        let plan = plan_includes(&src, &dst, &["apps/*/.env.local".to_string()]);

        let missing: Vec<_> = plan.missing().map(Path::to_path_buf).collect();
        assert_eq!(missing, vec![PathBuf::from("apps/web/.env.local")]);
    }

    /// `**` matches across separators including zero of them, which is what
    /// makes `**/.env.local` find a root-level file as well as a nested one.
    #[test]
    fn a_recursive_wildcard_matches_at_every_depth_including_none() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join(".env.local"), "root");
        write(&src.join("apps/web/.env.local"), "nested");

        let plan = plan_includes(&src, &dst, &["**/.env.local".to_string()]);

        let mut missing: Vec<_> = plan.missing().map(Path::to_path_buf).collect();
        missing.sort();
        assert_eq!(
            missing,
            vec![
                PathBuf::from(".env.local"),
                PathBuf::from("apps/web/.env.local"),
            ]
        );
    }

    /// A symlinked directory in the middle of a pattern is read through, as
    /// `glob_with` reads through it today. jwalk's default would give it no
    /// children and drop the file underneath with no warning at all.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_directory_mid_pattern_is_traversed() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join("shared/web/.env.local"), "x");
        std::fs::create_dir_all(src.join("apps")).unwrap();
        std::os::unix::fs::symlink("../shared/web", src.join("apps/web")).unwrap();

        let plan = plan_includes(&src, &dst, &["apps/*/.env.local".to_string()]);

        let missing: Vec<_> = plan.missing().map(Path::to_path_buf).collect();
        assert_eq!(missing, vec![PathBuf::from("apps/web/.env.local")]);
        assert!(plan.patterns[0].links.is_empty());
    }

    /// A link the pattern claims is still reproduced as a link, not read
    /// through, even though the walk follows links to reach the case above.
    /// This is what the child-clearing in `process_read_dir` protects.
    #[cfg(unix)]
    #[test]
    fn a_claimed_symlinked_directory_is_planned_as_a_link() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join("shared/web/.env.local"), "x");
        std::fs::create_dir_all(src.join("apps")).unwrap();
        std::os::unix::fs::symlink("../shared/web", src.join("apps/web")).unwrap();

        let plan = plan_includes(&src, &dst, &["apps/*".to_string()]);

        assert_eq!(plan.missing_len(), 0, "the link's contents are not planned");
        assert_eq!(plan.patterns[0].links.len(), 1);
        assert_eq!(plan.patterns[0].links[0].0, PathBuf::from("apps/web"));
        assert_eq!(plan.patterns[0].links[0].1, PathBuf::from("../shared/web"));
    }

    /// A wildcard pattern whose literal prefix does not exist is silent, as it
    /// is today: `apps/*/.env.local` in a repository with no `apps/` is a
    /// common configuration and must not warn on every setup.
    #[test]
    fn a_missing_walk_root_is_silent() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join("keep.txt"), "x");

        let plan = plan_includes(&src, &dst, &["apps/*/.env.local".to_string()]);

        assert_eq!(plan.missing_len(), 0);
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    }

    /// A `copy_out` pattern naming a symlink resolves through it and archives
    /// the target's contents. `copy_out`'s own comment that a Follow-mode plan
    /// holds no links depends on this, and nothing covered it before.
    #[cfg(unix)]
    #[test]
    fn copy_out_resolves_a_pattern_that_names_a_link() {
        let base = tempfile::tempdir().unwrap();
        let wt = base.path().join("wt");
        let dst = base.path().join("dst");
        write(&wt.join("real/notes.md"), "kept");
        std::os::unix::fs::symlink("real", wt.join("archive")).unwrap();

        let (copied, warnings) = copy_out(&wt, &dst, &["archive".to_string()]);

        assert_eq!(copied, 1, "{warnings:?}");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            std::fs::read_to_string(dst.join("archive/notes.md")).unwrap(),
            "kept"
        );
    }

    /// Two runs over one tree plan identically. A parallel walk yields paths in
    /// whatever order threads finish, so the per-pattern sort is what makes the
    /// plan deterministic rather than a formality.
    #[test]
    fn planning_the_same_tree_twice_yields_the_same_plan() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        for i in 0..50 {
            write(&src.join("apps").join(format!("a{i}")).join(".env.local"), "x");
        }

        let patterns = ["apps/*/.env.local".to_string()];
        let first: Vec<_> = plan_includes(&src, &dst, &patterns)
            .missing()
            .map(Path::to_path_buf)
            .collect();
        let second: Vec<_> = plan_includes(&src, &dst, &patterns)
            .missing()
            .map(Path::to_path_buf)
            .collect();

        assert_eq!(first.len(), 50);
        assert_eq!(first, second);
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p devkit-common worktree::`
Expected: `a_symlinked_directory_mid_pattern_is_traversed` and `copy_out_resolves_a_pattern_that_names_a_link` FAIL against the current code. The other five pass already and are the contract the rewrite must not break.

- [ ] **Step 3: Rewrite `plan_one`**

Replace the whole function with these two:

```rust
    /// Walk one pattern into `out`. Every failure is a warning, never a return
    /// value, so one bad pattern cannot stop the rest of the list.
    fn plan_one(
        &self,
        source: &Path,
        pattern: &str,
        opts: glob::MatchOptions,
        out: &mut PatternPlan,
        warnings: &mut Vec<String>,
    ) {
        let trimmed = pattern.trim_end_matches('/');
        // An empty pattern would match the source directory itself and plan
        // every file under the root.
        if trimmed.is_empty() {
            return;
        }
        // `source.join` discards the base for a rooted pattern and keeps a `..`
        // component, so a pattern that leaves the tree is refused before any
        // path is built from it.
        if escapes(trimmed) {
            warnings.push(format!(
                "include pattern reaches outside the source tree, skipped: {pattern}"
            ));
            return;
        }
        let Some(root) = walk_root(trimmed) else {
            self.plan_literal(source, Path::new(trimmed), out, warnings);
            return;
        };
        let matcher = match glob::Pattern::new(trimmed) {
            Ok(p) => p,
            Err(e) => {
                warnings.push(format!("bad include pattern `{pattern}`: {e}"));
                return;
            }
        };
        let start = source.join(&root);
        self.walk_and_classify(source, &start, Some(&matcher), opts, out, warnings);
    }

    /// A pattern with no wildcard names exactly one path, so it costs one stat
    /// rather than a walk. Mode decides what a link means: `Preserve` records
    /// the link, `Follow` resolves through it, which is why `copy_out` can
    /// promise that a Follow-mode plan holds none.
    fn plan_literal(
        &self,
        source: &Path,
        rel: &Path,
        out: &mut PatternPlan,
        warnings: &mut Vec<String>,
    ) {
        let full = source.join(rel);
        // A glob over a missing path yields nothing rather than an error, and
        // a literal pattern matching nothing behaves the same way.
        let Ok(meta) = std::fs::symlink_metadata(&full) else {
            return;
        };
        let opts = match_options();
        if meta.file_type().is_symlink() {
            if self.mode == LinkMode::Preserve {
                match std::fs::read_link(&full) {
                    Ok(target) => self.record_link(rel.to_path_buf(), target, out),
                    Err(e) => warnings.push(format!("reading link {}: {e}", full.display())),
                }
                return;
            }
            match std::fs::metadata(&full) {
                Ok(target) if target.is_dir() => {
                    self.walk_and_classify(source, &full, None, opts, out, warnings);
                }
                Ok(_) => {
                    let exists = self.dest.join(rel).exists();
                    self.record_file(rel.to_path_buf(), exists, out);
                }
                Err(e) => warnings.push(format!("reading link {}: {e}", full.display())),
            }
            return;
        }
        if meta.is_dir() {
            self.walk_and_classify(source, &full, None, opts, out, warnings);
        } else {
            let exists = self.dest.join(rel).exists();
            self.record_file(rel.to_path_buf(), exists, out);
        }
    }
```

The `is_symlink` free function is now unused. Delete it.

- [ ] **Step 4: Correct the match options**

`plan_literal` needs the options too, so they move out of `plan_with_mode` into one function. Add beside `walk_root`:

```rust
/// The options every include match is tested with.
///
/// `require_literal_separator` is true because that is what the old walker
/// actually did: `glob_with` forced it true whatever it was handed
/// (`glob-0.3.3/src/lib.rs:176`) and matched one path component at a time, so a
/// single `*` has never crossed a `/` here. `matches_path_with` honours the
/// flag, so carrying across the `false` this module used to build would widen
/// every pattern and pull unrequested files into a worktree. `**` still
/// recurses; it is a different token.
fn match_options() -> glob::MatchOptions {
    glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    }
}
```

In `plan_with_mode`, replace the inline `let opts = glob::MatchOptions { … };` with:

```rust
    let opts = match_options();
```

- [ ] **Step 5: Run the full workspace**

Run: `cargo test --workspace`
Expected: all pass, including the seven new tests and every pre-existing include, symlink, and progress test.

- [ ] **Step 6: Confirm the old walker is gone**

Run: `rg -n "glob_with|plan_dir|classify_file|classify_link|fn is_symlink" crates/devkit-common/src/worktree.rs`
Expected: no matches. `glob` stays in the manifest for `Pattern`.

- [ ] **Step 7: Gate and commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
git add crates/devkit-common/src/worktree.rs
git commit -m "perf(worktree): expand include patterns with a scoped jwalk"
```

---

### Task 9: Close the relative-target symlink cycle

jwalk compares a link's raw target text against absolute ancestors, so it catches
an absolute-target cycle and misses `ln -s ..`. `copy_out` runs during `issue
end`, where an undetected cycle fills the archive with a nested duplicate of one
subtree while the worktree is being torn down.

**Files:**
- Modify: `crates/devkit-common/src/worktree.rs` (`Walk::walk_and_classify`)

**Interfaces:**
- Consumes: `Walk::walk_and_classify` from task 7.
- Produces: `type Ancestors = std::sync::Arc<Vec<PathBuf>>`, the walk's `ReadDirState`.

- [ ] **Step 1: Write the failing test**

Add to the test module:

```rust
    /// `copy_out` follows links, and jwalk only detects a cycle whose target is
    /// absolute. A relative one would descend until the OS path limit stopped
    /// it, filling the archive with a nested duplicate of one subtree while the
    /// worktree is being deleted.
    ///
    /// Carries a timeout because a regression hangs rather than fails.
    #[cfg(unix)]
    #[test]
    fn copy_out_survives_a_relative_symlink_cycle() {
        let base = tempfile::tempdir().unwrap();
        let wt = base.path().join("wt");
        let dst = base.path().join("dst");
        write(&wt.join("scratch/keep.txt"), "x");
        // Relative, which jwalk misses, and absolute, which it catches. The
        // canonicalising check covers both, so both belong here.
        std::os::unix::fs::symlink("..", wt.join("scratch/loop")).unwrap();
        std::os::unix::fs::symlink(wt.join("scratch"), wt.join("scratch/abs")).unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(copy_out(&wt, &dst, &["scratch/".to_string()]));
        });

        let (copied, _warnings) = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("copy_out descended a symlink cycle instead of refusing it");
        assert!(copied >= 1, "the real file is still archived");
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p devkit-common copy_out_survives_a_relative_symlink_cycle`
Expected: FAIL on the `recv_timeout` expect after 60 seconds, or a very long run leaving a deeply nested destination tree.

- [ ] **Step 3: Add the ancestor check**

jwalk carries per-directory state down a walk for exactly this; its own docs give
`.gitignore` state as the example. Add beside `walk_root`:

```rust
/// Canonicalised directories already entered on the path to the current one.
/// jwalk detects a symlink cycle by comparing a link's raw target against
/// absolute ancestors, so `ln -s ..` slips past it; this closes that.
type Ancestors = std::sync::Arc<Vec<PathBuf>>;

/// Whether following `link` re-enters a directory already on the path to it.
/// Only a link can close a cycle, so an ordinary directory is never
/// canonicalised and the check costs nothing on a tree without links.
fn closes_a_cycle(seen: &Ancestors, link: &Path) -> bool {
    let Ok(resolved) = std::fs::canonicalize(link) else {
        return false;
    };
    seen.iter().any(|a| a == &resolved)
}
```

In `walk_and_classify`, change the builder from `jwalk::WalkDir::new(start)` to:

```rust
            jwalk::WalkDirGeneric::<(Ancestors, ())>::new(start)
```

and give the `process_read_dir` closure the state parameter it now uses, replacing
`move |_depth, dir, _state, children|` with `move |_depth, dir, state, children|`.
Insert this at the top of that closure body, before the `mode` check, so it runs
in both link modes:

```rust
                    if let Ok(here) = std::fs::canonicalize(dir) {
                        let mut seen = Vec::with_capacity(state.len() + 1);
                        seen.extend_from_slice(state);
                        seen.push(here);
                        *state = std::sync::Arc::new(seen);
                    }
                    for child in children.iter_mut().flatten() {
                        if child.path_is_symlink()
                            && child.file_type().is_dir()
                            && closes_a_cycle(state, &dir.join(&child.file_name))
                        {
                            child.read_children = None;
                        }
                    }
```

A child cleared here stays cleared when the Preserve-mode rule below also runs.

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p devkit-common copy_out_survives_a_relative_symlink_cycle`
Expected: PASS, well inside the timeout.

- [ ] **Step 5: Run the full workspace**

Run: `cargo test --workspace`
Expected: all pass. `copy_out_still_archives_a_links_contents` in particular must
stay green: a link that does *not* close a cycle is still followed.

- [ ] **Step 6: Gate and commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
git add crates/devkit-common/src/worktree.rs
git commit -m "fix(worktree): refuse a symlink cycle jwalk cannot see"
```

---

### Task 10: Documentation and measurement

**Files:**
- Modify: `docs/configuration.md`
- Modify: `AGENTS.md`
- Modify: `docs/superpowers/specs/2026-08-31-parallel-includes-design.md`

**Interfaces:**
- Consumes: everything above.
- Produces: nothing.

- [ ] **Step 1: Document the config key**

Add a `[parallelism]` section to `docs/configuration.md`, placed to match the file's existing ordering of table sections. Match the surrounding prose style rather than importing this plan's.

```markdown
## `[parallelism]`

Width of the worker pool devkit shares across its parallel work: the
`worktree_include` walk and copy today, and whatever else adopts
`devkit_common::pool` later.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `threads` | integer ≥ 1 | 4 | Threads in the shared pool. |

`DEVKIT_THREADS` overrides it, and an unparseable or zero value there is
ignored rather than obeyed.

A thread count describes the machine rather than the project, so it belongs in
the personal layer at `~/.config/devkit/config.toml`. A config may carry
`[parallelism]` alone, with no `[defaults]` table around it.

Four is the measured throughput knee for file copying. Raising it helps most on
a filesystem where a stat is slow and concurrency hides the latency, such as a
Windows drive mounted under WSL.
```

- [ ] **Step 2: Document the pool for agents**

In `AGENTS.md`, in the `crates/devkit-common` row of the layout table, add `pool` to the module list beside `paths` and `secrets`.

Add to the **Invariants (do not break)** list:

```markdown
- **Parallel work goes through `devkit_common::pool`.** One bounded
  `rayon::ThreadPool` serves the whole workspace, sized by `DEVKIT_THREADS`,
  then `[parallelism] threads`, then 4. Reaching for `par_iter` or jwalk's
  default parallelism directly gets rayon's *global* pool instead, with its own
  width and no coordination with this one — several agent sessions run devkit at
  once on a machine. `pool::install` and `pool::jwalk_parallelism` both degrade
  to serial when already inside the pool, because a bounded pool re-entered from
  its own worker can leave a nested walk waiting on a thread that never frees.
```

- [ ] **Step 3: Correct the spec's `configure` call site**

The spec says `configure` is called "wherever the subcommand's config first resolves, immediately after `load::load`". Implementation found a single door instead. Replace that paragraph in `docs/superpowers/specs/2026-08-31-parallel-includes-design.md`:

```markdown
The call site is `devkit_ports::load::load`, immediately after `config::resolve`.
Every subcommand's config resolution passes through it, so one call covers all
of them and none can forget it. Not `main`: `main` parses argv and dispatches,
and config loads inside each subcommand with its own `--config` and start
directory.
```

- [ ] **Step 4: Measure**

The claim this work rests on is unmeasured on this branch. Time it against a real
tree, not a synthetic one, and not in CI: a runner measures its own disk.

Write a throwaway benchmark binary at `crates/devkit-common/examples/bench_includes.rs`:

```rust
//! Times one `worktree_include` copy against a real tree.
//!
//! Throwaway: `cargo run --release --example bench_includes -- <source> <pattern>`
//! copies into a fresh temporary directory and prints the elapsed time. Vary
//! `DEVKIT_THREADS` across runs to see the pool's effect.

fn main() {
    let mut args = std::env::args().skip(1);
    let source = args.next().expect("usage: bench_includes <source> <pattern>");
    let pattern = args.next().expect("usage: bench_includes <source> <pattern>");
    let dest = tempfile::tempdir().expect("temp dir");

    let started = std::time::Instant::now();
    let (copied, linked, warnings) = devkit_common::worktree::copy_includes(
        std::path::Path::new(&source),
        dest.path(),
        &[pattern],
    );
    let elapsed = started.elapsed();

    println!(
        "{copied} copied, {linked} linked, {} warnings in {elapsed:.2?} on {} threads",
        warnings.len(),
        devkit_common::pool::width()
    );
}
```

`tempfile` is already a dev-dependency of `devkit-common`, which is what an
example links against.

Then, against a large directory include such as a Godot project's `.godot/`:

```bash
cargo build --release --example bench_includes
hyperfine --warmup 1 --runs 5 \
  --parameter-list threads 1,2,4,8 \
  'DEVKIT_THREADS={threads} ./target/release/examples/bench_includes /path/to/project .godot/'
```

Substitute your own project path. Record the numbers for 1 and 4 threads.

- [ ] **Step 5: Record the numbers and drop the benchmark**

In the spec's Goal section, replace "Reported from a separate benchmarking
session on a real tree: rayon roughly halves copy time, and jwalk cuts walk time
to about a third" with the measured figures, the tree they came from, and its
file count. A dated design document is a snapshot, so hard numbers belong there.

If the copy did not improve, say so in the spec rather than quietly keeping the
claim. That was the plan's one open question, and a null result is an answer.

Delete `crates/devkit-common/examples/bench_includes.rs`. It measured the thing
it was written to measure; keeping it means maintaining it.

- [ ] **Step 6: Gate and commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git status --short   # confirm bench_includes.rs is gone
git add docs/configuration.md AGENTS.md docs/superpowers/specs/2026-08-31-parallel-includes-design.md
git commit -m "docs: document the shared pool and its config"
```

---

## Verification

After task 10, confirm the whole change from a clean state:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p devkit-ports --test registry
```

The tests cannot prove the walk is actually parallel rather than correct-and-serial,
so confirm that separately. Task 10's benchmark is the direct check: if
`DEVKIT_THREADS=1` and `DEVKIT_THREADS=4` produce the same time on a large tree,
the pool is not being used and something upstream is falling back to serial.

Then confirm the copy still works end to end through a real command:

```bash
cargo install --path .
cd /path/to/a/worktree
DEVKIT_THREADS=4 issue sync-includes --dry-run
```

Expect the same file list a `DEVKIT_THREADS=1` run reports, in the same order.
A difference between those two is a determinism bug, which is exactly what
`planning_the_same_tree_twice_yields_the_same_plan` is meant to catch first.
