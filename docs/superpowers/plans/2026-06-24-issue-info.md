# `issue info` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a read-only `issue info` subcommand that reports one worktree's PR number and Linear id, with a `--json` machine output and a `--cache-only` offline mode backed by a per-worktree PR cache.

**Architecture:** A new `gather_local` in the `devkit-issue` facade builds a status report with no network. The `issue` binary gets three small modules — `select` (worktree-matching predicate, shared with `end`), `info_cache` (per-worktree `.devkit/pr.json`), and `info` (the subcommand) — plus wiring in `main.rs`. Live runs use the existing `gather` and write the PR through to the cache; `--cache-only` uses `gather_local` and overlays the cached PR. The cache is cleaned structurally: `.devkit/` is gitignored, so `git worktree remove` (what `issue end` runs) deletes it.

**Tech Stack:** Rust (edition 2024), `clap` derive subcommands, `serde`/`serde_json`, `anyhow`. Spec: `docs/superpowers/specs/2026-06-24-issue-info-design.md`.

**Commit convention:** Conventional Commits; imperative subject ≤50 chars, lowercase, no trailing period. End every commit message with:
`Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`

---

## File structure

- Create: `src/bin/issue/select.rs` — `matches(row, sel)` worktree-matching predicate.
- Modify: `src/bin/issue/end.rs` — use `select::matches` instead of the inline predicate.
- Create: `src/bin/issue/info_cache.rs` — `CachedPr` + atomic `read`/`write` of `<worktree>/.devkit/pr.json`.
- Create: `src/bin/issue/info.rs` — `pick_index` selection + `run` (live/cache-only, render/json).
- Modify: `src/bin/issue/main.rs` — declare modules, add the `Info` subcommand and its match arm, update the `about` string.
- Modify: `crates/devkit-issue/src/status.rs` — add `gather_local`.
- Create: `crates/devkit-issue/tests/gather_local.rs` — integration test for `gather_local`.
- Modify: `README.md` (and any `docs/` CLI reference) — document `issue info`.

---

## Task 1: Shared worktree-matching predicate (`select::matches`)

`end.rs` has an inline predicate that matches a selector against a worktree's
issue id, branch, basename, or full path. `info` needs the same logic, so
extract the per-row predicate into a shared module and have `end` call it.

**Files:**
- Create: `src/bin/issue/select.rs`
- Modify: `src/bin/issue/main.rs` (add `mod select;`)
- Modify: `src/bin/issue/end.rs:13-30` (call `select::matches`)

- [ ] **Step 1: Write the failing test**

Create `src/bin/issue/select.rs`:

```rust
use devkit_issue::status::IssueWorktree;
use std::path::Path;

/// True when `sel` names this worktree by issue id, branch, worktree basename,
/// or full path (all compared case-insensitively).
pub fn matches(row: &IssueWorktree, sel: &str) -> bool {
    let s = sel.to_lowercase();
    let base = Path::new(&row.worktree)
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_lowercase();
    [
        row.issue_id.to_lowercase(),
        row.branch.to_lowercase(),
        base,
        row.worktree.to_lowercase(),
    ]
    .contains(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> IssueWorktree {
        IssueWorktree {
            worktree: "/home/u/wt/eng-7-fix".into(),
            branch: "lev/eng-7-fix".into(),
            issue_id: "ENG-7".into(),
            dirty: false,
            pr_number: None,
            pr_state: "NO_PR".into(),
            pr_url: None,
            linear_kind: None,
            linear_name: None,
            finished: false,
            reason_not_finished: None,
        }
    }

    #[test]
    fn matches_by_id_branch_basename_and_path() {
        let r = row();
        assert!(matches(&r, "eng-7"));
        assert!(matches(&r, "ENG-7"));
        assert!(matches(&r, "lev/eng-7-fix"));
        assert!(matches(&r, "eng-7-fix"));
        assert!(matches(&r, "/home/u/wt/eng-7-fix"));
    }

    #[test]
    fn rejects_non_match() {
        assert!(!matches(&row(), "eng-8"));
    }
}
```

Add `mod select;` to `src/bin/issue/main.rs` alongside the other `mod` lines (after `mod review;`):

```rust
mod review;
mod select;
mod setup;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit --bin issue select::`
Expected: FAIL to compile until `mod select;` is added, then PASS (the module is self-contained). If it already passes, that is fine — proceed.

- [ ] **Step 3: Refactor `end.rs` to use the shared predicate**

In `src/bin/issue/end.rs`, replace the inline predicate inside `select_explicit` (the closure body at lines 16-29) so the filter calls `crate::select::matches`:

```rust
        let hits: Vec<&IssueWorktree> = rows
            .iter()
            .filter(|r| crate::select::matches(r, sel))
            .collect();
```

Remove the now-unused `Path` import in `end.rs` only if nothing else uses it
(it is still used by `cleanup`, so leave `use std::path::Path;` in place).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit --bin issue`
Expected: PASS (select tests pass; end behavior unchanged).

- [ ] **Step 5: Commit**

```bash
git add src/bin/issue/select.rs src/bin/issue/main.rs src/bin/issue/end.rs
git commit -m "refactor(issue): extract shared worktree-match predicate"
```

---

## Task 2: Per-worktree PR cache (`info_cache`)

A tiny atomic JSON cache at `<worktree>/.devkit/pr.json`, holding one immutable
PR. Read/write are best-effort; a miss or parse error is never fatal.

**Files:**
- Create: `src/bin/issue/info_cache.rs`
- Modify: `src/bin/issue/main.rs` (add `mod info_cache;`)

- [ ] **Step 1: Write the failing test**

Create `src/bin/issue/info_cache.rs`:

```rust
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The cached PR for a worktree: written after a live `issue info`, read by
/// `issue info --cache-only`. A PR number is immutable once assigned, so this
/// needs no TTL — a live run overwrites it and the cache self-heals.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedPr {
    pub number: u64,
    pub state: String,
    pub url: String,
}

/// `<worktree>/.devkit/pr.json`.
fn path(worktree: &Path) -> PathBuf {
    worktree.join(".devkit").join("pr.json")
}

/// Read the cached PR, or `None` if the file is absent or unparseable.
pub fn read(worktree: &Path) -> Option<CachedPr> {
    let body = std::fs::read_to_string(path(worktree)).ok()?;
    serde_json::from_str(&body).ok()
}

/// Write the PR cache atomically (temp file + rename) under `<worktree>/.devkit/`,
/// creating the directory. Best-effort: callers may ignore the error since a
/// cache miss is never fatal.
pub fn write(worktree: &Path, pr: &CachedPr) -> Result<()> {
    let p = path(worktree);
    let dir = p.parent().expect("pr.json path has a parent");
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("pr.json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(pr)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("devkit-info-cache-{}-{}", std::process::id(), tag))
    }

    #[test]
    fn write_then_read_round_trips() {
        let wt = scratch("rt");
        let _ = std::fs::remove_dir_all(&wt);
        std::fs::create_dir_all(&wt).unwrap();
        let pr = CachedPr { number: 123, state: "OPEN".into(), url: "https://x/pr/123".into() };
        write(&wt, &pr).unwrap();
        assert_eq!(read(&wt), Some(pr));
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn read_missing_is_none() {
        let wt = scratch("missing");
        let _ = std::fs::remove_dir_all(&wt);
        assert_eq!(read(&wt), None);
    }

    #[test]
    fn read_corrupt_is_none() {
        let wt = scratch("corrupt");
        let _ = std::fs::remove_dir_all(&wt);
        std::fs::create_dir_all(wt.join(".devkit")).unwrap();
        std::fs::write(wt.join(".devkit").join("pr.json"), b"not json").unwrap();
        assert_eq!(read(&wt), None);
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn write_leaves_no_temp_file() {
        let wt = scratch("notmp");
        let _ = std::fs::remove_dir_all(&wt);
        std::fs::create_dir_all(&wt).unwrap();
        write(&wt, &CachedPr { number: 1, state: "MERGED".into(), url: "u".into() }).unwrap();
        let leftover: Vec<_> = std::fs::read_dir(wt.join(".devkit"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftover.is_empty(), "temp file left behind: {leftover:?}");
        let _ = std::fs::remove_dir_all(&wt);
    }
}
```

Add `mod info_cache;` to `src/bin/issue/main.rs` (keep modules alphabetical near the others):

```rust
mod info_cache;
```

- [ ] **Step 2: Run test to verify it fails, then passes**

Run: `cargo test -p devkit --bin issue info_cache::`
Expected: compiles once `mod info_cache;` is present, then PASS.

- [ ] **Step 3: Commit**

```bash
git add src/bin/issue/info_cache.rs src/bin/issue/main.rs
git commit -m "feat(issue): add per-worktree pr cache"
```

---

## Task 3: Network-free status (`gather_local`)

Add a sibling to `gather` that does discovery + dirty checks but no `gh`/Linear
calls, so `--cache-only` has a report to overlay onto.

**Files:**
- Modify: `crates/devkit-issue/src/status.rs` (add `gather_local` after `gather`, ~line 271)
- Create: `crates/devkit-issue/tests/gather_local.rs`

- [ ] **Step 1: Write the failing integration test**

Create `crates/devkit-issue/tests/gather_local.rs`:

```rust
use std::path::Path;
use std::process::Command;

fn git(args: &[&str], cwd: &Path) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("git runs")
        .success();
    assert!(ok, "git {args:?} failed");
}

#[test]
fn gather_local_returns_offline_rows_without_network() {
    let base = std::env::temp_dir().join(format!("devkit-gl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let main = base.join("main");
    std::fs::create_dir_all(&main).unwrap();

    git(&["init", "-q", "-b", "main"], &main);
    git(&["config", "user.email", "t@t"], &main);
    git(&["config", "user.name", "t"], &main);
    std::fs::write(main.join("f"), "x").unwrap();
    git(&["add", "."], &main);
    git(&["commit", "-qm", "init"], &main);

    let wt = base.join("eng-1-foo");
    git(
        &["worktree", "add", "-q", "-b", "lev/eng-1-bar", wt.to_str().unwrap()],
        &main,
    );

    let report = devkit_issue::status::gather_local(main.to_str().unwrap(), &[]).unwrap();
    let row = report
        .worktrees
        .iter()
        .find(|r| r.issue_id == "ENG-1")
        .expect("eng-1 row present");
    assert_eq!(row.pr_state, "NO_PR");
    assert_eq!(row.pr_number, None);
    assert_eq!(row.linear_kind, None);
    assert!(!row.dirty);

    let _ = std::fs::remove_dir_all(&base);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-issue --test gather_local`
Expected: FAIL to compile — `gather_local` does not exist yet.

- [ ] **Step 3: Implement `gather_local`**

In `crates/devkit-issue/src/status.rs`, add this function immediately after
`gather` (after line 271, before `#[cfg(test)]`):

```rust
/// Local-only status: discovery + dirty checks, with no `gh`/Linear network.
/// PRs stay `NO_PR` and Linear stays unknown; callers (e.g. `issue info
/// --cache-only`) overlay cached data themselves. Same signature shape as
/// `gather`.
pub fn gather_local(start: &str, ids: &[String]) -> Result<StatusReport> {
    let d = discover(start, ids)?;
    let has_key = devkit_common::secrets::resolve("LINEAR_API_KEY").is_some();
    let dirty: Vec<bool> = d.worktree_paths().iter().map(|p| dirty_of(p)).collect();
    Ok(assemble(d, dirty, Prs(Vec::new()), HashMap::new(), None, has_key))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p devkit-issue --test gather_local`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-issue/src/status.rs crates/devkit-issue/tests/gather_local.rs
git commit -m "feat(issue): add network-free gather_local"
```

---

## Task 4: The `issue info` subcommand

Wire it all together: select the target worktree (cwd or selector), build the
report live or from cache, overlay/write the cache, then render or emit JSON.

**Files:**
- Create: `src/bin/issue/info.rs`
- Modify: `src/bin/issue/main.rs` (add `mod info;`, the `Info` variant, its match arm, and the `about` string)

- [ ] **Step 1: Write the failing test (pure selection logic)**

Create `src/bin/issue/info.rs`:

```rust
use crate::triage::render;
use anyhow::Result;
use devkit_common::cmd::git;
use devkit_issue::status::{self as st, IssueWorktree, StatusReport};
use std::path::Path;

/// Index of the worktree the command targets: the one matching `selector`, or —
/// when `selector` is `None` — the one whose path equals `current_top`.
fn pick_index(
    rows: &[IssueWorktree],
    selector: Option<&str>,
    current_top: Option<&str>,
) -> Option<usize> {
    match selector {
        Some(sel) => rows.iter().position(|r| crate::select::matches(r, sel)),
        None => {
            let top = current_top?;
            rows.iter().position(|r| same_path(&r.worktree, top))
        }
    }
}

/// Path equality that tolerates symlinks/normalization by canonicalizing both
/// sides; falls back to a string compare when a path cannot be canonicalized.
fn same_path(a: &str, b: &str) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

/// The current worktree's root (`git rev-parse --show-toplevel`), trimmed.
fn current_top(start: &str) -> Option<String> {
    git(&["rev-parse", "--show-toplevel"], start)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn run(start: &str, selector: Option<&str>, json: bool, cache_only: bool) -> Result<()> {
    let report = if cache_only {
        st::gather_local(start, &[])?
    } else {
        st::gather(start, &[])?
    };

    let top = current_top(start);
    let Some(i) = pick_index(&report.worktrees, selector, top.as_deref()) else {
        match selector {
            Some(sel) => anyhow::bail!("no worktree matches '{sel}'"),
            None => anyhow::bail!("not in an issue worktree"),
        }
    };

    let mut row = report.worktrees[i].clone();

    if cache_only {
        if let Some(pr) = crate::info_cache::read(Path::new(&row.worktree)) {
            row.pr_number = Some(pr.number);
            row.pr_state = pr.state;
            row.pr_url = Some(pr.url);
        }
    } else if let (Some(number), Some(url)) = (row.pr_number, row.pr_url.clone()) {
        let _ = crate::info_cache::write(
            Path::new(&row.worktree),
            &crate::info_cache::CachedPr { number, state: row.pr_state.clone(), url },
        );
    }

    if json {
        println!("{}", serde_json::to_string(&row)?);
    } else {
        let one = StatusReport {
            finished_count: usize::from(row.finished),
            has_linear_key: report.has_linear_key,
            linear_workspace: report.linear_workspace.clone(),
            worktrees: vec![row],
        };
        render(&one);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(worktree: &str, branch: &str, id: &str) -> IssueWorktree {
        IssueWorktree {
            worktree: worktree.into(),
            branch: branch.into(),
            issue_id: id.into(),
            dirty: false,
            pr_number: None,
            pr_state: "NO_PR".into(),
            pr_url: None,
            linear_kind: None,
            linear_name: None,
            finished: false,
            reason_not_finished: None,
        }
    }

    #[test]
    fn selector_picks_by_id() {
        let rows = vec![row("/a", "lev/eng-1-x", "ENG-1"), row("/b", "lev/eng-2-y", "ENG-2")];
        assert_eq!(pick_index(&rows, Some("eng-2"), None), Some(1));
    }

    #[test]
    fn no_selector_picks_current_top() {
        let rows = vec![row("/a", "lev/eng-1-x", "ENG-1"), row("/b", "lev/eng-2-y", "ENG-2")];
        assert_eq!(pick_index(&rows, None, Some("/b")), Some(1));
    }

    #[test]
    fn no_match_is_none() {
        let rows = vec![row("/a", "lev/eng-1-x", "ENG-1")];
        assert_eq!(pick_index(&rows, Some("eng-9"), None), None);
        assert_eq!(pick_index(&rows, None, Some("/elsewhere")), None);
        assert_eq!(pick_index(&rows, None, None), None);
    }
}
```

- [ ] **Step 2: Wire the subcommand into `main.rs`**

In `src/bin/issue/main.rs`:

Add the module declaration (with the others):
```rust
mod info;
```

Update the `about` string (line 19) to include `info`:
```rust
    about = "Issue lifecycle: setup, status, info, end, prs, dashboard, review"
```

Add the `Info` variant to `enum Cmd` (place it right after `Status`):
```rust
    /// Show one worktree's PR + Linear id (current worktree, or a SELECTOR).
    Info {
        /// Issue id, branch, worktree basename, or path. Defaults to cwd.
        selector: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long = "cache-only")]
        cache_only: bool,
    },
```

Add the match arm in `main()` (after the `Status` arm):
```rust
        Some(Cmd::Info { selector, json, cache_only }) => {
            info::run(&start(&cli.dir), selector.as_deref(), json, cache_only)
        }
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p devkit --bin issue info::`
Expected: PASS (the three `pick_index` tests).

- [ ] **Step 4: Manual smoke test**

Run (from inside this worktree):
```bash
cargo run -q --bin issue -- info --cache-only
cargo run -q --bin issue -- info --cache-only --json
```
Expected: the `--json` form prints one JSON object with `"issue_id":"ISSUE-INFO"`-style fields (or whatever this worktree's branch yields); `pr_number` is `null` until a live run populates the cache. A live `cargo run -q --bin issue -- info` makes one `gh` call and writes `.devkit/pr.json`.

- [ ] **Step 5: Commit**

```bash
git add src/bin/issue/info.rs src/bin/issue/main.rs
git commit -m "feat(issue): add info subcommand"
```

---

## Task 5: Documentation + full gate

**Files:**
- Modify: `README.md` and any `docs/` file documenting `issue` subcommands

- [ ] **Step 1: Find where `issue` subcommands are documented**

Run: `rg -n "issue status|issue dashboard|issue prs" README.md docs/`
Expected: one or more sections listing the `issue` subcommands.

- [ ] **Step 2: Add `issue info` documentation**

Next to the `issue status` entry, add an `issue info` entry describing:
- `issue info [SELECTOR]` — show one worktree's PR number + Linear id (defaults to the current worktree).
- `--json` — emit a single machine-readable object (the `IssueWorktree` struct).
- `--cache-only` — no network; PR from the per-worktree cache, Linear shown as `—`.
- Note the per-worktree cache at `<worktree>/.devkit/pr.json`, auto-removed with the worktree.

Match the surrounding formatting exactly (table row, bullet, or prose as used).

- [ ] **Step 3: Run the full merge gate**

Run:
```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: formatting clean, zero clippy warnings, all tests pass (the prior 327 plus the new ones).

- [ ] **Step 4: Commit**

```bash
git add README.md docs/
git commit -m "docs(issue): document info subcommand"
```

---

## Self-review notes (already applied)

- **Spec coverage:** CLI surface → Task 4; readable summary + `--json` → Task 4 (`render` / `serde_json`); `--cache-only` + per-worktree cache → Tasks 2 & 4; cleanup-on-removal → structural (no code, verified `.devkit/` is gitignored); `gather_local` facade change → Task 3; selector (cwd + id/branch/path) → Tasks 1 & 4; `--no-fetch` dropped → not implemented, by design. All covered.
- **Type consistency:** `CachedPr { number, state, url }`, `gather_local(start, ids)`, `pick_index(rows, selector, current_top)`, `select::matches(row, sel)` are used identically wherever referenced.
- **No placeholders:** every code and command step is concrete.
