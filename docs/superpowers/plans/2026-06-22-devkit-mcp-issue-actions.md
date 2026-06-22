# devkit MCP `issue` read actions (phase 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expose the two read-only `issue` operations (`issue.status`, `issue.prs`) as devkit MCP actions by extracting their data-gathering into a new `devkit-issue` library crate that both the CLI and the MCP handler consume.

**Architecture:** A new `devkit-issue` crate owns pure, serializable data-gathering (`status::gather`, `prs::gather`) with no rendering and no mutations. The `issue` binary keeps its rendering and diff-cache and is refactored to call the facade (single source of truth). A new `crates/devkit-mcp/src/issue.rs` registers the two actions, mirroring `devrun.rs`. The "finished/not-finished" verdict moves from the renderer into the data layer so CLI and MCP share it.

**Tech Stack:** Rust 2024 workspace; `anyhow`, `serde`/`serde_json`; existing `devkit-common` (`cmd`, `worktree`, `linear`, `ui`) and the hand-rolled MCP server (no async/tokio/rmcp). External tools: `git`, `gh`, Linear GraphQL.

## Global Constraints

Copied verbatim from the spec (`docs/superpowers/specs/2026-06-22-devkit-mcp-issue-actions-design.md`). Every task's requirements implicitly include these:

- **Read-only only.** Both actions run `git worktree list`, `gh pr list`, `gh api user`, `gh repo view`, and Linear GraphQL queries. No mutations, no stdin. The facade writes no files.
- **No shelling.** The MCP handler calls the `devkit-issue` facade directly; it never execs the `issue` binary.
- **`prs` facade is stateless.** The `~/.cache/devkit/pr-status/` diff cache stays CLI-only; the facade neither reads nor writes it.
- **Spinner stays CLI-only.** The `indicatif` progress spinner (`src/bin/issue/spin.rs`) does not move into the crate; the facade takes no `ProgressBar`.
- **No CLI behavior change.** `issue status` and `issue prs` produce the same tables/output as before the refactor.
- **Facade home is the new crate `devkit-issue`** (depends on `devkit-common` only for this phase).
- **Verdict lives in the data layer.** `reason_not_finished` is computed in `devkit-issue::status` and is the single source for both the CLI table and the MCP response.
- **Commits:** Conventional Commits (`type(scope): description`, imperative, ≤50 chars subject, lowercase after colon, no trailing period). Footer on every commit: `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.
- **Gate (run before every commit):** `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all` (CI checks `--check`).
- **Project-agnostic.** No real-project names; use `example`/`exampleuser` placeholders in any docs/tests.

---

### Task 1: Create `devkit-issue` crate and extract the `status` facade

Creates the new crate and moves `issue`'s worktree-status gathering into `devkit-issue::status`, with the verdict computed in the data layer. Refactors the binary (`status.rs`, `triage.rs`, and the out-of-scope-but-coupled `end.rs`) to consume it so the workspace compiles. This is one cohesive compile-unit change: the binary cannot half-reference the old and new gatherer.

**Files:**
- Create: `crates/devkit-issue/Cargo.toml`
- Create: `crates/devkit-issue/src/lib.rs`
- Create: `crates/devkit-issue/src/status.rs`
- Modify: `Cargo.toml` (workspace members + workspace.dependencies + root `[dependencies]`)
- Modify: `src/bin/issue/triage.rs` (becomes render-only, consuming the facade structs)
- Modify: `src/bin/issue/status.rs` (calls the facade)
- Modify: `src/bin/issue/end.rs` (repoint gather/render/reason_not_finished to the facade)

**Interfaces:**
- Produces (consumed by Task 2's binary `prs.rs` indirectly, Task 3's MCP handler, and `end.rs`):
  ```rust
  // devkit_issue::status
  #[derive(Debug, Clone, serde::Serialize)]
  pub struct IssueWorktree {
      pub worktree: String,
      pub branch: String,
      pub issue_id: String,
      pub dirty: bool,
      pub pr_number: Option<u64>,
      pub pr_state: String,            // MERGED | OPEN | CLOSED | NO_PR
      pub pr_url: Option<String>,
      pub linear_kind: Option<String>, // completed | started | unstarted | … (None = no Linear entry)
      pub linear_name: Option<String>, // e.g. "Done"
      pub finished: bool,
      pub reason_not_finished: Option<String>,
  }
  #[derive(Debug, Clone, serde::Serialize)]
  pub struct StatusReport {
      pub worktrees: Vec<IssueWorktree>,
      pub finished_count: usize,
      pub has_linear_key: bool,
      pub linear_workspace: Option<String>,
  }
  pub fn gather(start: &str, ids: &[String]) -> anyhow::Result<StatusReport>;
  pub fn reason_not_finished(wt: &IssueWorktree, has_key: bool, pr_only: bool) -> Option<String>;
  ```

- [ ] **Step 1: Scaffold the crate manifest**

Create `crates/devkit-issue/Cargo.toml`:

```toml
[package]
name = "devkit-issue"
edition.workspace = true
version = "0.2.0" # x-release-please-version

[dependencies]
anyhow.workspace = true
serde = { workspace = true }
devkit-common.workspace = true
```

- [ ] **Step 2: Register the crate in the workspace**

In the root `Cargo.toml`, add the member and the workspace dependency, and make it a dependency of the root `devkit` package (the `issue` binary lives there).

Members line (was `members = ["crates/devkit-common", "crates/devkit-ports", "crates/devkit-locks", "crates/devkit-mcp"]`):

```toml
members = ["crates/devkit-common", "crates/devkit-ports", "crates/devkit-locks", "crates/devkit-issue", "crates/devkit-mcp"]
```

Under `[workspace.dependencies]`, after the `devkit-locks` line, add:

```toml
devkit-issue = { path = "crates/devkit-issue" }
```

Under the root package `[dependencies]`, after `devkit-locks.workspace = true`, add:

```toml
devkit-issue.workspace = true
```

- [ ] **Step 3: Write `crates/devkit-issue/src/lib.rs`**

```rust
pub mod prs;
pub mod status;
```

Create an empty `crates/devkit-issue/src/prs.rs` for now so the module resolves (Task 2 fills it):

```rust
// Filled in Task 2 (issue.prs facade).
```

- [ ] **Step 4: Write `crates/devkit-issue/src/status.rs`**

Move the gathering logic out of `src/bin/issue/triage.rs`. The PR-matching helpers (`Pr`, `state_rank`, `best_pr`) move verbatim except `pub(crate)` becomes private. `build_rows` loses its `ProgressBar` parameter, gains an early return when there are no non-main worktrees (so no `gh` call happens — this is what makes `issue.status` testable without `gh`), and builds the new struct. `gather` attaches Linear state and computes the verdict per row. `reason_not_finished` reads the struct fields.

```rust
use anyhow::{Context, Result};
use devkit_common::cmd::{gh_json, git};
use devkit_common::linear;
use devkit_common::worktree;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
struct Pr {
    number: u64,
    state: String, // MERGED | OPEN | CLOSED
    url: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
}

fn state_rank(s: &str) -> u8 {
    match s {
        "MERGED" => 3,
        "OPEN" => 2,
        "CLOSED" => 1,
        _ => 0,
    }
}

/// Best PR for a head branch: prefer MERGED > OPEN > CLOSED, then higher number.
fn best_pr<'a>(prs: &'a [Pr], head: &str) -> Option<&'a Pr> {
    prs.iter()
        .filter(|p| p.head_ref_name == head)
        .max_by_key(|p| (state_rank(&p.state), p.number))
}

/// One issue worktree with its PR + Linear state and the finished verdict.
#[derive(Debug, Clone, Serialize)]
pub struct IssueWorktree {
    pub worktree: String,
    pub branch: String,
    pub issue_id: String,
    pub dirty: bool,
    pub pr_number: Option<u64>,
    pub pr_state: String, // MERGED|OPEN|CLOSED|NO_PR
    pub pr_url: Option<String>,
    pub linear_kind: Option<String>,
    pub linear_name: Option<String>,
    pub finished: bool,
    pub reason_not_finished: Option<String>,
}

/// The full status snapshot for a set of worktrees.
#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    pub worktrees: Vec<IssueWorktree>,
    pub finished_count: usize,
    pub has_linear_key: bool,
    pub linear_workspace: Option<String>,
}

/// Discover worktrees and their best PR. Returns an empty vec (and skips the
/// `gh` round-trip) when the repo has no non-main worktrees.
fn build_rows(start: &str) -> Result<Vec<IssueWorktree>> {
    let (main, others) = worktree::discover(start)?;
    if others.is_empty() {
        return Ok(Vec::new());
    }
    let main_s = main.to_str().context("main repo path not UTF-8")?;
    let prs: Vec<Pr> = gh_json(
        &[
            "pr",
            "list",
            "--state",
            "all",
            "--limit",
            "500",
            "--json",
            "number,state,url,headRefName",
        ],
        main_s,
    )?;
    let mut rows = Vec::new();
    for wt in &others {
        let path = wt.path.to_string_lossy().into_owned();
        let dirty = !git(&["status", "--porcelain"], &path)
            .unwrap_or_default()
            .trim()
            .is_empty();
        let iid = worktree::issue_id_of(&wt.branch, &wt.path);
        let pr = if wt.branch != "DETACHED" {
            best_pr(&prs, &wt.branch)
        } else {
            None
        };
        let (pr_number, pr_state, pr_url) = match pr {
            Some(p) => (Some(p.number), p.state.clone(), Some(p.url.clone())),
            None => (None, "NO_PR".to_string(), None),
        };
        rows.push(IssueWorktree {
            worktree: path,
            branch: wt.branch.clone(),
            issue_id: iid,
            dirty,
            pr_number,
            pr_state,
            pr_url,
            linear_kind: None,
            linear_name: None,
            finished: false,
            reason_not_finished: None,
        });
    }
    Ok(rows)
}

/// None when finished; otherwise a short reason it is not. With `pr_only`, the
/// Linear gate is dropped (finished = PR merged + clean).
pub fn reason_not_finished(wt: &IssueWorktree, has_key: bool, pr_only: bool) -> Option<String> {
    if wt.issue_id == "UNKNOWN" {
        return Some("not an issue worktree".into());
    }
    let mut bits: Vec<String> = Vec::new();
    if wt.pr_state != "MERGED" {
        bits.push(if wt.pr_state != "NO_PR" {
            "PR not merged".into()
        } else {
            "no PR".into()
        });
    }
    if !pr_only {
        match wt.linear_kind.as_deref() {
            None => bits.push(if has_key {
                "Linear unknown".into()
            } else {
                "no Linear key".into()
            }),
            Some(kind) if kind != "completed" => {
                bits.push(format!("Linear {}", wt.linear_name.as_deref().unwrap_or("")))
            }
            _ => {}
        }
    }
    if wt.dirty {
        bits.push("dirty".into());
    }
    if bits.is_empty() {
        None
    } else {
        Some(bits.join(", "))
    }
}

/// Discover worktrees, attach Linear state, and compute the finished verdict.
pub fn gather(start: &str, ids: &[String]) -> Result<StatusReport> {
    let mut rows = build_rows(start)?;
    let key = std::env::var("LINEAR_API_KEY").ok();
    let has_key = key.is_some();
    if rows.is_empty() {
        return Ok(StatusReport {
            worktrees: Vec::new(),
            finished_count: 0,
            has_linear_key: has_key,
            linear_workspace: None,
        });
    }
    if !ids.is_empty() {
        let wanted: Vec<String> = ids.iter().map(|s| s.to_uppercase()).collect();
        rows.retain(|r| wanted.contains(&r.issue_id));
    }
    let issue_ids: Vec<String> = rows
        .iter()
        .filter(|r| r.issue_id != "UNKNOWN")
        .map(|r| r.issue_id.clone())
        .collect();
    let states = linear::states(&issue_ids, key.as_deref());
    let linear_workspace = linear::workspace_url_key();
    let mut finished_count = 0;
    for wt in &mut rows {
        if let Some(st) = states.get(&wt.issue_id) {
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
    Ok(StatusReport {
        worktrees: rows,
        finished_count,
        has_linear_key: has_key,
        linear_workspace,
    })
}
```

- [ ] **Step 5: Move the unit tests into the facade and add the new verdict test**

Append to `crates/devkit-issue/src/status.rs`. The `best_pr` tests move verbatim. The `reason_not_finished` tests are adapted to construct `IssueWorktree` and use the new signature, plus a new table-driven test covering the verdict combinations.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn pr(n: u64, state: &str, head: &str) -> Pr {
        Pr {
            number: n,
            state: state.into(),
            url: format!("https://x/{n}"),
            head_ref_name: head.into(),
        }
    }

    fn wt(issue_id: &str, pr_state: &str, dirty: bool, linear_kind: Option<&str>) -> IssueWorktree {
        IssueWorktree {
            worktree: "/w".into(),
            branch: "b".into(),
            issue_id: issue_id.into(),
            dirty,
            pr_number: Some(1),
            pr_state: pr_state.into(),
            pr_url: None,
            linear_kind: linear_kind.map(String::from),
            linear_name: linear_kind.map(|_| "Done".to_string()),
            finished: false,
            reason_not_finished: None,
        }
    }

    #[test]
    fn best_pr_prefers_merged_over_open() {
        let prs = vec![
            pr(1, "OPEN", "feat"),
            pr(2, "MERGED", "feat"),
            pr(3, "CLOSED", "feat"),
        ];
        assert_eq!(best_pr(&prs, "feat").unwrap().number, 2);
    }

    #[test]
    fn best_pr_higher_number_within_same_state() {
        let prs = vec![pr(5, "OPEN", "feat"), pr(9, "OPEN", "feat")];
        assert_eq!(best_pr(&prs, "feat").unwrap().number, 9);
    }

    #[test]
    fn best_pr_none_for_unknown_head() {
        let prs = vec![pr(1, "MERGED", "feat")];
        assert!(best_pr(&prs, "other").is_none());
    }

    #[test]
    fn finished_when_merged_done_clean() {
        assert!(reason_not_finished(&wt("ENG-1", "MERGED", false, Some("completed")), true, false).is_none());
    }

    #[test]
    fn not_finished_when_dirty() {
        assert_eq!(
            reason_not_finished(&wt("ENG-1", "MERGED", true, Some("completed")), true, false).as_deref(),
            Some("dirty")
        );
    }

    #[test]
    fn pr_only_ignores_linear() {
        // No Linear entry, no key, but pr_only drops the Linear gate.
        assert!(reason_not_finished(&wt("ENG-1", "MERGED", false, None), false, true).is_none());
    }

    #[test]
    fn verdict_combinations() {
        // Unknown id is never an issue worktree.
        assert_eq!(
            reason_not_finished(&wt("UNKNOWN", "MERGED", false, Some("completed")), true, false).as_deref(),
            Some("not an issue worktree")
        );
        // No PR + no Linear key, all reasons join with ", ".
        assert_eq!(
            reason_not_finished(&wt("ENG-2", "NO_PR", false, None), false, false).as_deref(),
            Some("no PR, no Linear key")
        );
        // Open PR + started Linear + dirty.
        assert_eq!(
            reason_not_finished(&wt("ENG-3", "OPEN", true, Some("started")), true, false).as_deref(),
            Some("PR not merged, Linear Done, dirty")
        );
        // Has key but no Linear entry → "Linear unknown".
        assert_eq!(
            reason_not_finished(&wt("ENG-4", "MERGED", false, None), true, false).as_deref(),
            Some("Linear unknown")
        );
    }
}
```

- [ ] **Step 6: Run the facade tests — expect PASS**

Run: `cargo test -p devkit-issue`
Expected: all tests pass (the crate compiles and the moved + new tests are green).

- [ ] **Step 7: Refactor `src/bin/issue/triage.rs` to render-only over the facade structs**

Replace the **entire** file contents. Gathering, `Pr`/`best_pr`, `reason_not_finished`, and all tests are gone (they live in the crate). What remains is the table renderer, rewritten to read `IssueWorktree`/`StatusReport`, returning the report's `finished_count`.

```rust
use devkit_common::ui;
use devkit_issue::status::{IssueWorktree, StatusReport};

fn pr_label(row: &IssueWorktree) -> String {
    if row.pr_state == "NO_PR" {
        "no PR".into()
    } else {
        format!("{} #{}", row.pr_state, row.pr_number.unwrap_or(0))
    }
}

/// Branch is secondary — the issue id identifies the worktree — so cap it with
/// an ellipsis, letting the PR/LINEAR/VERDICT columns survive a narrow terminal.
const BRANCH_MAX: usize = 46;

pub(crate) fn render(report: &StatusReport) -> usize {
    println!("{}", ui::bold_cyan("ISSUE WORKTREES"));
    if report.worktrees.is_empty() {
        println!("  {}", ui::dim("(none)"));
        return 0;
    }
    let mut sorted: Vec<&IssueWorktree> = report.worktrees.iter().collect();
    sorted.sort_by(|a, b| a.issue_id.cmp(&b.issue_id));
    let mut t = ui::table(&["ISSUE", "BRANCH", "TREE", "PR", "LINEAR", "VERDICT"]);
    for row in sorted {
        let verdict_disp = if row.finished {
            ui::bold_green("FINISHED")
        } else {
            // The only "ball in your court" reason is a dirty tree; flag it
            // yellow, leave the rest (waiting on PR/Linear) dim.
            match row.reason_not_finished.as_deref() {
                Some(r) if r.contains("dirty") => ui::yellow(r),
                Some(r) => ui::dim(r),
                None => ui::dim(""),
            }
        };
        let issue_disp = {
            let linked = match report.linear_workspace.as_deref() {
                Some(k) if row.linear_kind.is_some() => ui::link(
                    &row.issue_id,
                    &format!("https://linear.app/{k}/issue/{}", row.issue_id),
                ),
                _ => row.issue_id.clone(),
            };
            if row.issue_id == "UNKNOWN" {
                ui::dim(&linked)
            } else {
                ui::cyan(&linked)
            }
        };
        let pr_disp = {
            let label = pr_label(row);
            let colored = match row.pr_state.as_str() {
                "MERGED" => ui::green(&label),
                "OPEN" => ui::yellow(&label),
                "CLOSED" => ui::red(&label),
                _ => ui::dim(&label), // NO_PR
            };
            match &row.pr_url {
                Some(u) => ui::link(&colored, u),
                None => colored,
            }
        };
        let linear_disp = match row.linear_kind.as_deref() {
            None => ui::dim(if report.has_linear_key { "unknown" } else { "no key" }),
            Some(kind) => {
                let name = row.linear_name.as_deref().unwrap_or("");
                match kind {
                    "completed" => ui::green(name),
                    "started" => ui::yellow(name),
                    "canceled" => ui::red(name),
                    _ => ui::dim(name),
                }
            }
        };
        let tree_disp = if row.dirty {
            ui::red("dirty")
        } else {
            ui::dim("clean")
        };
        t.add_row(vec![
            issue_disp,
            ui::dim(&ui::truncate(&row.branch, BRANCH_MAX)),
            tree_disp,
            pr_disp,
            linear_disp,
            verdict_disp,
        ]);
    }
    println!("{t}");
    report.finished_count
}
```

- [ ] **Step 8: Refactor `src/bin/issue/status.rs` to call the facade**

Replace the **entire** file:

```rust
use crate::triage::render;
use anyhow::Result;
use devkit_common::ui;

pub fn run(start: &str, ids: &[String]) -> Result<()> {
    let pb = crate::spin::spinner("Discovering worktrees…");
    let report = devkit_issue::status::gather(start, ids)?;
    pb.finish_and_clear();
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

- [ ] **Step 9: Repoint `src/bin/issue/end.rs` to the facade**

`end.rs` is out of scope for behavior, but it shares the gatherer, so it must move with it. Change only the data plumbing — `cleanup`, `confirm`, and `select_explicit`'s body stay the same except the row type. Apply these edits:

Replace the import block at the top (lines 1-6):

```rust
use anyhow::{Context, Result};
use devkit_common::cmd::git;
use std::io::{self, Write};
use std::path::Path;

use crate::triage::render;
use devkit_issue::status::{IssueWorktree, gather, reason_not_finished};
```

Change `select_explicit`'s signature and the `Row` references inside it from `Row` to `IssueWorktree`:

```rust
fn select_explicit(rows: &[IssueWorktree], selectors: &[String]) -> Vec<IssueWorktree> {
```
(the `hits: Vec<&Row>` binding inside becomes `hits: Vec<&IssueWorktree>`; the body is otherwise unchanged.)

In `run`, replace the `clean_worktree` branch's gather/render/select (the current lines 147-163 region):

```rust
    let targets: Vec<IssueWorktree> = if clean_worktree {
        anyhow::ensure!(
            !ids.is_empty(),
            "--clean-worktree needs one or more selectors (issue id, branch, or worktree path)"
        );
        let report = gather(start, &[])?;
        render(&report);
        let t = select_explicit(&report.worktrees, ids);
        if t.is_empty() {
            println!("\nNo matching worktrees.");
            return Ok(());
        }
        println!(
            "\n--clean-worktree: removing {} selected worktree(s), ignoring the PR/Linear/finished gate.",
            t.len()
        );
        t
    } else {
        let report = gather(start, ids)?;
        render(&report);
        if pr_only {
            println!("--pr-only: Linear 'Done' gate skipped.");
        }
        let t: Vec<IssueWorktree> = report
            .worktrees
            .iter()
            .filter(|r| reason_not_finished(r, report.has_linear_key, pr_only).is_none())
            .cloned()
            .collect();
        if t.is_empty() {
            println!("\nNothing finished to clean up.");
            return Ok(());
        }
        println!("\n{} worktree(s) ready to remove:", t.len());
        t
    };
```

The rest of `run` (the `for row in &targets` loop, `cleanup`, `confirm`, `CleanupError`) is unchanged.

- [ ] **Step 10: Run the full gate — expect PASS**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`
Expected: all tests pass, zero clippy warnings, formatting applied.

- [ ] **Step 11: Commit**

```bash
git add Cargo.toml crates/devkit-issue src/bin/issue/triage.rs src/bin/issue/status.rs src/bin/issue/end.rs
git commit
```

Commit message:
```
feat(issue): extract status gathering into devkit-issue

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
```

---

### Task 2: Extract the `prs` facade

Moves the `gh` PR fetching and the (already pure) PR mappers into `devkit-issue::prs`, returning serializable view structs. Refactors the binary `prs.rs` to call the facade and keep only rendering + the diff cache.

**Files:**
- Modify: `crates/devkit-issue/src/prs.rs` (fill the stub from Task 1)
- Modify: `src/bin/issue/prs.rs` (render + cache only)

**Interfaces:**
- Consumes: nothing from Task 1 (independent module in the same crate).
- Produces (consumed by Task 3's MCP handler and the binary `prs.rs`):
  ```rust
  // devkit_issue::prs
  #[derive(Debug, Clone, serde::Serialize)]
  pub struct MinePrView { pub number: u64, pub url: String, pub issue_id: String,
                          pub review_state: String, pub check_state: String, pub action: String }
  #[derive(Debug, Clone, serde::Serialize)]
  pub struct ReviewPrView { pub number: u64, pub url: String, pub author: String,
                            pub my_vote: String, pub action: String }
  #[derive(Debug, Clone, serde::Serialize)]
  pub struct PrsReport { pub mine: Vec<MinePrView>, pub reviews: Vec<ReviewPrView> }
  pub fn gather(root: &str, mine: bool, reviews: bool, repo: Option<&str>) -> anyhow::Result<PrsReport>;
  pub fn resolve_repo(repo: Option<&str>, cwd: &str) -> anyhow::Result<String>;
  ```

- [ ] **Step 1: Write `crates/devkit-issue/src/prs.rs`**

Replace the stub. The gh JSON shapes and the pure mappers move verbatim from `src/bin/issue/prs.rs` (lines 8-195: `Check`, `Author`, `Review`, `ReviewRequest`, `MinePr`, `ReviewPr`, `BOTS`/`FAIL`/`RUNNING`, `is_bot`, `issue_of`, `checks_of`, `review_text`, `has_replied`, `mine_action`, `reviewer_state`). `Me` and `RepoInfo` (lines 257-265) and `resolve_repo`/`fetch_mine`/`fetch_reviews` (lines 267-321) move too, retyped to take an explicit `cwd` and call `devkit_common::cmd::gh_json` directly. `paint_action`, `diff_cell`, `issue_cell`, the cache, and the tables stay in the binary.

```rust
use anyhow::Result;
use devkit_common::cmd::gh_json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// gh JSON shapes ----------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct Check {
    conclusion: Option<String>,
    status: Option<String>,
    state: Option<String>,
}

#[derive(Deserialize)]
struct Author {
    login: String,
}

#[derive(Deserialize)]
struct Review {
    author: Author,
    #[serde(default)]
    state: String,
    #[serde(rename = "submittedAt", default)]
    submitted_at: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ReviewRequest {
    login: String,
}

#[derive(Deserialize)]
struct MinePr {
    number: u64,
    url: String,
    #[serde(rename = "headRefName", default)]
    head_ref_name: String,
    #[serde(rename = "isDraft", default)]
    is_draft: bool,
    #[serde(rename = "reviewDecision", default)]
    review_decision: Option<String>,
    #[serde(default)]
    mergeable: String,
    #[serde(rename = "statusCheckRollup", default)]
    status_check_rollup: Vec<Check>,
    #[serde(default)]
    reviews: Vec<Review>,
}

#[derive(Deserialize)]
struct ReviewPr {
    number: u64,
    url: String,
    author: Author,
    #[serde(rename = "latestReviews", default)]
    latest_reviews: Vec<Review>,
    #[serde(rename = "reviewRequests", default)]
    review_requests: Vec<ReviewRequest>,
}

#[derive(Deserialize)]
struct Me {
    login: String,
}

#[derive(Deserialize)]
struct RepoInfo {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

// pure logic --------------------------------------------------------------------

const BOTS: [&str; 3] = ["greptile-apps", "linear-code", "coderabbitai"];
const FAIL: [&str; 4] = ["FAILURE", "ERROR", "TIMED_OUT", "CANCELLED"];
const RUNNING: [&str; 3] = ["IN_PROGRESS", "QUEUED", "PENDING"];

fn is_bot(login: &str) -> bool {
    BOTS.contains(&login) || login.ends_with("[bot]")
}

/// The issue id a PR addresses, taken from its branch (head) ref and uppercased.
fn issue_of(head: &str) -> String {
    devkit_common::worktree::find_id(head)
        .map(|s| s.to_uppercase())
        .unwrap_or_else(|| "-".to_string())
}

fn checks_of(rollup: &[Check]) -> &'static str {
    if rollup.is_empty() {
        return "-";
    }
    let fail = rollup.iter().any(|c| {
        c.conclusion.as_deref().is_some_and(|x| FAIL.contains(&x))
            || matches!(c.state.as_deref(), Some("FAILURE") | Some("ERROR"))
    });
    if fail {
        return "fail";
    }
    let run = rollup.iter().any(|c| {
        c.status.as_deref().is_some_and(|x| RUNNING.contains(&x))
            || c.state.as_deref() == Some("PENDING")
    });
    if run {
        return "run";
    }
    "ok"
}

fn review_text(pr: &MinePr) -> &'static str {
    match pr.review_decision.as_deref() {
        Some("CHANGES_REQUESTED") => "changes",
        Some("APPROVED") => "approved",
        Some("REVIEW_REQUIRED") => "awaiting",
        _ => {
            if pr.reviews.is_empty() {
                "awaiting"
            } else {
                "commented"
            }
        }
    }
}

/// True when my latest review is newer than the latest non-bot reviewer's.
fn has_replied(pr: &MinePr, me: &str) -> bool {
    let mine = pr
        .reviews
        .iter()
        .filter(|r| r.author.login == me)
        .map(|r| r.submitted_at.as_str())
        .max()
        .unwrap_or("");
    let theirs = pr
        .reviews
        .iter()
        .filter(|r| r.author.login != me && !is_bot(&r.author.login))
        .map(|r| r.submitted_at.as_str())
        .max()
        .unwrap_or("");
    !mine.is_empty() && mine > theirs
}

fn mine_action(pr: &MinePr, me: &str) -> String {
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
            } else if checks_of(&pr.status_check_rollup) == "fail" {
                "fix CI -> merge".into()
            } else {
                "MERGE".into()
            }
        }
        _ => format!("awaiting review{}", if conflict { "; rebase" } else { "" }),
    }
}

/// (my_vote, action) for a PR where I'm a reviewer.
fn reviewer_state(pr: &ReviewPr, me: &str) -> (String, String) {
    let vote = pr
        .latest_reviews
        .iter()
        .filter(|r| r.author.login == me)
        .map(|r| r.state.clone())
        .next_back()
        .unwrap_or_default();
    let requested = pr.review_requests.iter().any(|req| req.login == me);
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

// gh fetches --------------------------------------------------------------------

/// Resolve `owner/name`. Returns `repo` as-is when given, else asks `gh`.
pub fn resolve_repo(repo: Option<&str>, cwd: &str) -> Result<String> {
    if let Some(r) = repo {
        return Ok(r.to_string());
    }
    let info: RepoInfo = gh_json(&["repo", "view", "--json", "nameWithOwner"], cwd)?;
    Ok(info.name_with_owner)
}

fn fetch_mine(repo: Option<&str>, cwd: &str) -> Result<Vec<MinePr>> {
    let mut args: Vec<String> = vec!["pr".into(), "list".into()];
    if let Some(r) = repo {
        args.push("--repo".into());
        args.push(r.into());
    }
    for a in [
        "--author",
        "@me",
        "--state",
        "open",
        "--limit",
        "100",
        "--json",
        "number,url,headRefName,isDraft,reviewDecision,mergeable,statusCheckRollup,reviews",
    ] {
        args.push(a.into());
    }
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    gh_json(&refs, cwd)
}

fn fetch_reviews(repo: Option<&str>, me: &str, cwd: &str) -> Result<Vec<ReviewPr>> {
    let fields = "number,url,headRefName,author,latestReviews,reviewRequests";
    let mut seen: BTreeMap<u64, ReviewPr> = BTreeMap::new();
    for search in ["review-requested:@me", "reviewed-by:@me"] {
        let mut args: Vec<String> = vec!["pr".into(), "list".into()];
        if let Some(r) = repo {
            args.push("--repo".into());
            args.push(r.into());
        }
        for a in [
            "--state", "open", "--limit", "100", "--search", search, "--json", fields,
        ] {
            args.push(a.into());
        }
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let batch: Vec<ReviewPr> = gh_json(&refs, cwd)?;
        for pr in batch {
            seen.entry(pr.number).or_insert(pr);
        }
    }
    Ok(seen
        .into_values()
        .filter(|pr| pr.author.login != me)
        .collect())
}

// views + gather ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct MinePrView {
    pub number: u64,
    pub url: String,
    pub issue_id: String,
    pub review_state: String,
    pub check_state: String,
    pub action: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewPrView {
    pub number: u64,
    pub url: String,
    pub author: String,
    pub my_vote: String,
    pub action: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrsReport {
    pub mine: Vec<MinePrView>,
    pub reviews: Vec<ReviewPrView>,
}

/// Fetch and classify the caller's PRs. Neither flag set ⇒ both groups.
/// Stateless: no diff cache is read or written.
pub fn gather(root: &str, mine: bool, reviews: bool, repo: Option<&str>) -> Result<PrsReport> {
    let want_mine = mine || !reviews;
    let want_reviews = reviews || !mine;

    // Independent gh round-trips run concurrently; the reviews fetch needs the
    // current user, so it shares a thread with the user lookup.
    let (me, mine_prs, review_rows) = std::thread::scope(|s| {
        let user_reviews = s.spawn(|| -> Result<(String, Vec<ReviewPr>)> {
            let me: Me = gh_json(&["api", "user"], root)?;
            let me = me.login;
            let rows = if want_reviews {
                fetch_reviews(repo, &me, root)?
            } else {
                vec![]
            };
            Ok((me, rows))
        });
        let mine_thread = s.spawn(|| -> Result<Vec<MinePr>> {
            if want_mine {
                fetch_mine(repo, root)
            } else {
                Ok(vec![])
            }
        });
        let (me, review_rows) = user_reviews.join().expect("user/reviews thread panicked")?;
        let mine_prs = mine_thread.join().expect("mine thread panicked")?;
        Ok::<_, anyhow::Error>((me, mine_prs, review_rows))
    })?;

    let mine_views: Vec<MinePrView> = mine_prs
        .iter()
        .map(|pr| MinePrView {
            number: pr.number,
            url: pr.url.clone(),
            issue_id: issue_of(&pr.head_ref_name),
            review_state: review_text(pr).to_string(),
            check_state: checks_of(&pr.status_check_rollup).to_string(),
            action: mine_action(pr, &me),
        })
        .collect();

    let mut sorted: Vec<&ReviewPr> = review_rows.iter().collect();
    sorted.sort_by_key(|p| p.number);
    let review_views: Vec<ReviewPrView> = sorted
        .iter()
        .map(|pr| {
            let (my_vote, action) = reviewer_state(pr, &me);
            ReviewPrView {
                number: pr.number,
                url: pr.url.clone(),
                author: pr.author.login.clone(),
                my_vote,
                action,
            }
        })
        .collect();

    Ok(PrsReport {
        mine: mine_views,
        reviews: review_views,
    })
}
```

- [ ] **Step 2: Move the pure-mapper unit tests into the facade**

Append to `crates/devkit-issue/src/prs.rs`. These move verbatim from `src/bin/issue/prs.rs` (lines 495-633): `checks_fail_run_ok_empty`, `approved_green_merges`, `approved_with_failing_ci`, `changes_requested_action`, `draft_action`, `review_text_variants`, `reviewer_state_requested_needs_review`, `reviewer_state_approved_done`, `issue_of_finds_swe`, `issue_of_finds_non_swe_prefix`, with their `mine`/`check` helpers. The `diff_cell_shows_change` test does **not** move (it stays in the binary with `diff_cell`).

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn mine(decision: Option<&str>, mergeable: &str, draft: bool, checks: Vec<Check>) -> MinePr {
        MinePr {
            number: 1,
            url: "u".into(),
            head_ref_name: "h".into(),
            is_draft: draft,
            review_decision: decision.map(String::from),
            mergeable: mergeable.into(),
            status_check_rollup: checks,
            reviews: vec![],
        }
    }
    fn check(conclusion: Option<&str>, status: Option<&str>) -> Check {
        Check {
            conclusion: conclusion.map(String::from),
            status: status.map(String::from),
            state: None,
        }
    }
    #[test]
    fn checks_fail_run_ok_empty() {
        assert_eq!(checks_of(&[]), "-");
        assert_eq!(checks_of(&[check(Some("SUCCESS"), Some("COMPLETED"))]), "ok");
        assert_eq!(checks_of(&[check(Some("FAILURE"), Some("COMPLETED"))]), "fail");
        assert_eq!(checks_of(&[check(None, Some("IN_PROGRESS"))]), "run");
    }
    #[test]
    fn approved_green_merges() {
        assert_eq!(
            mine_action(&mine(Some("APPROVED"), "MERGEABLE", false, vec![check(Some("SUCCESS"), None)]), "me"),
            "MERGE"
        );
    }
    #[test]
    fn approved_with_failing_ci() {
        assert_eq!(
            mine_action(&mine(Some("APPROVED"), "MERGEABLE", false, vec![check(Some("FAILURE"), None)]), "me"),
            "fix CI -> merge"
        );
    }
    #[test]
    fn changes_requested_action() {
        assert_eq!(
            mine_action(&mine(Some("CHANGES_REQUESTED"), "MERGEABLE", false, vec![]), "me"),
            "address changes"
        );
    }
    #[test]
    fn draft_action() {
        assert_eq!(mine_action(&mine(None, "MERGEABLE", true, vec![]), "me"), "draft");
    }
    #[test]
    fn review_text_variants() {
        assert_eq!(review_text(&mine(Some("APPROVED"), "x", false, vec![])), "approved");
        assert_eq!(review_text(&mine(Some("CHANGES_REQUESTED"), "x", false, vec![])), "changes");
        assert_eq!(review_text(&mine(None, "x", false, vec![])), "awaiting");
    }
    #[test]
    fn reviewer_state_requested_needs_review() {
        let pr = ReviewPr {
            number: 1,
            url: "u".into(),
            author: Author { login: "other".into() },
            latest_reviews: vec![],
            review_requests: vec![ReviewRequest { login: "me".into() }],
        };
        let (vote, action) = reviewer_state(&pr, "me");
        assert_eq!(vote, "-");
        assert_eq!(action, "REVIEW NEEDED");
    }
    #[test]
    fn reviewer_state_approved_done() {
        let pr = ReviewPr {
            number: 1,
            url: "u".into(),
            author: Author { login: "other".into() },
            latest_reviews: vec![Review {
                author: Author { login: "me".into() },
                state: "APPROVED".into(),
                submitted_at: "".into(),
            }],
            review_requests: vec![],
        };
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

- [ ] **Step 3: Run the facade tests — expect PASS**

Run: `cargo test -p devkit-issue`
Expected: the moved mapper tests pass alongside Task 1's status tests.

- [ ] **Step 4: Refactor `src/bin/issue/prs.rs` to render + cache only**

Replace the **entire** file. Everything above the rendering section (gh shapes, mappers, fetches) is gone — it lives in the facade. The renderer now consumes `MinePrView`/`ReviewPrView`. `paint_action`, `diff_cell`, `issue_cell`, the cache, and the legend stay. `resolve_repo` for the cache key comes from the facade.

```rust
use anyhow::Result;
use devkit_common::{paths, ui};
use devkit_issue::prs::{MinePrView, ReviewPrView};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// rendering ---------------------------------------------------------------------

/// Colour an ACTION value by whose turn it is: green = ready to land, red =
/// needs you, yellow = waiting on the author, dim = passive. Mirrors the verbs
/// produced by `mine_action`/`reviewer_state`.
fn paint_action(action: &str, s: &str) -> String {
    if action.starts_with("MERGE")
        || action.starts_with("rebase -> merge")
        || action.starts_with("done")
    {
        ui::green(s)
    } else if action.starts_with("address")
        || action.starts_with("fix")
        || action.starts_with("REVIEW NEEDED")
    {
        ui::red(s)
    } else if action.starts_with("awaiting author") {
        ui::yellow(s)
    } else {
        ui::dim(s)
    }
}

/// Render `cur` through `paint`. When it differs from the cached `prev`, prefix
/// the struck-through old value and a dim arrow so the change reads at a glance.
fn diff_cell(prev: Option<&str>, cur: &str, paint: impl Fn(&str) -> String) -> String {
    match prev {
        Some(p) if p != cur => format!("{}{}{}", ui::dim_strike(p), ui::dim(" → "), paint(cur)),
        _ => paint(cur),
    }
}

fn issue_cell(issue_id: &str, url_key: Option<&str>) -> String {
    if issue_id == "-" {
        return ui::dim("-");
    }
    let linked = match url_key {
        Some(k) => ui::link(issue_id, &format!("https://linear.app/{k}/issue/{issue_id}")),
        None => issue_id.to_string(),
    };
    ui::cyan(&linked)
}

// diff cache --------------------------------------------------------------------

type Snap = BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>;

fn cache_path(repo: &str) -> PathBuf {
    paths::cache_dir()
        .join("pr-status")
        .join(format!("{}.json", repo.replace('/', "_")))
}
fn load_cache(path: &Path) -> Snap {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}
fn save_cache(path: &Path, data: &Snap) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(data)?)?;
    Ok(())
}

fn mine_table(
    prs: &[MinePrView],
    url_key: Option<&str>,
    prev: &BTreeMap<String, BTreeMap<String, String>>,
) -> BTreeMap<String, BTreeMap<String, String>> {
    println!("{}", ui::bold_cyan("MY OPEN PRs"));
    let mut cur = BTreeMap::new();
    if prs.is_empty() {
        println!("  {}", ui::dim("(none)"));
        return cur;
    }
    let mut t = ui::table(&["PR", "ISSUE", "REVIEW", "CHECK", "ACTION"]);
    for pr in prs {
        let review = pr.review_state.clone();
        let check = pr.check_state.clone();
        let action = pr.action.clone();
        let was = prev.get(&pr.number.to_string());
        let g = |k: &str| was.and_then(|m| m.get(k)).map(|s| s.as_str());
        t.add_row(vec![
            ui::link(&format!("#{}", pr.number), &pr.url),
            issue_cell(&pr.issue_id, url_key),
            diff_cell(g("review"), &review, |s| s.to_string()),
            diff_cell(g("check"), &check, |s| s.to_string()),
            diff_cell(g("action"), &action, |s| paint_action(&action, s)),
        ]);
        cur.insert(
            pr.number.to_string(),
            BTreeMap::from([
                ("review".to_string(), review),
                ("check".to_string(), check),
                ("action".to_string(), action),
            ]),
        );
    }
    println!("{t}");
    cur
}

fn reviews_table(
    rows: &[ReviewPrView],
    prev: &BTreeMap<String, BTreeMap<String, String>>,
) -> BTreeMap<String, BTreeMap<String, String>> {
    println!("\n{}", ui::bold_cyan("PRs AWAITING MY REVIEW"));
    let mut cur = BTreeMap::new();
    if rows.is_empty() {
        println!("  {}", ui::dim("(none)"));
        return cur;
    }
    let mut t = ui::table(&["PR", "AUTHOR", "MY VOTE", "ACTION"]);
    for pr in rows {
        let vote = pr.my_vote.clone();
        let action = pr.action.clone();
        let was = prev.get(&pr.number.to_string());
        let g = |k: &str| was.and_then(|m| m.get(k)).map(|s| s.as_str());
        t.add_row(vec![
            ui::link(&format!("#{}", pr.number), &pr.url),
            pr.author.clone(),
            diff_cell(g("vote"), &vote, |s| s.to_string()),
            diff_cell(g("action"), &action, |s| paint_action(&action, s)),
        ]);
        cur.insert(
            pr.number.to_string(),
            BTreeMap::from([("vote".to_string(), vote), ("action".to_string(), action)]),
        );
    }
    println!("{t}");
    cur
}

// Entry point -------------------------------------------------------------------

pub fn run(mine: bool, reviews: bool, repo: Option<String>, no_cache: bool) -> Result<()> {
    let want_mine = mine || !reviews;
    let want_reviews = reviews || !mine;

    let pb = crate::spin::spinner("Fetching PRs from GitHub…");
    let url_key = devkit_common::linear::workspace_url_key();
    let report = devkit_issue::prs::gather(".", mine, reviews, repo.as_deref())?;
    let repo_key = if no_cache {
        None
    } else {
        Some(devkit_issue::prs::resolve_repo(repo.as_deref(), ".")?)
    };
    pb.finish_and_clear();

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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diff_cell_shows_change() {
        // Tests are not a tty, so colour/strike helpers pass text through and the
        // change reads as a plain `old → new`.
        let plain = |s: &str| s.to_string();
        assert_eq!(diff_cell(Some("ok"), "fail", plain), "ok → fail");
        assert_eq!(diff_cell(Some("ok"), "ok", plain), "ok");
        assert_eq!(diff_cell(None, "ok", plain), "ok");
    }
}
```

- [ ] **Step 5: Run the full gate — expect PASS**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`
Expected: all tests pass, zero warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-issue/src/prs.rs src/bin/issue/prs.rs
git commit
```

Commit message:
```
feat(issue): extract pr triage into devkit-issue

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
```

---

### Task 3: Add the MCP `issue.status` and `issue.prs` actions

Registers the two actions in the MCP server, calling the facade. Adds integration tests for the deterministic paths.

**Files:**
- Create: `crates/devkit-mcp/src/issue.rs`
- Modify: `crates/devkit-mcp/src/lib.rs` (add `mod issue;`)
- Modify: `crates/devkit-mcp/src/actions.rs` (extend the registry)
- Modify: `crates/devkit-mcp/Cargo.toml` (add `devkit-issue` dependency)
- Modify: `tests/mcp.rs` (integration tests)

**Interfaces:**
- Consumes: `devkit_issue::status::gather`, `devkit_issue::prs::gather` (Tasks 1-2).
- Produces: actions `issue.status` and `issue.prs` in the registry; `issue.status` returns a serialized `StatusReport` object, `issue.prs` returns a serialized `PrsReport` object.

- [ ] **Step 1: Add the crate dependency**

In `crates/devkit-mcp/Cargo.toml`, under `[dependencies]` after `devkit-locks.workspace = true`, add:

```toml
devkit-issue.workspace = true
```

- [ ] **Step 2: Write the failing integration tests**

Append to `tests/mcp.rs`. First add a helper that makes a real git repo (so `git worktree list` succeeds and, with no extra worktrees, `gather` returns early without needing `gh`), then a describe test and a status test.

Add this helper after `project_with_config` (around line 43):

```rust
/// A real (empty) git repo so `git worktree list` resolves with only the main
/// worktree — `issue.status` then returns empty without needing `gh`.
fn git_repo() -> PathBuf {
    let p = scratch("repo");
    let ok = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&p)
        .status()
        .expect("spawn git init")
        .success();
    assert!(ok, "git init failed");
    p
}
```

Add the tests at the end of the file:

```rust
#[test]
fn issue_actions_are_described() {
    let proj = project();
    let state = scratch("state");
    let resps = mcp(
        &proj,
        &state,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "devkit_describe", "arguments": {} }
        })],
    );
    let text = resps[0]["result"]["content"][0]["text"].as_str().unwrap();
    let list: Value = serde_json::from_str(text).unwrap();
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"issue.status"), "issue.status is described");
    assert!(names.contains(&"issue.prs"), "issue.prs is described");
}

#[test]
fn issue_status_empty_for_repo_with_no_worktrees() {
    let proj = git_repo();
    let state = scratch("state");
    let root = proj.to_str().unwrap();
    let resps = mcp(
        &proj,
        &state,
        &[call_req(1, "issue.status", json!({ "root": root }))],
    );
    let report = tool_json(&resps[0], false);
    assert!(
        report["worktrees"].as_array().unwrap().is_empty(),
        "no non-main worktrees → empty list"
    );
    assert_eq!(report["finished_count"], 0);
}
```

- [ ] **Step 3: Run the new tests — expect FAIL**

Run: `cargo test --test mcp issue_`
Expected: FAIL — `issue.status`/`issue.prs` are unknown actions (`issue_actions_are_described` fails the `contains` assertions; `issue_status_empty_…` gets an `isError` "unknown action" payload, failing `tool_json(.., false)`).

- [ ] **Step 4: Write `crates/devkit-mcp/src/issue.rs`**

```rust
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use devkit_issue::{prs, status};

use crate::ServerCtx;
use crate::actions::Action;

pub fn actions() -> Vec<Action> {
    vec![
        Action {
            name: "issue.status",
            summary: "List issue worktrees (optionally filtered by id) with PR/Linear state and a finished verdict.",
            schema: status_schema,
            handler: status,
        },
        Action {
            name: "issue.prs",
            summary: "Triage your GitHub PRs: the ones you authored and the ones awaiting your review.",
            schema: prs_schema,
            handler: prs_handler,
        },
    ]
}

#[derive(Deserialize)]
struct StatusArgs {
    #[serde(default)]
    root: Option<String>,
    #[serde(default)]
    ids: Vec<String>,
}

fn status_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Directory whose worktrees are enumerated (default \".\")." },
            "ids": { "type": "array", "items": { "type": "string" }, "description": "Filter to these issue ids (case-insensitive)." }
        },
        "additionalProperties": false
    })
}

fn status(_ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: StatusArgs = serde_json::from_value(args).context("invalid issue.status arguments")?;
    let root = a.root.unwrap_or_else(|| ".".to_string());
    let report = status::gather(&root, &a.ids)?;
    Ok(serde_json::to_value(report)?)
}

#[derive(Deserialize)]
struct PrsArgs {
    #[serde(default)]
    root: Option<String>,
    #[serde(default)]
    mine: bool,
    #[serde(default)]
    reviews: bool,
    #[serde(default)]
    repo: Option<String>,
}

fn prs_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Directory to run gh in (default \".\"); not the MCP server's CWD." },
            "mine": { "type": "boolean", "description": "Include PRs you authored. Neither flag set ⇒ both groups." },
            "reviews": { "type": "boolean", "description": "Include PRs awaiting your review. Neither flag set ⇒ both groups." },
            "repo": { "type": "string", "description": "owner/name to target instead of detecting from root." }
        },
        "additionalProperties": false
    })
}

fn prs_handler(_ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: PrsArgs = serde_json::from_value(args).context("invalid issue.prs arguments")?;
    let root = a.root.unwrap_or_else(|| ".".to_string());
    let report = prs::gather(&root, a.mine, a.reviews, a.repo.as_deref())?;
    Ok(serde_json::to_value(report)?)
}
```

- [ ] **Step 5: Register the module**

In `crates/devkit-mcp/src/lib.rs`, add `mod issue;` after `mod devrun;` (line 2):

```rust
mod actions;
mod devrun;
mod issue;
mod jsonrpc;
mod locks;
mod ports;
```

In `crates/devkit-mcp/src/actions.rs`, in `actions()`, after the `devrun` line (line 21), add:

```rust
    v.extend(crate::issue::actions());
```

- [ ] **Step 6: Run the tests — expect PASS**

Run: `cargo test --test mcp issue_`
Expected: PASS — both `issue_actions_are_described` and `issue_status_empty_for_repo_with_no_worktrees` pass.

- [ ] **Step 7: Run the full gate — expect PASS**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`
Expected: all tests pass (including the existing `describe_returns_a_schema_for_each_action`, which now also covers `issue.status`/`issue.prs`), zero warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/devkit-mcp/Cargo.toml crates/devkit-mcp/src/issue.rs crates/devkit-mcp/src/lib.rs crates/devkit-mcp/src/actions.rs tests/mcp.rs
git commit
```

Commit message:
```
feat(mcp): add issue.status and issue.prs actions

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
```

---

### Task 4: Documentation

Updates user- and contributor-facing docs to reflect the new crate and the shipped read-only `issue` actions.

**Files:**
- Modify: `README.md` (MCP actions section)
- Modify: `AGENTS.md` (layout table: new crate row + `devkit-mcp` row)
- Modify: `docs/next-steps.md` (flip the `issue` actions bullet)

- [ ] **Step 1: Update `README.md`**

Find the MCP section that documents the devrun actions (search for `devrun.status`). After the devrun actions paragraph, add a paragraph describing the read-only issue actions. Use this text:

```markdown
The MCP server also exposes two read-only `issue` actions: `issue.status` lists
the issue worktrees for a directory (`root`, default `.`; optional `ids` filter)
with each one's PR state, Linear state, and a finished/not-finished verdict;
`issue.prs` triages your GitHub PRs (`mine`, `reviews` — neither set means both;
optional `repo`). Both return structured JSON with the verdicts and next-action
labels pre-computed. They never mutate; `issue review`/`issue end` stay CLI-only.
```

- [ ] **Step 2: Update `AGENTS.md` layout table**

In the `## Layout` table, add a row for the new crate after the `crates/devkit-locks` row:

```markdown
| `crates/devkit-issue` | lib: read-only issue triage facade — `status` (worktree + PR + Linear state with the finished verdict) and `prs` (PR triage); serializable, no rendering, no mutations |
```

Update the `crates/devkit-mcp` row to mention the issue facade. Change it to:

```markdown
| `crates/devkit-mcp` | lib: stdio MCP server (`jsonrpc`, action `registry`, `ports`/`locks`/`devrun`/`issue` handlers) over the port + lock facades, the `devkit-ports::run` server-lifecycle facade, and the `devkit-issue` triage facade |
```

- [ ] **Step 3: Update `docs/next-steps.md`**

Find the bullet beginning `- **`issue` actions (deferred — needs library extraction first).**` and replace that entire bullet with:

```markdown
- **`issue` read actions (phase 3 — shipped, read-only).** `issue.status` and
  `issue.prs` are registered MCP actions over the new `devkit-issue` facade
  (`status::gather`, `prs::gather`). The `issue` binary was refactored to consume
  the facade. Still deferred: the mutating `issue.review` (push/PR/Slack) and
  `issue.end` (worktree removal) actions, which need confirm-gating; and
  `issue setup`/`issue dashboard`, which are not request/response fits.
```

- [ ] **Step 4: Run the gate (docs don't change code, but keep the workspace green)**

Run: `cargo test --workspace`
Expected: all tests pass (no code changed; this confirms nothing regressed).

- [ ] **Step 5: Commit**

```bash
git add README.md AGENTS.md docs/next-steps.md
git commit
```

Commit message:
```
docs: note shipped issue read mcp actions

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
```

---

## Unresolved questions

These do not block execution (each has a chosen default baked into the plan); flag if you disagree:

1. **`IssueWorktree` carries `linear_kind` + `linear_name` (two fields)** rather than the spec's single `linear_state: Option<String>`. The renderer needs the `kind` for colouring and the `name` for display, so both are exposed; the MCP JSON gains both. Acceptable, or collapse to one?
2. **`linear_workspace` is exposed in `StatusReport`** (the spec left this open). It's cheap and lets an agent build issue links. Kept in. Drop it if you'd rather keep the URL slug a rendering-only concern.
3. **`issue.prs` has no deterministic CI integration test** (it always calls `gh api user`/`gh pr list`; there is no offline path like `issue.status`'s no-worktree short-circuit). It's covered by the facade's pure-mapper unit tests + the schema-coverage test, matching the `devrun.up` precedent (whose happy path is also not integration-tested). Acceptable?
4. **The cache repo resolution (`resolve_repo`) now runs serially after `prs::gather`** in the CLI, rather than concurrently in a third thread as before. One extra serial `gh repo view` only when the cache is enabled (`!no_cache` and no explicit `repo`). Negligible; flag if you want it kept concurrent.
