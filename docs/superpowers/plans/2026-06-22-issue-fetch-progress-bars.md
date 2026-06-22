# Step Progress Bars + Parallel Issue Fetches — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the `issue` CLI's single spinner with numbered step bars that run independent fetches concurrently, collapse the `prs` command's four GitHub round-trips into one GraphQL request, and clear the bars before printing the table/dashboard.

**Architecture:** All bar orchestration lives in the `src/bin/issue` CLI via a new `Steps` (`MultiProgress`) helper. The library crates (`devkit-issue`, `devkit-common`) stay pure and rendering-free; they gain (a) granular `status` pieces the CLI drives, (b) internal parallelism inside the silent `gather` entry points, (c) a single-request GraphQL PR fetch, and (d) an optional per-page progress callback on Linear history.

**Tech Stack:** Rust (edition 2024), `indicatif` 0.17, `anyhow`, `serde`/`serde_json`, `ureq` (Linear), `gh`/`git` subprocesses, `std::thread::scope`.

## Global Constraints

- `cargo test --workspace` is the merge gate and must stay green (128 tests today).
- `cargo clippy --workspace --all-targets -- -D warnings` must stay clean (zero-warning policy).
- `cargo fmt --all` before every commit; CI uses the stable toolchain.
- Commits follow Conventional Commits; commit per task.
- `indicatif` must NOT be added to `devkit-issue` or `devkit-common` — bars live only in the `devkit` binary package (`src/bin/issue`). `indicatif` is already a dep of the root package (`Cargo.toml:30`).
- Bars are hidden when stderr is not a TTY (pipes, redirects, MCP, `cargo test`) — result tables print to stdout unchanged.
- `gather` (status) and `prs::gather` keep their **exact current signatures** so `devkit-mcp` and existing tests keep compiling.
- No `_ => Issue`-style catch-alls; map enums exhaustively (per AGENTS.md).
- Worktree: all work happens in `../devkit-worktrees/fetch-progress-bars` (branch `fetch-progress-bars`); the primary clone stays on `main`.

---

## File Structure

- **Modify** `src/bin/issue/spin.rs` — add the `Steps` `MultiProgress` helper next to `spinner()`.
- **Modify** `crates/devkit-issue/src/status.rs` — split `build_rows`/`gather` into `discover` / `dirty_of` / `fetch_prs` / `assemble`; rewire `gather` to run them concurrently.
- **Modify** `src/bin/issue/status.rs` — add `gather_with_bars`; `run` uses it.
- **Modify** `crates/devkit-issue/src/prs.rs` — replace the four `gh` round-trips with one `gh api graphql` request; reshape JSON structs; `checks_of` consumes the rollup `state`.
- **Modify** `src/bin/issue/prs.rs` — drive 2 concurrent bars (Linear workspace ‖ GitHub).
- **Modify** `crates/devkit-common/src/linear.rs` — add `assigned_issue_history_with_progress` (per-page callback); keep `assigned_issue_history` as a no-op-callback wrapper.
- **Modify** `src/bin/issue/dashboard/data.rs` — thread the page callback into `issues`.
- **Modify** `src/bin/issue/dashboard/mod.rs` — status bars via `gather_with_bars`, rising-count issue history, parallel PR/commit bars.

---

## Task 1: `Steps` bar helper

**Files:**
- Modify: `src/bin/issue/spin.rs`

**Interfaces:**
- Produces:
  - `pub struct Steps` with `pub fn new() -> Steps`
  - `pub fn spinner(&self, msg: &str) -> indicatif::ProgressBar` — animated spinner bar (steady tick), style `{spinner:.cyan} {wide_msg}`. Caller embeds any `[n/N]` prefix in `msg`.
  - `pub fn bar(&self, msg: &str, len: u64) -> indicatif::ProgressBar` — determinate bar, style `{spinner:.cyan} {wide_msg} [{bar:20.cyan/dim}] {pos}/{len}`.
  - `pub fn clear(&self)` — clears all bars.
- All bars hidden when stderr is not a TTY.

- [ ] **Step 1: Write the failing test**

Add to the bottom of `src/bin/issue/spin.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Tests never run under a TTY, so every bar the helper hands out must be
    // hidden — guaranteeing pipes / MCP / CI print no progress noise.
    #[test]
    fn steps_bars_hidden_off_tty() {
        let steps = Steps::new();
        assert!(steps.spinner("working…").is_hidden());
        assert!(steps.bar("counting…", 10).is_hidden());
        steps.clear();
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit --bin issue spin::tests 2>&1 | tail -20`
Expected: FAIL — `cannot find struct Steps` / `Steps::new` not found.

- [ ] **Step 3: Write minimal implementation**

Add above the `#[cfg(test)]` block in `src/bin/issue/spin.rs` (keep the existing `spinner` free function as-is). Update the top `use` line to include the extra imports:

```rust
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::io::IsTerminal;
use std::time::Duration;

/// A group of progress bars sharing one [`MultiProgress`]. Each bar animates on
/// stderr; the whole group is hidden when stderr is not a terminal, so pipes,
/// redirects, MCP, and tests produce no progress output.
///
/// Numbering is the caller's job: embed any `[2/4]`-style prefix in the message.
/// Call [`Steps::clear`] once all work is done, before printing results.
pub struct Steps {
    mp: MultiProgress,
}

impl Steps {
    pub fn new() -> Steps {
        let mp = if std::io::stderr().is_terminal() {
            MultiProgress::new()
        } else {
            MultiProgress::with_draw_target(ProgressDrawTarget::hidden())
        };
        Steps { mp }
    }

    /// An indeterminate spinner bar for a single opaque/batched fetch.
    pub fn spinner(&self, msg: &str) -> ProgressBar {
        let pb = self.mp.add(ProgressBar::new_spinner());
        pb.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {wide_msg}")
                .expect("valid spinner template"),
        );
        pb.enable_steady_tick(Duration::from_millis(80));
        pb.set_message(msg.to_string());
        pb
    }

    /// A determinate fill bar for a loop over a known count (`len`).
    pub fn bar(&self, msg: &str, len: u64) -> ProgressBar {
        let pb = self.mp.add(ProgressBar::new(len));
        pb.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {wide_msg} [{bar:20.cyan/dim}] {pos}/{len}")
                .expect("valid bar template")
                .progress_chars("=>-"),
        );
        pb.enable_steady_tick(Duration::from_millis(80));
        pb.set_message(msg.to_string());
        pb
    }

    /// Clear every bar in the group (call once all work is done).
    pub fn clear(&self) {
        let _ = self.mp.clear();
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p devkit --bin issue spin::tests 2>&1 | tail -20`
Expected: PASS (1 test).

- [ ] **Step 5: Commit**

```bash
git add src/bin/issue/spin.rs
git commit -m "feat(issue): add multi-bar Steps progress helper"
```

---

## Task 2: Split `status` library into composable pieces

**Files:**
- Modify: `crates/devkit-issue/src/status.rs`

**Interfaces:**
- Consumes: `devkit_common::worktree::discover`, `devkit_common::cmd::{gh_json, git}`, `devkit_common::linear::{states, workspace_url_key, LinearState}`.
- Produces (all `pub` in `devkit_issue::status`):
  - `struct Discovered { /* private fields */ }` with `pub fn is_empty(&self) -> bool`, `pub fn len(&self) -> usize`, `pub fn worktree_paths(&self) -> Vec<String>`, `pub fn issue_ids(&self) -> &[String]`.
  - `pub fn discover(start: &str, ids: &[String]) -> Result<Discovered>`
  - `pub fn dirty_of(path: &str) -> bool`
  - `struct Prs(/* private */)` — opaque PR list.
  - `pub fn fetch_prs(d: &Discovered) -> Result<Prs>`
  - `pub fn assemble(d: Discovered, dirty: Vec<bool>, prs: Prs, linear: std::collections::HashMap<String, LinearState>, linear_workspace: Option<String>, has_key: bool) -> StatusReport`
  - `pub fn gather(start: &str, ids: &[String]) -> Result<StatusReport>` (unchanged signature).

- [ ] **Step 1: Write the failing test**

Add these tests inside the existing `#[cfg(test)] mod tests` in `crates/devkit-issue/src/status.rs` (the `wt`/`pr` helpers already exist there):

```rust
    use std::collections::HashMap;

    // assemble zips dirty flags onto rows in order, attaches the best PR by
    // branch, applies Linear state, and computes the finished verdict — the same
    // result the old monolithic gather produced.
    #[test]
    fn assemble_attaches_pr_dirty_and_verdict() {
        let rows = vec![
            IssueWorktree {
                worktree: "/w1".into(),
                branch: "lev/eng-1-foo".into(),
                issue_id: "ENG-1".into(),
                dirty: false,
                pr_number: None,
                pr_state: "NO_PR".into(),
                pr_url: None,
                linear_kind: None,
                linear_name: None,
                finished: false,
                reason_not_finished: None,
            },
        ];
        let d = Discovered::for_test(rows, "/main".into(), vec!["ENG-1".into()]);
        let prs = Prs::for_test(vec![pr(7, "MERGED", "lev/eng-1-foo")]);
        let mut linear = HashMap::new();
        linear.insert(
            "ENG-1".to_string(),
            LinearState { kind: "completed".into(), name: "Done".into() },
        );
        let report = assemble(d, vec![false], prs, linear, Some("acme".into()), true);
        let row = &report.worktrees[0];
        assert_eq!(row.pr_number, Some(7));
        assert_eq!(row.pr_state, "MERGED");
        assert!(!row.dirty);
        assert!(row.finished);
        assert_eq!(report.finished_count, 1);
        assert_eq!(report.linear_workspace.as_deref(), Some("acme"));
    }

    #[test]
    fn assemble_marks_dirty_from_flags() {
        let rows = vec![IssueWorktree {
            worktree: "/w1".into(),
            branch: "lev/eng-2-bar".into(),
            issue_id: "ENG-2".into(),
            dirty: false,
            pr_number: None,
            pr_state: "NO_PR".into(),
            pr_url: None,
            linear_kind: None,
            linear_name: None,
            finished: false,
            reason_not_finished: None,
        }];
        let d = Discovered::for_test(rows, "/main".into(), vec!["ENG-2".into()]);
        let report = assemble(d, vec![true], Prs::for_test(vec![]), HashMap::new(), None, false);
        assert!(report.worktrees[0].dirty);
        assert!(!report.worktrees[0].finished);
    }
```

Add these test-only constructors inside the same `mod tests` block (they reach the private fields):

```rust
    impl Discovered {
        fn for_test(rows: Vec<IssueWorktree>, main_path: String, issue_ids: Vec<String>) -> Self {
            Discovered { rows, main_path, issue_ids }
        }
    }
    impl Prs {
        fn for_test(prs: Vec<Pr>) -> Self {
            Prs(prs)
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-issue status::tests::assemble 2>&1 | tail -20`
Expected: FAIL — `cannot find type Discovered` / `assemble` not found.

- [ ] **Step 3: Write minimal implementation**

In `crates/devkit-issue/src/status.rs`, add `use std::collections::HashMap;` at the top if not present. Replace the existing `build_rows` and `gather` functions (keep `Pr`, `state_rank`, `best_pr`, `reason_not_finished`, `IssueWorktree`, `StatusReport` as they are) with:

```rust
/// Local-only discovery: worktrees + dirty placeholders + issue ids + the main
/// repo path. The slow network fetches consume this. Fast — no `gh`/Linear.
pub struct Discovered {
    rows: Vec<IssueWorktree>,
    main_path: String,
    issue_ids: Vec<String>,
}

impl Discovered {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn worktree_paths(&self) -> Vec<String> {
        self.rows.iter().map(|r| r.worktree.clone()).collect()
    }
    pub fn issue_ids(&self) -> &[String] {
        &self.issue_ids
    }
}

/// An opaque GitHub PR list for a set of worktrees.
pub struct Prs(Vec<Pr>);

/// Discover worktrees and their issue ids, filtered to `ids` when non-empty.
/// Rows carry `dirty = false` placeholders; the dirty check is a separate step
/// so callers can drive it with a progress bar.
pub fn discover(start: &str, ids: &[String]) -> Result<Discovered> {
    let (main, others) = worktree::discover(start)?;
    let main_path = main.to_str().context("main repo path not UTF-8")?.to_string();
    let wanted: Vec<String> = ids.iter().map(|s| s.to_uppercase()).collect();
    let mut rows = Vec::new();
    for wt in &others {
        let iid = worktree::issue_id_of(&wt.branch, &wt.path);
        if !wanted.is_empty() && !wanted.contains(&iid) {
            continue;
        }
        rows.push(IssueWorktree {
            worktree: wt.path.to_string_lossy().into_owned(),
            branch: wt.branch.clone(),
            issue_id: iid,
            dirty: false,
            pr_number: None,
            pr_state: "NO_PR".to_string(),
            pr_url: None,
            linear_kind: None,
            linear_name: None,
            finished: false,
            reason_not_finished: None,
        });
    }
    let issue_ids = rows
        .iter()
        .filter(|r| r.issue_id != "UNKNOWN")
        .map(|r| r.issue_id.clone())
        .collect();
    Ok(Discovered { rows, main_path, issue_ids })
}

/// True when a worktree has uncommitted changes.
pub fn dirty_of(path: &str) -> bool {
    !git(&["status", "--porcelain"], path)
        .unwrap_or_default()
        .trim()
        .is_empty()
}

/// The single `gh pr list` round-trip for every worktree PR. Skips the call
/// entirely when there are no worktrees.
pub fn fetch_prs(d: &Discovered) -> Result<Prs> {
    if d.rows.is_empty() {
        return Ok(Prs(Vec::new()));
    }
    let prs: Vec<Pr> = gh_json(
        &[
            "pr", "list", "--state", "all", "--limit", "500", "--json",
            "number,state,url,headRefName",
        ],
        &d.main_path,
    )?;
    Ok(Prs(prs))
}

/// Attach dirty flags (in row order), best PR, Linear state, and the finished
/// verdict. `linear_workspace` is carried through to the report for link building.
pub fn assemble(
    d: Discovered,
    dirty: Vec<bool>,
    prs: Prs,
    linear: HashMap<String, LinearState>,
    linear_workspace: Option<String>,
    has_key: bool,
) -> StatusReport {
    let mut rows = d.rows;
    let mut finished_count = 0;
    for (i, wt) in rows.iter_mut().enumerate() {
        wt.dirty = dirty.get(i).copied().unwrap_or(false);
        let pr = if wt.branch != "DETACHED" {
            best_pr(&prs.0, &wt.branch)
        } else {
            None
        };
        if let Some(p) = pr {
            wt.pr_number = Some(p.number);
            wt.pr_state = p.state.clone();
            wt.pr_url = Some(p.url.clone());
        }
        if let Some(st) = linear.get(&wt.issue_id) {
            wt.linear_kind = Some(st.kind.clone());
            wt.linear_name = Some(st.name.clone());
        }
        let reason = reason_not_finished(wt, has_key, false);
        wt.finished = reason.is_none();
        if wt.finished {
            finished_count += 1;
        }
        wt.reason_not_finished = reason;
    }
    StatusReport {
        worktrees: rows,
        finished_count,
        has_linear_key: has_key,
        linear_workspace,
    }
}

/// Discover worktrees, fetch PRs + Linear state concurrently, and compute the
/// finished verdict. Silent — no progress output (the CLI re-orchestrates the
/// same pieces with bars). Signature unchanged for MCP/dashboard/tests.
pub fn gather(start: &str, ids: &[String]) -> Result<StatusReport> {
    let d = discover(start, ids)?;
    let key = std::env::var("LINEAR_API_KEY").ok();
    let has_key = key.is_some();
    if d.is_empty() {
        return Ok(assemble(d, Vec::new(), Prs(Vec::new()), HashMap::new(), None, has_key));
    }
    let paths = d.worktree_paths();
    let ids_v: Vec<String> = d.issue_ids().to_vec();
    let (dirty, prs, linear, ws) = std::thread::scope(|s| {
        let dt = s.spawn(|| paths.iter().map(|p| dirty_of(p)).collect::<Vec<bool>>());
        let pt = s.spawn(|| fetch_prs(&d));
        let lt = s.spawn(|| {
            (
                linear::states(&ids_v, key.as_deref()),
                linear::workspace_url_key(),
            )
        });
        let dirty = dt.join().expect("dirty thread panicked");
        let prs = pt.join().expect("prs thread panicked")?;
        let (linear, ws) = lt.join().expect("linear thread panicked");
        Ok::<_, anyhow::Error>((dirty, prs, linear, ws))
    })?;
    Ok(assemble(d, dirty, prs, linear, ws, has_key))
}
```

Note: the old `gather` early-returned an empty `StatusReport` when `rows.is_empty()`; the new `assemble(d, …, has_key)` over empty rows produces the identical empty report (zero worktrees, `finished_count = 0`, `has_linear_key = has_key`, `linear_workspace = None`). The `gh pr list` round-trip is still skipped via `fetch_prs`'s early return.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-issue 2>&1 | tail -25`
Expected: PASS — the two new `assemble_*` tests plus all existing `status::tests` (`best_pr_*`, `finished_*`, `not_finished_*`, `pr_only_*`, `verdict_combinations`).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/devkit-issue/src/status.rs
git commit -m "refactor(issue): split status gather into discover/fetch/assemble"
```

---

## Task 3: `status` CLI — drive 4 bars

**Files:**
- Modify: `src/bin/issue/status.rs`

**Interfaces:**
- Consumes: `devkit_issue::status::{discover, dirty_of, fetch_prs, assemble, StatusReport}`, `crate::spin::Steps`, `devkit_common::linear::{states, workspace_url_key}`.
- Produces: `pub fn gather_with_bars(start: &str, ids: &[String]) -> Result<StatusReport>` — used by `status::run` and the dashboard.

> This task changes UI orchestration; there is no unit test for bar output (bars are hidden off-TTY). The deliverable is verified by `cargo build` + the workspace test suite staying green, plus a manual smoke run.

- [ ] **Step 1: Rewrite `src/bin/issue/status.rs`**

```rust
use crate::triage::render;
use anyhow::Result;
use devkit_common::{linear, ui};
use devkit_issue::status::{self as st, StatusReport};
use std::collections::HashMap;

/// Discover worktrees, then fetch PRs and Linear state concurrently behind a
/// numbered group of progress bars, clearing them before the caller renders.
pub fn gather_with_bars(start: &str, ids: &[String]) -> Result<StatusReport> {
    let steps = crate::spin::Steps::new();

    let p1 = steps.spinner("[1/4] Discovering worktrees…");
    let disco = st::discover(start, ids)?;
    p1.finish_and_clear();

    let key = std::env::var("LINEAR_API_KEY").ok();
    let has_key = key.is_some();
    if disco.is_empty() {
        steps.clear();
        return Ok(st::assemble(disco, Vec::new(), st::fetch_prs(&disco)?, HashMap::new(), None, has_key));
    }

    let m = disco.len();
    let paths = disco.worktree_paths();
    let ids_v: Vec<String> = disco.issue_ids().to_vec();

    let bar2 = steps.bar(&format!("[2/4] Checking {m} worktrees"), m as u64);
    let bar3 = steps.spinner("[3/4] Fetching PRs from GitHub…");
    let _bar4 = steps.spinner("[4/4] Fetching Linear states…");

    let (dirty, prs, linear, ws) = std::thread::scope(|s| {
        let b2 = bar2.clone();
        let dt = s.spawn(move || {
            paths
                .iter()
                .map(|p| {
                    let d = st::dirty_of(p);
                    b2.inc(1);
                    d
                })
                .collect::<Vec<bool>>()
        });
        let pt = s.spawn(|| st::fetch_prs(&disco));
        let lt = s.spawn(|| {
            (
                linear::states(&ids_v, key.as_deref()),
                linear::workspace_url_key(),
            )
        });
        let dirty = dt.join().expect("dirty thread panicked");
        let prs = pt.join().expect("prs thread panicked")?;
        let (linear, ws) = lt.join().expect("linear thread panicked");
        Ok::<_, anyhow::Error>((dirty, prs, linear, ws))
    })?;

    steps.clear();
    Ok(st::assemble(disco, dirty, prs, linear, ws, has_key))
}

pub fn run(start: &str, ids: &[String]) -> Result<()> {
    let report = gather_with_bars(start, ids)?;
    let finished = render(&report);
    if finished > 0 {
        println!(
            "\n{} Run `issue end` to remove them.",
            ui::green(&format!("{finished} finished."))
        );
    }
    if !report.has_linear_key {
        println!(
            "\n{}",
            ui::dim(
                "LINEAR_API_KEY unset — Linear gate skipped. Create a key at https://linear.app/settings/api"
            )
        );
    }
    Ok(())
}
```

- [ ] **Step 2: Build to verify it compiles**

Run: `cargo build -p devkit --bin issue 2>&1 | tail -20`
Expected: builds clean (no warnings).

Note on the borrow: `disco` is borrowed by the `pt` thread (`&disco`) inside `thread::scope`; `paths`/`ids_v` are owned clones taken before the scope, so `disco` is free to move into `assemble` after the scope joins.

- [ ] **Step 3: Run the workspace tests + clippy**

Run: `cargo test --workspace 2>&1 | tail -15 && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -15`
Expected: tests PASS, clippy clean.

- [ ] **Step 4: Manual smoke**

Run: `cargo run -q --bin issue -- status 2>&1 | tail -30`
Expected: bars animate on stderr (if any worktrees exist), then clear, leaving only the status table — identical table content to before.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add src/bin/issue/status.rs
git commit -m "feat(issue): show parallel step bars for status"
```

---

## Task 4: `prs` library — one GraphQL request

**Files:**
- Modify: `crates/devkit-issue/src/prs.rs`

**Interfaces:**
- Consumes: `devkit_common::cmd::gh_json`.
- Produces: `pub fn gather(root: &str, mine: bool, reviews: bool, repo: Option<&str>) -> Result<PrsReport>` (unchanged signature); `pub fn resolve_repo(repo: Option<&str>, cwd: &str) -> Result<String>` (unchanged). `PrsReport`/`MinePrView`/`ReviewPrView` unchanged.

> Behavior note: the previous `gh pr list` (no `--repo`) was scoped to the cwd repo. GraphQL `search` is global, so the fetch resolves the repo slug once and adds a `repo:owner/name` qualifier to each search to preserve cwd-scoping. PR data is then one GraphQL round-trip; when the caller already knows the repo it passes `Some(repo)` and no resolve call is made.

- [ ] **Step 1: Write the failing test (golden fixture)**

Replace the `mine`/`check` test helpers and the `#[cfg(test)] mod tests` body in `crates/devkit-issue/src/prs.rs` with tests built on the new `PrNode` shape. Add this fixture-parse test plus rewritten unit tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn node(json: serde_json::Value) -> PrNode {
        serde_json::from_value(json).unwrap()
    }

    // A representative `gh api graphql` response parses into the views with the
    // same classification the old per-`gh pr list` path produced.
    #[test]
    fn parses_graphql_and_classifies() {
        let raw = r#"{
          "data": {
            "viewer": { "login": "me" },
            "mine": { "nodes": [
              { "number": 10, "url": "u10", "headRefName": "lev/eng-1-foo",
                "isDraft": false, "reviewDecision": "APPROVED", "mergeable": "MERGEABLE",
                "author": {"login": "me"},
                "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "SUCCESS"}}}]},
                "reviews": {"nodes": [{"author": {"login": "alice"}, "state": "APPROVED", "submittedAt": "2026-06-20T10:00:00Z"}]},
                "reviewRequests": {"nodes": []} }
            ]},
            "reviewRequested": { "nodes": [
              { "number": 20, "url": "u20", "headRefName": "x",
                "isDraft": false, "reviewDecision": "REVIEW_REQUIRED", "mergeable": "MERGEABLE",
                "author": {"login": "bob"},
                "commits": {"nodes": []},
                "reviews": {"nodes": []},
                "reviewRequests": {"nodes": [{"requestedReviewer": {"login": "me"}}]} }
            ]},
            "reviewedBy": { "nodes": [] }
          }
        }"#;
        let resp: GqlResp = serde_json::from_str(raw).unwrap();
        let report = classify(resp.data, true, true);
        assert_eq!(report.mine.len(), 1);
        assert_eq!(report.mine[0].number, 10);
        assert_eq!(report.mine[0].issue_id, "ENG-1");
        assert_eq!(report.mine[0].review_state, "approved");
        assert_eq!(report.mine[0].check_state, "ok");
        assert_eq!(report.mine[0].action, "MERGE");
        assert_eq!(report.reviews.len(), 1);
        assert_eq!(report.reviews[0].number, 20);
        assert_eq!(report.reviews[0].my_vote, "-");
        assert_eq!(report.reviews[0].action, "REVIEW NEEDED");
    }

    fn mine_node(decision: Option<&str>, mergeable: &str, draft: bool, rollup: Option<&str>) -> PrNode {
        let commits = match rollup {
            Some(s) => serde_json::json!({"nodes": [{"commit": {"statusCheckRollup": {"state": s}}}]}),
            None => serde_json::json!({"nodes": []}),
        };
        node(serde_json::json!({
            "number": 1, "url": "u", "headRefName": "h",
            "isDraft": draft, "reviewDecision": decision, "mergeable": mergeable,
            "author": {"login": "x"}, "commits": commits,
            "reviews": {"nodes": []}, "reviewRequests": {"nodes": []}
        }))
    }

    #[test]
    fn checks_fail_run_ok_empty() {
        assert_eq!(checks_text(None), "-");
        assert_eq!(checks_text(Some("SUCCESS")), "ok");
        assert_eq!(checks_text(Some("FAILURE")), "fail");
        assert_eq!(checks_text(Some("ERROR")), "fail");
        assert_eq!(checks_text(Some("PENDING")), "run");
        assert_eq!(checks_text(Some("EXPECTED")), "run");
    }

    #[test]
    fn approved_green_merges() {
        assert_eq!(mine_action(&mine_node(Some("APPROVED"), "MERGEABLE", false, Some("SUCCESS")), "me"), "MERGE");
    }
    #[test]
    fn approved_with_failing_ci() {
        assert_eq!(mine_action(&mine_node(Some("APPROVED"), "MERGEABLE", false, Some("FAILURE")), "me"), "fix CI -> merge");
    }
    #[test]
    fn changes_requested_action() {
        assert_eq!(mine_action(&mine_node(Some("CHANGES_REQUESTED"), "MERGEABLE", false, None), "me"), "address changes");
    }
    #[test]
    fn draft_action() {
        assert_eq!(mine_action(&mine_node(None, "MERGEABLE", true, None), "me"), "draft");
    }
    #[test]
    fn review_text_variants() {
        assert_eq!(review_text(&mine_node(Some("APPROVED"), "x", false, None)), "approved");
        assert_eq!(review_text(&mine_node(Some("CHANGES_REQUESTED"), "x", false, None)), "changes");
        assert_eq!(review_text(&mine_node(None, "x", false, None)), "awaiting");
    }
    #[test]
    fn reviewer_state_requested_needs_review() {
        let pr = node(serde_json::json!({
            "number": 1, "url": "u", "headRefName": "h", "isDraft": false,
            "reviewDecision": null, "mergeable": "MERGEABLE", "author": {"login": "other"},
            "commits": {"nodes": []}, "reviews": {"nodes": []},
            "reviewRequests": {"nodes": [{"requestedReviewer": {"login": "me"}}]}
        }));
        let (vote, action) = reviewer_state(&pr, "me");
        assert_eq!(vote, "-");
        assert_eq!(action, "REVIEW NEEDED");
    }
    #[test]
    fn reviewer_state_approved_done() {
        let pr = node(serde_json::json!({
            "number": 1, "url": "u", "headRefName": "h", "isDraft": false,
            "reviewDecision": null, "mergeable": "MERGEABLE", "author": {"login": "other"},
            "commits": {"nodes": []},
            "reviews": {"nodes": [{"author": {"login": "me"}, "state": "APPROVED", "submittedAt": "2026-01-01T00:00:00Z"}]},
            "reviewRequests": {"nodes": []}
        }));
        let (vote, action) = reviewer_state(&pr, "me");
        assert_eq!(vote, "approved");
        assert_eq!(action, "done (approved)");
    }
    #[test]
    fn issue_of_finds_swe() {
        assert_eq!(issue_of("lev/swe-123-fix"), "SWE-123");
        assert_eq!(issue_of("main"), "-");
    }
    #[test]
    fn issue_of_finds_non_swe_prefix() {
        assert_eq!(issue_of("lev/eng-1234-fix"), "ENG-1234");
        assert_eq!(issue_of("feature/abc-9-thing"), "ABC-9");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-issue prs:: 2>&1 | tail -20`
Expected: FAIL — `cannot find type GqlResp` / `PrNode` / `classify` / `checks_text`.

- [ ] **Step 3: Write the implementation**

In `crates/devkit-issue/src/prs.rs`: delete the old `gh` JSON structs (`Check`, `Author`, `Review`, `ReviewRequest`, `MinePr`, `ReviewPr`, `Me`) and the fetch functions (`fetch_mine`, `fetch_reviews`). Keep `RepoInfo`, `resolve_repo`, the constants (`BOTS`, `FAIL`, `RUNNING`), `is_bot`, `issue_of`, the view structs (`MinePrView`, `ReviewPrView`, `PrsReport`), and `has_replied`'s intent. Add the GraphQL structs and rewrite the classifiers + fetch + gather:

```rust
// GraphQL response shapes ---------------------------------------------------------

#[derive(serde::Deserialize)]
struct GqlResp {
    data: GqlData,
}

#[derive(serde::Deserialize)]
struct GqlData {
    viewer: Viewer,
    mine: SearchNodes,
    #[serde(rename = "reviewRequested")]
    review_requested: SearchNodes,
    #[serde(rename = "reviewedBy")]
    reviewed_by: SearchNodes,
}

#[derive(serde::Deserialize)]
struct Viewer {
    login: String,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct SearchNodes {
    nodes: Vec<PrNode>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct ActorLogin {
    login: String,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct ReviewNode {
    author: ActorLogin,
    state: String,
    #[serde(rename = "submittedAt")]
    submitted_at: String,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct ReviewConn {
    nodes: Vec<ReviewNode>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct ReqNode {
    #[serde(rename = "requestedReviewer")]
    requested_reviewer: Option<ActorLogin>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct ReqConn {
    nodes: Vec<ReqNode>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct Rollup {
    state: String,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct CommitInner {
    #[serde(rename = "statusCheckRollup")]
    status_check_rollup: Option<Rollup>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct CommitNode {
    commit: CommitInner,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct CommitsConn {
    nodes: Vec<CommitNode>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct PrNode {
    number: u64,
    url: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "isDraft")]
    is_draft: bool,
    #[serde(rename = "reviewDecision")]
    review_decision: Option<String>,
    mergeable: String,
    author: ActorLogin,
    commits: CommitsConn,
    reviews: ReviewConn,
    #[serde(rename = "reviewRequests")]
    review_requests: ReqConn,
}

impl PrNode {
    /// The status-check rollup state of the last commit, if any.
    fn rollup_state(&self) -> Option<&str> {
        self.commits
            .nodes
            .first()
            .and_then(|c| c.commit.status_check_rollup.as_ref())
            .map(|r| r.state.as_str())
    }
}
```

Replace `checks_of` with `checks_text` (rollup state → label), and update `mine_action` to use it. Keep `review_text`, `has_replied`, `mine_action`, `reviewer_state` operating on `&PrNode`:

```rust
fn checks_text(rollup: Option<&str>) -> &'static str {
    match rollup {
        None => "-",
        Some("SUCCESS") => "ok",
        Some(s) if FAIL.contains(&s) => "fail",
        Some(_) => "run",
    }
}

fn review_text(pr: &PrNode) -> &'static str {
    match pr.review_decision.as_deref() {
        Some("CHANGES_REQUESTED") => "changes",
        Some("APPROVED") => "approved",
        Some("REVIEW_REQUIRED") => "awaiting",
        _ => {
            if pr.reviews.nodes.is_empty() {
                "awaiting"
            } else {
                "commented"
            }
        }
    }
}

/// True when my latest review is newer than the latest non-bot reviewer's.
fn has_replied(pr: &PrNode, me: &str) -> bool {
    let mine = pr
        .reviews
        .nodes
        .iter()
        .filter(|r| r.author.login == me)
        .map(|r| r.submitted_at.as_str())
        .max()
        .unwrap_or("");
    let theirs = pr
        .reviews
        .nodes
        .iter()
        .filter(|r| r.author.login != me && !is_bot(&r.author.login))
        .map(|r| r.submitted_at.as_str())
        .max()
        .unwrap_or("");
    !mine.is_empty() && mine > theirs
}

fn mine_action(pr: &PrNode, me: &str) -> String {
    if pr.is_draft {
        return "draft".into();
    }
    let conflict = pr.mergeable == "CONFLICTING";
    match pr.review_decision.as_deref() {
        Some("CHANGES_REQUESTED") => {
            let base = if has_replied(pr, me) {
                "replied; await re-review"
            } else {
                "address changes"
            };
            format!("{base}{}", if conflict { " + rebase" } else { "" })
        }
        Some("APPROVED") => {
            if conflict {
                "rebase -> merge".into()
            } else if checks_text(pr.rollup_state()) == "fail" {
                "fix CI -> merge".into()
            } else {
                "MERGE".into()
            }
        }
        _ => format!("awaiting review{}", if conflict { "; rebase" } else { "" }),
    }
}

/// (my_vote, action) for a PR where I'm a reviewer. My latest review state is
/// taken from `reviews` (most recent by submittedAt among my reviews).
fn reviewer_state(pr: &PrNode, me: &str) -> (String, String) {
    let vote = pr
        .reviews
        .nodes
        .iter()
        .filter(|r| r.author.login == me)
        .max_by(|a, b| a.submitted_at.cmp(&b.submitted_at))
        .map(|r| r.state.clone())
        .unwrap_or_default();
    let requested = pr
        .review_requests
        .nodes
        .iter()
        .filter_map(|r| r.requested_reviewer.as_ref())
        .any(|rr| rr.login == me);
    let vote_label = match vote.as_str() {
        "APPROVED" => "approved",
        "CHANGES_REQUESTED" => "changes",
        "COMMENTED" => "commented",
        _ => "-",
    }
    .to_string();
    let action = if requested {
        "REVIEW NEEDED"
    } else {
        match vote.as_str() {
            "APPROVED" => "done (approved)",
            "CHANGES_REQUESTED" => "awaiting author fixes",
            "COMMENTED" => "commented; decide",
            _ => "REVIEW NEEDED",
        }
    }
    .to_string();
    (vote_label, action)
}
```

Add the single-request fetch and a pure `classify` (so the parse path is unit-tested), then rewrite `gather`:

```rust
const PR_FIELDS: &str = "number url headRefName isDraft reviewDecision mergeable \
author { login } \
commits(last: 1) { nodes { commit { statusCheckRollup { state } } } } \
reviews(first: 100) { nodes { author { login } state submittedAt } } \
reviewRequests(first: 100) { nodes { requestedReviewer { ... on User { login } } } }";

fn build_query(repo: &str) -> String {
    let scope = format!("repo:{repo} ");
    let frag = format!("nodes {{ ... on PullRequest {{ {PR_FIELDS} }} }}");
    format!(
        "query {{ viewer {{ login }} \
mine: search(query: \"{scope}is:pr is:open author:@me\", type: ISSUE, first: 100) {{ {frag} }} \
reviewRequested: search(query: \"{scope}is:pr is:open review-requested:@me\", type: ISSUE, first: 100) {{ {frag} }} \
reviewedBy: search(query: \"{scope}is:pr is:open reviewed-by:@me\", type: ISSUE, first: 100) {{ {frag} }} }}"
    )
}

/// Turn one GraphQL response into the report. Pure → unit-tested.
fn classify(data: GqlData, want_mine: bool, want_reviews: bool) -> PrsReport {
    let me = data.viewer.login;

    let mine_views: Vec<MinePrView> = if want_mine {
        data.mine
            .nodes
            .iter()
            .filter(|pr| pr.number != 0)
            .map(|pr| MinePrView {
                number: pr.number,
                url: pr.url.clone(),
                issue_id: issue_of(&pr.head_ref_name),
                review_state: review_text(pr).to_string(),
                check_state: checks_text(pr.rollup_state()).to_string(),
                action: mine_action(pr, &me),
            })
            .collect()
    } else {
        Vec::new()
    };

    let review_views: Vec<ReviewPrView> = if want_reviews {
        let mut seen: BTreeMap<u64, PrNode> = BTreeMap::new();
        for pr in data
            .review_requested
            .nodes
            .into_iter()
            .chain(data.reviewed_by.nodes)
            .filter(|pr| pr.number != 0 && pr.author.login != me)
        {
            seen.entry(pr.number).or_insert(pr);
        }
        seen.into_values()
            .map(|pr| {
                let (my_vote, action) = reviewer_state(&pr, &me);
                ReviewPrView {
                    number: pr.number,
                    url: pr.url.clone(),
                    author: pr.author.login.clone(),
                    my_vote,
                    action,
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    PrsReport {
        mine: mine_views,
        reviews: review_views,
    }
}

/// Fetch and classify the caller's PRs in a single GraphQL round-trip. Neither
/// flag set ⇒ both groups. Stateless: no diff cache is read or written.
pub fn gather(root: &str, mine: bool, reviews: bool, repo: Option<&str>) -> Result<PrsReport> {
    let want_mine = mine || !reviews;
    let want_reviews = reviews || !mine;
    let repo = match repo {
        Some(r) => r.to_string(),
        None => resolve_repo(None, root)?,
    };
    let query = build_query(&repo);
    let arg = format!("query={query}");
    let resp: GqlResp = gh_json(&["api", "graphql", "-f", &arg], root)?;
    Ok(classify(resp.data, want_mine, want_reviews))
}
```

(`BTreeMap` is already imported at the top of the file; keep that `use`.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-issue prs:: 2>&1 | tail -25`
Expected: PASS — `parses_graphql_and_classifies` plus all rewritten unit tests.

- [ ] **Step 5: Verify the live query shape (manual)**

Run: `gh api graphql -f query="$(cat <<'EOF'
query { viewer { login }
  mine: search(query: "repo:cli/cli is:pr is:open author:@me", type: ISSUE, first: 1) {
    nodes { ... on PullRequest { number url headRefName isDraft reviewDecision mergeable
      author { login }
      commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
      reviews(first: 1) { nodes { author { login } state submittedAt } }
      reviewRequests(first: 1) { nodes { requestedReviewer { ... on User { login } } } } } } } }
EOF
)" 2>&1 | tail -20`
Expected: a JSON object with `data.viewer.login` and the three search aliases — confirms every field name resolves against the live schema. If `gh` is unavailable, skip; the golden-fixture test already exercises the parse path.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/devkit-issue/src/prs.rs
git commit -m "perf(issue): fetch all prs in one graphql request"
```

---

## Task 5: `prs` CLI — 2 concurrent bars

**Files:**
- Modify: `src/bin/issue/prs.rs`

**Interfaces:**
- Consumes: `crate::spin::Steps`, `devkit_issue::prs::{gather, resolve_repo}`, `devkit_common::linear::workspace_url_key`.

> UI orchestration; verified by build + workspace tests + manual smoke.

- [ ] **Step 1: Rewrite the `run` entry point in `src/bin/issue/prs.rs`**

Replace the body of `pub fn run(...)` (lines ~147-195, the spinner block through the fetch) so the Linear-workspace lookup and the GitHub fetch run concurrently behind two bars. The repo is resolved once on the GitHub thread and reused for both the fetch scope and the cache key:

```rust
pub fn run(mine: bool, reviews: bool, repo: Option<String>, no_cache: bool) -> Result<()> {
    let want_mine = mine || !reviews;
    let want_reviews = reviews || !mine;

    let steps = crate::spin::Steps::new();
    let _b1 = steps.spinner("[1/2] Resolving Linear workspace…");
    let _b2 = steps.spinner("[2/2] Fetching PRs from GitHub…");

    let (url_key, report, repo_key) = std::thread::scope(|s| {
        let linear_t = s.spawn(devkit_common::linear::workspace_url_key);
        let github_t = s.spawn(|| -> Result<_> {
            let resolved = devkit_issue::prs::resolve_repo(repo.as_deref(), ".")?;
            let report = devkit_issue::prs::gather(".", mine, reviews, Some(&resolved))?;
            let repo_key = if no_cache { None } else { Some(resolved) };
            Ok((report, repo_key))
        });
        let url_key = linear_t.join().expect("linear thread panicked");
        let (report, repo_key) = github_t.join().expect("github thread panicked")?;
        Ok::<_, anyhow::Error>((url_key, report, repo_key))
    })?;

    steps.clear();

    let path = repo_key.as_ref().map(|r| cache_path(r));
    let mut cache: Snap = path.as_deref().map(load_cache).unwrap_or_default();

    if want_mine {
        let prev = cache.get("mine").cloned().unwrap_or_default();
        let cur = mine_table(&report.mine, url_key.as_deref(), &prev);
        cache.insert("mine".to_string(), cur);
    }
    if want_reviews {
        let prev = cache.get("reviews").cloned().unwrap_or_default();
        let cur = reviews_table(&report.reviews, &prev);
        cache.insert("reviews".to_string(), cur);
    }

    if (want_mine && !report.mine.is_empty()) || (want_reviews && !report.reviews.is_empty()) {
        println!(
            "\n{} {} (REVIEW NEEDED · address changes · fix CI) · {} (MERGE · done) · {} (awaiting author fixes) · {}",
            ui::dim("ACTION colour:"),
            ui::red("needs you"),
            ui::green("ready to land"),
            ui::yellow("waiting on author"),
            ui::dim("passive (awaiting review · draft)"),
        );
        println!(
            "{}",
            ui::dim("old → new in a cell = value changed since the last run.")
        );
    }

    if let Some(p) = &path {
        save_cache(p, &cache)?;
    }
    Ok(())
}
```

Note: `resolve_repo` previously ran only for the cache key; it now also scopes the GraphQL search, so it moves onto the GitHub thread and its result is reused for both. The old separate `prs::gather(".", mine, reviews, repo.as_deref())` + later `resolve_repo` calls are removed.

- [ ] **Step 2: Build to verify it compiles**

Run: `cargo build -p devkit --bin issue 2>&1 | tail -20`
Expected: builds clean.

- [ ] **Step 3: Run workspace tests + clippy**

Run: `cargo test --workspace 2>&1 | tail -15 && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -15`
Expected: PASS, clippy clean. (The `diff_cell_shows_change` test in this file is untouched and stays green.)

- [ ] **Step 4: Manual smoke**

Run: `cargo run -q --bin issue -- prs 2>&1 | tail -30`
Expected: two bars animate then clear; the two PR tables print as before.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add src/bin/issue/prs.rs
git commit -m "feat(issue): fetch prs and linear workspace in parallel"
```

---

## Task 6: Linear history per-page progress callback

**Files:**
- Modify: `crates/devkit-common/src/linear.rs`

**Interfaces:**
- Produces:
  - `pub fn assigned_issue_history_with_progress(key: &str, on_page: impl FnMut(usize)) -> Result<Vec<AssignedIssue>>` — invokes `on_page(total_so_far)` after each page.
  - `pub fn assigned_issue_history(key: &str) -> Result<Vec<AssignedIssue>>` — unchanged signature; delegates with a no-op callback.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `crates/devkit-common/src/linear.rs`:

```rust
    #[test]
    fn assigned_history_no_op_wrapper_exists() {
        // Compile-time guarantee that the no-op wrapper still delegates to the
        // progress variant with the same return type.
        fn _assert_sig(k: &str) -> Result<Vec<AssignedIssue>> {
            assigned_issue_history(k)
        }
        fn _assert_progress(k: &str) -> Result<Vec<AssignedIssue>> {
            assigned_issue_history_with_progress(k, |_n| {})
        }
        let _ = (_assert_sig, _assert_progress);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-common linear::tests::assigned_history_no_op 2>&1 | tail -20`
Expected: FAIL — `assigned_issue_history_with_progress` not found.

- [ ] **Step 3: Write the implementation**

Rename the existing `assigned_issue_history` body to the `_with_progress` variant and add the callback; add a thin wrapper. Replace the current `pub fn assigned_issue_history(key: &str) -> Result<Vec<AssignedIssue>>` with:

```rust
/// Every issue assigned to me, paginated. Empty on no key / network error.
pub fn assigned_issue_history(key: &str) -> Result<Vec<AssignedIssue>> {
    assigned_issue_history_with_progress(key, |_| {})
}

/// As [`assigned_issue_history`], calling `on_page` with the running total after
/// each fetched page — lets a caller show a rising count while pages stream in.
pub fn assigned_issue_history_with_progress(
    key: &str,
    mut on_page: impl FnMut(usize),
) -> Result<Vec<AssignedIssue>> {
    let mut out = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let resp: serde_json::Value = ureq::post("https://api.linear.app/graphql")
            .set("Authorization", key)
            .send_json(ureq::json!({ "query": assigned_query(after.as_deref()) }))?
            .into_json()?;
        let block = &resp["data"]["issues"];
        if let Some(nodes) = block["nodes"].as_array() {
            for n in nodes {
                let state: StateRef = serde_json::from_value(n["state"].clone())?;
                let mut history = Vec::new();
                if let Some(hn) = n["history"]["nodes"].as_array() {
                    for h in hn {
                        let from = serde_json::from_value(h["fromState"].clone()).ok();
                        let to = serde_json::from_value(h["toState"].clone()).ok();
                        let when = h["createdAt"].as_str().unwrap_or("").to_string();
                        history.push((when, from, to));
                    }
                }
                out.push(AssignedIssue {
                    identifier: n["identifier"].as_str().unwrap_or("").to_string(),
                    created_at: n["createdAt"].as_str().unwrap_or("").to_string(),
                    state,
                    history,
                });
            }
        }
        on_page(out.len());
        match (
            block["pageInfo"]["hasNextPage"].as_bool(),
            block["pageInfo"]["endCursor"].as_str(),
        ) {
            (Some(true), Some(cursor)) => after = Some(cursor.to_string()),
            _ => return Ok(out),
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-common linear:: 2>&1 | tail -20`
Expected: PASS, including the existing `assigned_query_paginates`.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/devkit-common/src/linear.rs
git commit -m "feat(linear): add per-page progress callback to issue history"
```

---

## Task 7: Dashboard — status bars, rising count, parallel history

**Files:**
- Modify: `src/bin/issue/dashboard/data.rs`
- Modify: `src/bin/issue/dashboard/mod.rs`

**Interfaces:**
- Consumes: `crate::status::gather_with_bars`, `crate::spin::Steps`, `devkit_common::linear::assigned_issue_history_with_progress`.
- Produces: `pub fn issues(use_cache: bool, on_page: impl FnMut(usize)) -> Vec<AssignedIssue>` (signature gains the callback).

> UI orchestration; verified by build + workspace tests + manual smoke.

- [ ] **Step 1: Add the callback to `data::issues`**

In `src/bin/issue/dashboard/data.rs`, change the signature and the fetch call:

```rust
pub fn issues(use_cache: bool, on_page: impl FnMut(usize)) -> Vec<AssignedIssue> {
    let Ok(key) = std::env::var("LINEAR_API_KEY") else {
        return Vec::new();
    };
    if use_cache && let Some(v) = cache::get::<Vec<AssignedIssue>>("issues", TTL_SECS) {
        return v;
    }
    let v = match linear::assigned_issue_history_with_progress(&key, on_page) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Linear history fetch failed: {e}");
            Vec::new()
        }
    };
    if use_cache && !v.is_empty() {
        cache::put("issues", &v);
    }
    v
}
```

- [ ] **Step 2: Rewire `dashboard::run` in `src/bin/issue/dashboard/mod.rs`**

Three edits (leave the chart/bucket logic between them untouched):

(a) Replace the status-panel fetch + the now-unused `prs` import. Change:

```rust
    // At-a-glance: worktree triage, then my PRs + PRs awaiting my review.
    let report = devkit_issue::status::gather(&start, &[])?;
    triage::render(&report);
```

to:

```rust
    // At-a-glance: worktree triage, then my PRs + PRs awaiting my review.
    let report = crate::status::gather_with_bars(&start, &[])?;
    triage::render(&report);
```

(b) Replace the issue-history spinner block. Change:

```rust
    let pb = crate::spin::spinner("Loading Linear issue history…");
    let issues = data::issues(use_cache);
    pb.finish_and_clear();
```

to:

```rust
    let steps = crate::spin::Steps::new();
    let pb = steps.spinner("Loading Linear issue history…");
    let issues = data::issues(use_cache, |n| {
        pb.set_message(format!("Loading Linear issue history… {n} issues"));
    });
    steps.clear();
```

(c) Replace the PR/commit-history spinner block. Change:

```rust
    let pb = crate::spin::spinner("Loading PR and commit history…");
    let (opened, merged, add, del) = data::pr_timeline(args.all_roles, use_cache);
    let author = match args.author.clone() {
        Some(a) => a,
        None => capture_email(&start),
    };
    let monorepo = monorepo_dir(&args)?;
    let commits = data::commit_dates(&monorepo, &author);
    pb.finish_and_clear();
```

to:

```rust
    let author = match args.author.clone() {
        Some(a) => a,
        None => capture_email(&start),
    };
    let monorepo = monorepo_dir(&args)?;
    let steps = crate::spin::Steps::new();
    let _b1 = steps.spinner("[1/2] Loading PR history…");
    let _b2 = steps.spinner("[2/2] Loading commit history…");
    let (opened, merged, add, del, commits) = std::thread::scope(|s| {
        let pr_t = s.spawn(|| data::pr_timeline(args.all_roles, use_cache));
        let commit_t = s.spawn(|| data::commit_dates(&monorepo, &author));
        let (opened, merged, add, del) = pr_t.join().expect("pr timeline thread panicked");
        let commits = commit_t.join().expect("commit thread panicked");
        (opened, merged, add, del, commits)
    });
    steps.clear();
```

- [ ] **Step 3: Build to verify it compiles**

Run: `cargo build -p devkit --bin issue 2>&1 | tail -20`
Expected: builds clean. If a warning fires for an unused `prs` import in `mod.rs`, remove `prs` from the `use crate::{prs, triage};` line (it is still used via `prs::run` later in the file — confirm before removing; keep it if `prs::run` remains).

- [ ] **Step 4: Run workspace tests + clippy**

Run: `cargo test --workspace 2>&1 | tail -15 && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -15`
Expected: PASS, clippy clean.

- [ ] **Step 5: Manual smoke**

Run: `cargo run -q --bin issue -- dashboard 2>&1 | tail -40`
Expected: status panel shows the 4-step bars; issue-history shows a rising "… N issues" count; PR/commit history shows 2 parallel bars; all clear before each chart/table prints.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add src/bin/issue/dashboard/data.rs src/bin/issue/dashboard/mod.rs
git commit -m "feat(issue): add step bars and parallel history to dashboard"
```

---

## Task 8: Full gate + README touch

**Files:**
- Modify: `README.md` (only if it documents the spinner/fetch behavior — check first)

- [ ] **Step 1: Check whether docs mention the spinner**

Run: `rg -n "spinner|Fetching|progress" README.md docs/configuration.md 2>&1 | tail -20`
Expected: review hits; if any describe the single-spinner behavior, update the wording to "numbered step bars" in one edit. If nothing relevant, skip the edit.

- [ ] **Step 2: Run the full merge gate**

Run: `cargo fmt --all --check && cargo test --workspace 2>&1 | tail -15 && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -15`
Expected: fmt clean, all tests PASS, clippy clean.

- [ ] **Step 3: Commit any doc change**

```bash
git add README.md
git commit -m "docs: describe issue step progress bars"
```

(Skip if Step 1 produced no edit.)

---

## Self-Review

**Spec coverage:**
- Numbered step bars, no emoji, cyan accent → Task 1 (`Steps`), used in Tasks 3/5/7.
- Determinate fill for known-count loop (worktree dirty-check) → Task 3 (`steps.bar`, `dirty_of` loop).
- Honest spinners for batched network calls → Tasks 3/5/7.
- Concurrent fetches (gh ‖ Linear in status; workspace ‖ github in prs; pr_timeline ‖ commit_dates in dashboard) → Tasks 3/5/7 via `thread::scope`.
- One GitHub request for prs → Task 4 (`build_query` + `gh api graphql`).
- `status` already 1 request → preserved in Task 2 (`fetch_prs`).
- Clear bars then print results → `steps.clear()` in Tasks 3/5/7.
- Rising count for paginated Linear history → Tasks 6 + 7.
- `indicatif` stays out of library crates → Tasks 1/2/4/6 add no `indicatif` dep.
- `gather`/`prs::gather` signatures unchanged (MCP/tests) → Tasks 2/4.
- Library parallelism for free (MCP/dashboard direct callers) → Task 2 `gather` `thread::scope`.
- Hidden off-TTY → Task 1 `ProgressDrawTarget::hidden()`, asserted in Task 1 test.
- TDD with golden fixture for GraphQL parse → Task 4 `parses_graphql_and_classifies`.

**Placeholder scan:** none — every code step shows complete code; commands have expected output.

**Type consistency:** `Steps::{new,spinner,bar,clear}`, `ProgressBar` return types, `Discovered`/`Prs`/`discover`/`dirty_of`/`fetch_prs`/`assemble` signatures, `GqlResp`/`GqlData`/`PrNode`/`classify`/`checks_text`, and `assigned_issue_history_with_progress(key, on_page)` are used identically across the tasks that define and consume them. `gather`(status) and `prs::gather` keep their pre-existing signatures.
