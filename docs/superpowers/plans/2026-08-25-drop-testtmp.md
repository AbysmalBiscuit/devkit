# Drop devkit-testtmp for tempfile Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete the `devkit-testtmp` crate and have every test take its scratch space from `tempfile` directly.

**Architecture:** `devkit-testtmp` wraps `tempfile::TempDir` to add `Deref<Target = Path>` and `AsRef<OsStr>`, plus a `path(prefix, name)` helper for tests that want a file rather than a directory. The wrapper is more surface than the job needs, and its `<prefix>-<random>` naming causes a real bug (below). Every call site funnels through a per-module helper (`scratch`/`tmp`/`unique_tmp`), so the conversion is ~30 helper signatures plus compiler-driven `.path()` fixes at the uses.

**Tech Stack:** Rust 2024, `tempfile` 3 (already a workspace dependency).

**Spec:** none — this is a refactor with a named bug attached. This document is the authority.

## The bug this closes

`devkit_testtmp::dir(prefix)` names its directory `<prefix>-<random>`, and
`tempfile`'s random component is alphanumeric. `devkit_common::worktree::find_id`
scans for the first letters-dash-digits run, so when the random component happens
to start with a digit the helper's own dash forms an id:

```
devkit.idnone-7x9k  ->  find_id sees "idnone-7"  ->  issue_id_of returns "IDNONE-7"
```

That makes `worktree::tests::a_worktree_with_neither_is_unknown` fail roughly one
run in six. `tempfile::tempdir()` names directories `.tmpXXXXXX` with no dash, so
the move closes the bug on its own. Task 1 additionally rewrites that test to
assert against a worktree directory it names itself, so the invariant no longer
rides on any temp-crate's naming.

## Global Constraints

- `devkit_testtmp::dir(tag)` becomes `tempfile::tempdir().unwrap()`. The `tag`
  argument is dropped; `tempfile` does not take a prefix here and the tag was
  only ever a debugging label on a self-deleting directory.
- `devkit_testtmp::TmpDir` becomes `tempfile::TempDir`.
- **Helper rule.** If a helper's entire body is `devkit_testtmp::dir(tag)`,
  delete the helper and inline `tempfile::tempdir().unwrap()` at its call sites.
  If the helper does more than that (creates directories, runs `git init`, builds
  a fixture), keep it and delete its now-unused `tag` parameter, updating callers.
- `tempfile::TempDir` has **no `Deref`** and **no `AsRef<OsStr>`**. It does impl
  `AsRef<Path>`. So:
  - `d.join(x)` becomes `d.path().join(x)`
  - `d.to_str()`, `d.to_path_buf()`, `d.display()`, `d.exists()` become `d.path().…`
  - `&d` stays as-is where an `AsRef<Path>` is expected
  - `&d` becomes `d.path()` in `Command::arg`, `Command::env`, `format!` and any
    other `AsRef<OsStr>` or `Display` position
  - Let the compiler find these. Do not guess at the list.
- `devkit_testtmp::path(prefix, name)` (returns a path with its guard attached)
  becomes a helper returning `(tempfile::TempDir, PathBuf)`; callers bind
  `let (_guard, p) = helper(…);`. The guard binding must outlive every use of the
  path — a `let _ = …` discard drops it immediately and deletes the directory.
- Add `tempfile.workspace = true` to `[dev-dependencies]` of any crate that gains
  a direct `tempfile` use and does not already have one. Use the workspace form.
- No non-test code changes. Every edit in Tasks 1-4 is inside a `#[cfg(test)]`
  module, an integration test, or a test-only helper.
- Do not delete `crates/devkit-testtmp` before Task 5; it stays compiling so each
  crate can convert independently.
- Per-task gate: the touched crate's own `cargo test -p <crate>` and
  `cargo clippy -p <crate> --all-targets -- -D warnings` must pass.
- Final gate (Task 5): `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`.
- When piping a cargo command into `tail`/`head`, set `set -o pipefail` first or
  the pipeline reports the pager's exit status, not cargo's.

---

### Task 1: devkit-common

**Files:**
- Modify: `crates/devkit-common/src/gitignore.rs`, `paths.rs`, `record.rs`,
  `supervise.rs`, `tracker/mod.rs`, `worktree.rs`, `secrets.rs`, `gitfetch.rs`,
  `store.rs`
- Modify: `crates/devkit-common/Cargo.toml` (drop `devkit-testtmp`; it already has
  a `tempfile` dev-dependency — restate it as `tempfile.workspace = true` to match
  every other crate)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: nothing later tasks consume. Each crate converts independently.

- [ ] **Step 1: Convert the `TmpDir` sites**

`worktree.rs` has a `fn tmp(tag: &str) -> devkit_testtmp::TmpDir` helper plus
direct `devkit_testtmp::dir(…)` calls; `gitignore.rs`, `paths.rs`, `record.rs`,
`supervise.rs` and `tracker/mod.rs` have one call each. Apply the Global
Constraints helper rule to each.

- [ ] **Step 2: Rewrite the flaky worktree test**

Replace `worktree.rs`'s `a_worktree_with_neither_is_unknown` with a version that
names the worktree directory itself, so the assertion does not depend on how any
temp-file crate names its directories:

```rust
#[test]
fn a_worktree_with_neither_is_unknown() {
    // The directory name is a fallback id source, so the worktree is given a
    // name carrying no letters-dash-digits run rather than the scratch
    // directory's own.
    let scratch = tempfile::tempdir().unwrap();
    let worktree = scratch.path().join("noidhere");
    std::fs::create_dir_all(&worktree).unwrap();
    assert_eq!(issue_id_of(&worktree, "lev/no-id-here"), "UNKNOWN");
}
```

- [ ] **Step 3: Convert the three `TmpPath` sites**

`secrets.rs:160`, `gitfetch.rs:110` and `store.rs:246` each have a helper
returning `devkit_testtmp::TmpPath`. Convert each to return
`(tempfile::TempDir, std::path::PathBuf)`. For example `store.rs`:

```rust
/// A file path that does not exist yet — `tag` names the lock or the
/// document, and the store creates whichever it is handed. The guard comes
/// back with it: dropping the guard removes the directory around the file.
fn scratch(tag: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(tag);
    (dir, path)
}
```

Callers become `let (_guard, lock) = scratch("a.lock");`. Note `store.rs` calls
`scratch` twice in one test for two different files — keep two separate guards.
In `secrets.rs` the `tag` names nothing (the file is always `secrets.toml`), so
its helper keeps the `name` argument only if it still distinguishes something;
if it does not, drop the parameter per the helper rule.

- [ ] **Step 4: Verify**

```bash
cargo test -p devkit-common
cargo clippy -p devkit-common --all-targets -- -D warnings
```
Both must pass. Run `a_worktree_with_neither_is_unknown` ten times in a row to
confirm the flake is gone:
```bash
for i in $(seq 10); do cargo test -p devkit-common worktree::tests::a_worktree_with_neither_is_unknown -- --exact || break; done
```

- [ ] **Step 5: Commit**

```bash
git add -A crates/devkit-common
git commit -m "test(common): take scratch space from tempfile"
```

---

### Task 2: devkit-config, devkit-locks, devkit-ports, devkit-issue, devkit-mcp

These are the same mechanical shape as Task 1 with no `TmpPath` sites. Do all
five in one pass.

**Files:**
- Modify: `crates/devkit-config/src/lib.rs`
- Modify: `crates/devkit-locks/src/hook.rs`, `src/lib.rs`, `src/store.rs`,
  `tests/memory_store.rs`
- Modify: `crates/devkit-ports/src/daemon/client.rs`, `src/registry.rs`,
  `src/run.rs`, `tests/memory_store.rs`, `tests/registry.rs`, `tests/task_gate.rs`
- Modify: `crates/devkit-issue/src/status.rs`, `tests/gather_local.rs`
- Modify: `crates/devkit-mcp/src/locks.rs`, `tests/issue_status_tracker.rs`
- Modify: each of those five `Cargo.toml` files (swap `devkit-testtmp.workspace =
  true` for `tempfile.workspace = true` in `[dev-dependencies]`; `devkit-config`
  already has `tempfile.workspace = true`, so there just drop the `devkit-testtmp`
  line)

**Interfaces:**
- Consumes: nothing. Produces: nothing.

- [ ] **Step 1: Convert each crate**

Apply the Global Constraints to every site. `devkit-config/src/lib.rs:1298`,
`devkit-locks/src/lib.rs:352`, `devkit-locks/src/store.rs:325`,
`devkit-locks/tests/memory_store.rs:5`, `devkit-ports/src/registry.rs:1002`,
`devkit-issue/tests/gather_local.rs:18` and
`devkit-mcp/tests/issue_status_tracker.rs:21` are the helper definitions;
`fixture_repo` and `fixture` do real work, so they keep their bodies and lose only
their unused parameters if the parameters become unused.

- [ ] **Step 2: Verify**

```bash
for c in devkit-config devkit-locks devkit-ports devkit-issue devkit-mcp; do
  cargo test -p $c && cargo clippy -p $c --all-targets -- -D warnings || break
done
```
All five must pass. `devkit-ports`' `tests/registry.rs` is the multiprocess flock
race test — it must stay green.

- [ ] **Step 3: Commit**

```bash
git add -A crates/devkit-config crates/devkit-locks crates/devkit-ports crates/devkit-issue crates/devkit-mcp
git commit -m "test(crates): take scratch space from tempfile"
```

---

### Task 3: devkit-docs

Kept separate because its tests thread the scratch guard through fixture helpers
that return it alongside derived paths — the place the guard-lifetime rule
actually bites.

**Files:**
- Modify: `crates/devkit-docs/src/cache.rs`, `src/layout.rs`, `src/lib.rs`,
  `src/lockfiles.rs`, `src/manifest.rs`, `src/refs.rs`
- Modify: every integration test under `crates/devkit-docs/tests/` that calls
  the shared `common::unique_tmp` helper — deleting it reaches all of them, not
  only `common/mod.rs`, `concurrency.rs`, `names.rs`, `refs_race.rs` and
  `doctor.rs`. Enumerate the callers with `rg` before starting.
- Modify: `crates/devkit-docs/Cargo.toml`

**Interfaces:**
- Consumes: nothing. Produces: nothing.

- [ ] **Step 1: Convert**

`tests/common/mod.rs:6`'s `unique_tmp(tag) -> TmpDir` is the shared fixture
helper; `tests/doctor.rs:10`'s `materialize` already returns the guard first in a
tuple and only needs its type and `.join` calls updated. The six `src/*.rs`
helpers are the plain `unique_tmp` shape.

- [ ] **Step 2: Verify**

```bash
cargo test -p devkit-docs
cargo clippy -p devkit-docs --all-targets -- -D warnings
```

- [ ] **Step 3: Commit**

```bash
git add -A crates/devkit-docs
git commit -m "test(docs): take scratch space from tempfile"
```

---

### Task 4: root package — binaries and integration tests

**Files:**
- Modify: `src/bin/devkit/auth.rs`, `src/bin/devkitd/cgroup.rs`,
  `src/bin/devkitd/supervisor.rs`, `src/bin/devrun/baseline.rs`,
  `src/bin/issue/checkout.rs`, `end.rs`, `info.rs`, `info_cache.rs`, `prs.rs`,
  `setup.rs`, `summary.rs`, `tracker.rs`, `dashboard/cache.rs`
- Modify: `tests/brief_pins.rs`, `tests/cli_ergonomics.rs`, `tests/common/mod.rs`,
  `tests/devrun_down_gate.rs`, `tests/docm_cli.rs`, `tests/docm_reporting.rs`,
  `tests/lock_harness_race.rs`, `tests/locks.rs`, `tests/mcp.rs`,
  `tests/schema_init.rs`
- Modify: `Cargo.toml` (drop `devkit-testtmp.workspace = true` from
  `[dev-dependencies]`; `tempfile.workspace = true` is already there)

**Interfaces:**
- Consumes: nothing. Produces: nothing.

- [ ] **Step 1: Convert the `TmpPath` sites**

Three here: `src/bin/devkit/auth.rs:192`, `src/bin/issue/dashboard/cache.rs:111`
and `src/bin/devkitd/supervisor.rs:526`. Same `(TempDir, PathBuf)` treatment as
Task 1 Step 3.

- [ ] **Step 2: Convert the `TmpDir` sites**

`tests/common/mod.rs:62` holds the guard as a **struct field** (`pub home:
devkit_testtmp::TmpDir` on `Harness`) — change the field type and fix every read
of `harness.home` that relied on `Deref` or `AsRef<OsStr>`. The daemon harness
passes `home` into `Command::env`, which is an `AsRef<OsStr>` position and will
need `.path()`.

`tests/docm_cli.rs:31`'s `temp_root` returns `(TmpDir, PathBuf)` already and calls
`guard.to_path_buf()` and `canonicalize(&guard)` — the first needs `.path()`, the
second is an `AsRef<Path>` position and can stay.

`tests/brief_pins.rs:22` and `tests/docm_cli.rs:62` hold the guard as `_scratch`
struct fields; only the type changes.

- [ ] **Step 3: Verify**

```bash
set -o pipefail
cargo test -p devkit --all-targets
cargo clippy -p devkit --all-targets -- -D warnings
```

- [ ] **Step 4: Commit**

```bash
git add -A src tests Cargo.toml
git commit -m "test(devkit): take scratch space from tempfile"
```

---

### Task 5: Delete the crate

**Files:**
- Delete: `crates/devkit-testtmp/` (the whole directory)
- Modify: `Cargo.toml` (drop `crates/devkit-testtmp` from `workspace.members` and
  the `devkit-testtmp = { path = … }` line from `workspace.dependencies`)
- Modify: `Cargo.lock`
- Modify: `AGENTS.md` (the crate's row in the layout table, and the "Test scratch
  comes from `devkit-testtmp`" convention bullet)

**Interfaces:**
- Consumes: every earlier task must be complete — the workspace will not build
  otherwise.

- [ ] **Step 1: Confirm nothing references it**

```bash
rg -n 'devkit[-_]testtmp' -g '!Cargo.lock' .
```
Expected: only `Cargo.toml`, `AGENTS.md` and `crates/devkit-testtmp/` itself. If
any other file appears, that file's task was not finished — fix it here.

- [ ] **Step 2: Delete and unwire**

Remove the directory, both `Cargo.toml` entries, and run `cargo check --workspace`
to refresh `Cargo.lock`.

- [ ] **Step 3: Rewrite the AGENTS.md convention bullet**

Replace the `devkit-testtmp` bullet under Conventions with one that states the
rule that still holds, in the house style (no hard numbers, timeless):

```markdown
- Test scratch comes from `tempfile`: `tempfile::tempdir()` for a directory, a
  path joined onto one for a file. Never build a scratch path by hand from
  `std::env::temp_dir()` — a hand-built path outlives the test and fills `/tmp`.
  `TempDir` deletes its tree on drop, so bind it for as long as the path is used:
  a helper that returns a path derived from a guard must hand back the guard too,
  or the directory is gone before the caller reads it.
```

Delete the crate's row from the layout table. The table's prose says "eight
library crates" and "Eight library crates are members" — correct both counts, or
better, reword to avoid a number that goes stale.

- [ ] **Step 4: Full gate**

```bash
set -o pipefail
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```
All three must pass with zero warnings.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: drop devkit-testtmp for tempfile"
```
