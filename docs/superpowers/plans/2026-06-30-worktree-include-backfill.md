# Worktree local-file backfill — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Copy configured untracked local files from the monorepo primary clone into a newly created worktree at `issue setup` / `issue checkout-pr` time.

**Architecture:** A config-agnostic `copy_includes(source, dest, patterns)` helper in `devkit-common::worktree` glob-expands path patterns against the monorepo root and copies each match (files directly, directories recursively) into the worktree, skipping existing destinations and never aborting (fail-open). The two `issue` commands read `defaults.worktree_include` and call it right after `git worktree add`, before per-app prep.

**Tech Stack:** Rust (edition 2024), `glob` crate (new dependency), `anyhow`. Tests use the repo's `std::env::temp_dir()` convention (no `tempfile` crate).

**Spec:** `docs/superpowers/specs/2026-06-30-worktree-include-backfill-design.md`

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `Cargo.toml` (workspace) | modify | add `glob = "0.3"` to `[workspace.dependencies]` |
| `crates/devkit-common/Cargo.toml` | modify | add `glob.workspace = true` |
| `crates/devkit-common/src/worktree.rs` | modify | add `copy_includes` + recursive copy helper + tests |
| `crates/devkit-ports/src/config.rs` | modify | add `Defaults.worktree_include` field + test |
| `src/bin/issue/setup.rs` | modify | call `copy_includes` after worktree creation |
| `src/bin/issue/checkout.rs` | modify | call `copy_includes` after worktree creation (unconditional) |
| `docs/configuration.md` | modify | document the `worktree_include` key |

Tasks 3 and 4 are mechanical wiring of the Task-1 helper into existing command flows that perform real git/network operations and have no `run()`-level unit tests in the repo. They are verified by `cargo build` + `cargo clippy` + the existing test suite staying green, not by new unit tests — the copy logic itself is fully covered in Task 1. Forcing a brittle git-spawning integration test for a two-line call would be worse than this honest gap.

---

## Task 1: `copy_includes` helper in `devkit-common::worktree`

**Files:**
- Modify: `Cargo.toml` (workspace `[workspace.dependencies]`)
- Modify: `crates/devkit-common/Cargo.toml`
- Modify: `crates/devkit-common/src/worktree.rs`

- [ ] **Step 1: Add the `glob` dependency**

In the workspace root `Cargo.toml`, under `[workspace.dependencies]`, add (alphabetical-ish, near `fd-lock`):

```toml
glob = "0.3"
```

In `crates/devkit-common/Cargo.toml`, under `[dependencies]`, add:

```toml
glob.workspace = true
```

- [ ] **Step 2: Write the failing tests**

In `crates/devkit-common/src/worktree.rs`, add to the existing `#[cfg(test)] mod tests` block (after the `id_from_branch_then_dir` test). These tests use the repo's temp-dir convention — no `tempfile` crate.

```rust
    use std::fs;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "devkit-incl-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn copies_a_matching_file_preserving_relative_path() {
        let base = tmp("file");
        let src = base.join("src");
        let dst = base.join("dst");
        write(&src.join("apps/web/.env.local"), "SECRET=1");

        let (n, warnings) =
            copy_includes(&src, &dst, &["apps/*/.env.local".to_string()]);

        assert_eq!(n, 1);
        assert!(warnings.is_empty());
        assert_eq!(
            fs::read_to_string(dst.join("apps/web/.env.local")).unwrap(),
            "SECRET=1"
        );
    }

    #[test]
    fn double_star_matches_nested_file() {
        let base = tmp("nested");
        let src = base.join("src");
        let dst = base.join("dst");
        write(&src.join("a/b/c/.env.local"), "X=1");

        let (n, _) = copy_includes(&src, &dst, &["**/.env.local".to_string()]);

        assert_eq!(n, 1);
        assert!(dst.join("a/b/c/.env.local").exists());
    }

    #[test]
    fn directory_pattern_copies_recursively() {
        let base = tmp("dir");
        let src = base.join("src");
        let dst = base.join("dst");
        write(&src.join(".claude/hooks/pre.sh"), "echo pre");
        write(&src.join(".claude/hooks/sub/post.sh"), "echo post");

        // Trailing slash must behave like the bare directory.
        let (n, warnings) =
            copy_includes(&src, &dst, &[".claude/hooks/".to_string()]);

        assert_eq!(n, 2);
        assert!(warnings.is_empty());
        assert_eq!(
            fs::read_to_string(dst.join(".claude/hooks/pre.sh")).unwrap(),
            "echo pre"
        );
        assert_eq!(
            fs::read_to_string(dst.join(".claude/hooks/sub/post.sh")).unwrap(),
            "echo post"
        );
    }

    #[test]
    fn pattern_matching_nothing_is_silently_skipped() {
        let base = tmp("nomatch");
        let src = base.join("src");
        let dst = base.join("dst");
        fs::create_dir_all(&src).unwrap();

        let (n, warnings) =
            copy_includes(&src, &dst, &["does/not/exist".to_string()]);

        assert_eq!(n, 0);
        assert!(warnings.is_empty());
    }

    #[test]
    fn existing_destination_file_is_not_clobbered() {
        let base = tmp("noclobber");
        let src = base.join("src");
        let dst = base.join("dst");
        write(&src.join(".tool-versions"), "node 20");
        write(&dst.join(".tool-versions"), "KEEP ME");

        let (n, _) =
            copy_includes(&src, &dst, &[".tool-versions".to_string()]);

        assert_eq!(n, 0);
        assert_eq!(
            fs::read_to_string(dst.join(".tool-versions")).unwrap(),
            "KEEP ME"
        );
    }

    #[test]
    fn empty_patterns_is_a_no_op() {
        let base = tmp("empty");
        let src = base.join("src");
        let dst = base.join("dst");
        fs::create_dir_all(&src).unwrap();

        let (n, warnings) = copy_includes(&src, &dst, &[]);

        assert_eq!(n, 0);
        assert!(warnings.is_empty());
        assert!(!dst.exists());
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p devkit-common --lib -- worktree::tests`
Expected: compile error — `cannot find function copy_includes in this scope`.

- [ ] **Step 4: Implement `copy_includes` and the recursive helper**

In `crates/devkit-common/src/worktree.rs`, change the top imports from:

```rust
use crate::cmd::git;
use anyhow::Result;
use std::path::PathBuf;
```

to:

```rust
use crate::cmd::git;
use anyhow::Result;
use std::path::{Path, PathBuf};
```

Then add this above the `#[cfg(test)]` module:

```rust
/// Copy files matching `patterns` (path globs relative to `source`) into `dest`
/// at the same relative path. A match that is a directory is copied recursively.
/// Patterns that match nothing are silently skipped; a destination file that
/// already exists is left untouched (never clobbered). Fail-open: a glob or copy
/// error is collected as a warning string rather than propagated, so backfill
/// never aborts worktree creation. Returns (files_copied, warnings).
pub fn copy_includes(source: &Path, dest: &Path, patterns: &[String]) -> (usize, Vec<String>) {
    let opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        // Match dotfiles with wildcards, mirroring shell `dotglob`.
        require_literal_leading_dot: false,
    };
    let mut copied = 0usize;
    let mut warnings = Vec::new();

    for pattern in patterns {
        // A trailing slash signals a directory (gitignore idiom); strip it so the
        // glob matches the directory entry, then recurse because it is a dir.
        let trimmed = pattern.trim_end_matches('/');
        let joined = source.join(trimmed);
        let Some(pat_str) = joined.to_str() else {
            warnings.push(format!("include pattern is not valid UTF-8: {pattern}"));
            continue;
        };
        let entries = match glob::glob_with(pat_str, opts) {
            Ok(paths) => paths,
            Err(e) => {
                warnings.push(format!("bad include pattern `{pattern}`: {e}"));
                continue;
            }
        };
        for entry in entries {
            let matched = match entry {
                Ok(p) => p,
                Err(e) => {
                    warnings.push(format!("reading match for `{pattern}`: {e}"));
                    continue;
                }
            };
            let Ok(rel) = matched.strip_prefix(source) else {
                warnings.push(format!("match outside source: {}", matched.display()));
                continue;
            };
            let target = dest.join(rel);
            if matched.is_dir() {
                copy_dir(&matched, &target, &mut copied, &mut warnings);
            } else {
                copy_file(&matched, &target, &mut copied, &mut warnings);
            }
        }
    }
    (copied, warnings)
}

/// Copy a single file, skipping if the destination already exists. Errors are
/// pushed as warnings.
fn copy_file(src: &Path, dst: &Path, copied: &mut usize, warnings: &mut Vec<String>) {
    if dst.exists() {
        return;
    }
    if let Some(parent) = dst.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        warnings.push(format!("creating {}: {e}", parent.display()));
        return;
    }
    match std::fs::copy(src, dst) {
        Ok(_) => *copied += 1,
        Err(e) => warnings.push(format!("copying {} -> {}: {e}", src.display(), dst.display())),
    }
}

/// Recursively copy a directory's files, skipping existing destinations.
fn copy_dir(src: &Path, dst: &Path, copied: &mut usize, warnings: &mut Vec<String>) {
    let entries = match std::fs::read_dir(src) {
        Ok(e) => e,
        Err(e) => {
            warnings.push(format!("reading dir {}: {e}", src.display()));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warnings.push(format!("reading entry in {}: {e}", src.display()));
                continue;
            }
        };
        let child = entry.path();
        let target = dst.join(entry.file_name());
        if child.is_dir() {
            copy_dir(&child, &target, copied, warnings);
        } else {
            copy_file(&child, &target, copied, warnings);
        }
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p devkit-common --lib -- worktree::tests`
Expected: all `copy_includes` tests PASS (plus the pre-existing `parses_two_worktrees`, `id_from_branch_then_dir`).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/devkit-common/Cargo.toml crates/devkit-common/src/worktree.rs Cargo.lock
git commit -m "feat(worktree): add copy_includes backfill helper"
```

---

## Task 2: `worktree_include` config field

**Files:**
- Modify: `crates/devkit-ports/src/config.rs`

- [ ] **Step 1: Write the failing test**

In `crates/devkit-ports/src/config.rs`, inside the `#[cfg(test)] mod tests` block, add:

```rust
    #[test]
    fn worktree_include_parses_and_defaults_empty() {
        let cfg: Config = toml::from_str(
            r#"
            [defaults]
            worktree_root = "/w"
            branch_prefix = "you/"
            baseline_ref = "origin/staging"
            baseline_path = "/b"
            worktree_include = ["apps/*/.env.local", ".tool-versions"]
            "#,
        )
        .unwrap();
        assert_eq!(
            cfg.defaults.worktree_include,
            vec!["apps/*/.env.local".to_string(), ".tool-versions".to_string()]
        );

        let bare: Config = toml::from_str(
            r#"
            [defaults]
            worktree_root = "/w"
            branch_prefix = "you/"
            baseline_ref = "origin/staging"
            baseline_path = "/b"
            "#,
        )
        .unwrap();
        assert!(bare.defaults.worktree_include.is_empty());
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p devkit-ports --lib -- config::tests::worktree_include_parses_and_defaults_empty`
Expected: compile error — `no field worktree_include on type Defaults`.

- [ ] **Step 3: Add the field**

In `crates/devkit-ports/src/config.rs`, in `struct Defaults`, add after the `stray_scan_width` field (keep the existing `#[serde(default = "default_stray_scan_width")] pub stray_scan_width: u16,`):

```rust
    /// Glob patterns (relative to the monorepo root) for untracked local files to
    /// copy into a newly created worktree. Each match is copied to the same
    /// relative path; a match that is a directory (or a pattern ending in `/`) is
    /// copied recursively. Existing destinations are never clobbered. Empty by
    /// default — the backfill is opt-in.
    #[serde(default)]
    pub worktree_include: Vec<String>,
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p devkit-ports --lib -- config::tests::worktree_include_parses_and_defaults_empty`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/config.rs
git commit -m "feat(config): add defaults.worktree_include"
```

---

## Task 3: Wire backfill into `issue setup`

**Files:**
- Modify: `src/bin/issue/setup.rs`

No new unit test (see File Structure note). Verified by build + clippy + existing suite.

- [ ] **Step 1: Add the backfill call**

In `src/bin/issue/setup.rs`, the worktree is created at the `steps.during("Creating worktree…", …)` block and `prep_apps` runs later. Insert the backfill **after** `crate::record::write(...)` completes and **before** the `prep_apps` block. Locate this existing code:

```rust
    crate::record::write(
        &worktree,
        &crate::record::IssueRecord {
            issue: args.issue.clone(),
            slug: args.slug.clone(),
            apps: args.apps.clone(),
        },
    )?;
    if !args.no_gitignore
        && let Err(e) = crate::gitignore::ensure_devkit_ignored()
    {
        eprintln!("warning: could not update global gitignore: {e:#}");
    }
```

Immediately **after** that `if !args.no_gitignore { … }` block, insert:

```rust
    backfill_includes(monorepo_s, &worktree, &cfg.defaults.worktree_include);
```

- [ ] **Step 2: Add the shared helper**

`setup.rs` is also used by `checkout.rs` (it calls `crate::setup::prep_apps`), so put the small wiring helper here and reuse it from checkout. Add this free function to `src/bin/issue/setup.rs` (top-level, near `prep_apps`):

```rust
/// Copy the configured `worktree_include` globs from the monorepo into a freshly
/// created worktree, printing each fail-open warning to stderr. A no-op when the
/// include list is empty.
pub fn backfill_includes(monorepo: &str, worktree: &std::path::Path, patterns: &[String]) {
    if patterns.is_empty() {
        return;
    }
    let (_copied, warnings) =
        devkit_common::worktree::copy_includes(std::path::Path::new(monorepo), worktree, patterns);
    for w in warnings {
        eprintln!("warning: {w}");
    }
}
```

- [ ] **Step 3: Build and lint**

Run: `cargo build -p devkit && cargo clippy -p devkit --all-targets -- -D warnings`
Expected: builds clean, zero warnings.

- [ ] **Step 4: Commit**

```bash
git add src/bin/issue/setup.rs
git commit -m "feat(issue): backfill local files on setup"
```

---

## Task 4: Wire backfill into `issue checkout-pr` (unconditional)

**Files:**
- Modify: `src/bin/issue/checkout.rs`

No new unit test (see File Structure note). Verified by build + clippy + existing suite.

- [ ] **Step 1: Add the backfill call**

In `src/bin/issue/checkout.rs`, the worktree + record are created inside the `with_cleanup(...)` block which returns `issue`. The app-prep runs later inside `if args.setup { … }`. Insert the backfill **after** the `with_cleanup` block returns and **before** `if args.setup`, so it runs on every checkout regardless of `--setup`. Locate:

```rust
        Ok(issue)
    })?;

    if args.setup {
```

Insert between `})?;` and `if args.setup {`:

```rust
    crate::setup::backfill_includes(monorepo_s, &worktree, &cfg.defaults.worktree_include);

```

Result:

```rust
        Ok(issue)
    })?;

    crate::setup::backfill_includes(monorepo_s, &worktree, &cfg.defaults.worktree_include);

    if args.setup {
```

- [ ] **Step 2: Build and lint**

Run: `cargo build -p devkit && cargo clippy -p devkit --all-targets -- -D warnings`
Expected: builds clean, zero warnings. (`monorepo_s` and `worktree` are already in scope from earlier in `run`.)

- [ ] **Step 3: Commit**

```bash
git add src/bin/issue/checkout.rs
git commit -m "feat(issue): backfill local files on checkout-pr"
```

---

## Task 5: Document the config key and run the full gate

**Files:**
- Modify: `docs/configuration.md`

- [ ] **Step 1: Document `worktree_include`**

In `docs/configuration.md`, in the `[defaults]` table, add a row immediately after the `apps_dir` row (currently `| \`apps_dir\` | no | … |`):

```markdown
| `worktree_include` | no | Glob patterns (relative to the monorepo root) for untracked local files copied into a newly created worktree by `issue setup` / `issue checkout-pr`, at the same relative path. A pattern ending in `/`, or one matching a directory, copies recursively. Existing destinations are never overwritten; copy failures warn and are skipped (fail-open). Anchor patterns (`apps/*/.env.local`) rather than scanning the whole tree — `**` descends into `node_modules`. |
```

- [ ] **Step 2: Run the full gate**

Run:
```bash
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```
Expected: tests all green (no FAILED/error lines), clippy exit 0, `fmt --check` exit 0.

- [ ] **Step 3: Commit**

```bash
git add docs/configuration.md
git commit -m "docs: document defaults.worktree_include"
```

---

## Self-Review

**Spec coverage:**
- Config `defaults.worktree_include` → Task 2 + docs Task 5. ✓
- `copy_includes` in `devkit-common::worktree`, decoupled signature → Task 1. ✓
- Glob expansion, file + recursive-directory copy, trailing-slash idiom → Task 1 (impl + 3 tests). ✓
- Skip-existing per file → Task 1 (`existing_destination_file_is_not_clobbered`). ✓
- Fail-open warnings → Task 1 (errors collected, never propagated) + wiring prints to stderr. ✓
- Insertion before `prep_apps` in setup → Task 3; unconditional in checkout-pr → Task 4. ✓
- `glob` crate, no parallelism → Task 1 Step 1. ✓
- Tests over temp dirs, full gate green → Task 1 + Task 5. ✓

**Placeholder scan:** No TBD/TODO; every code step shows full code. ✓

**Type consistency:** `copy_includes(&Path, &Path, &[String]) -> (usize, Vec<String>)` used identically in Task 1 (def), Task 3 (`backfill_includes` wrapper), Task 4 (reuse). `backfill_includes(&str, &Path, &[String])` defined in Task 3, called in Tasks 3 and 4. ✓
