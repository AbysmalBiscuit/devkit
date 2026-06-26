# CLI Progress Spinners Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `indicatif` progress spinners (TTY-only) to every user-facing CLI command that blocks on network/git/server work but currently prints nothing.

**Architecture:** Promote the existing `issue`-private `Steps` helper to `devkit-common` so all binaries share it, extend it with runtime auto-numbering and a `during(msg, closure)` wrapper that clears the bar before returning, then wire each blocking call site through it. Bars draw on stderr and are hidden off-TTY, so piped/MCP/CI/test output is unchanged.

**Tech Stack:** Rust 2024, `indicatif` 0.17, `anyhow`.

**Spec:** `docs/superpowers/specs/2026-06-26-cli-progress-spinners-design.md`

---

## File Structure

- **Create** `crates/devkit-common/src/progress.rs` — the shared `Steps` helper (moved from `src/bin/issue/spin.rs`, extended).
- **Modify** `crates/devkit-common/Cargo.toml` — add `indicatif` dependency.
- **Modify** `crates/devkit-common/src/lib.rs` — add `pub mod progress;`.
- **Delete** `src/bin/issue/spin.rs`; **Modify** `src/bin/issue/main.rs` (drop `mod spin;`), `prs.rs`, the status module(s), and `dashboard.rs` to import `devkit_common::progress::Steps`.
- **Modify** command handlers: `src/bin/issue/{checkout,setup,info,end,review}.rs`, `src/bin/devrun/main.rs`, `src/bin/devkit/{auth,doctor}.rs`.

**Numbering policy:** Commands with a clean, prompt-free, runtime-computable step count use `Steps::with_total(N)` + auto-numbered `during` (→ `setup`, `doctor`). Commands whose flow branches or interleaves an interactive stdin prompt use plain `Steps::new()` + descriptive `during` (→ `checkout-pr`, `info`, `end`, `review`, `devrun up`, `auth`) so a spinner never sits live across a prompt and numbering never goes stale.

---

## Task 1: Shared `Steps` helper in `devkit-common`

**Files:**
- Create: `crates/devkit-common/src/progress.rs`
- Modify: `crates/devkit-common/Cargo.toml`
- Modify: `crates/devkit-common/src/lib.rs:5` (add `pub mod progress;` after `pub mod paths;`)

- [ ] **Step 1: Add the `indicatif` dependency**

In `crates/devkit-common/Cargo.toml`, under `[dependencies]`, add (matching the workspace pin used elsewhere — `indicatif = "0.17"`):

```toml
indicatif = "0.17"
```

- [ ] **Step 2: Register the module**

In `crates/devkit-common/src/lib.rs`, add after the `pub mod paths;` line (keeping the alphabetical run `paths, progress, report`):

```rust
pub mod progress;
```

- [ ] **Step 3: Write `progress.rs` with the full helper + tests**

Create `crates/devkit-common/src/progress.rs`:

```rust
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::cell::Cell;
use std::io::IsTerminal;
use std::time::Duration;

/// A group of progress bars sharing one [`MultiProgress`]. Each bar animates on
/// stderr; the whole group is hidden when stderr is not a terminal, so pipes,
/// redirects, MCP, and tests produce no progress output.
///
/// Two display modes:
/// - [`Steps::new`] is unnumbered — for concurrent displays where several
///   [`Steps::spinner`] bars animate at once, or for branchy/prompt-interleaved
///   flows where a fixed `[i/N]` count would be misleading.
/// - [`Steps::with_total`] numbers each [`Steps::during`] step `[i/total]`.
pub struct Steps {
    mp: MultiProgress,
    total: Option<usize>,
    n: Cell<usize>,
}

impl Steps {
    pub fn new() -> Steps {
        Steps {
            mp: Self::target(),
            total: None,
            n: Cell::new(0),
        }
    }

    /// Numbered mode: every [`Steps::during`] message is prefixed `[i/total]`.
    pub fn with_total(total: usize) -> Steps {
        Steps {
            mp: Self::target(),
            total: Some(total),
            n: Cell::new(0),
        }
    }

    fn target() -> MultiProgress {
        if std::io::stderr().is_terminal() {
            MultiProgress::new()
        } else {
            MultiProgress::with_draw_target(ProgressDrawTarget::hidden())
        }
    }

    /// In numbered mode, prefix `[i/total] ` and advance the counter; otherwise
    /// pass the message through unchanged.
    fn label(&self, msg: &str) -> String {
        match self.total {
            Some(total) => {
                let i = self.n.get() + 1;
                self.n.set(i);
                format!("[{i}/{total}] {msg}")
            }
            None => msg.to_string(),
        }
    }

    /// An indeterminate spinner bar for a single opaque/batched fetch. The
    /// message is used verbatim — embed any prefix yourself. Used directly for
    /// concurrent displays that show several bars at once.
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
            ProgressStyle::with_template(
                "{spinner:.cyan} {wide_msg} [{bar:20.cyan/dim}] {pos}/{len}",
            )
            .expect("valid bar template")
            .progress_chars("=>-"),
        );
        pb.enable_steady_tick(Duration::from_millis(80));
        pb.set_message(msg.to_string());
        pb
    }

    /// Run `f` under a spinner (auto-numbered in numbered mode), clearing the
    /// bar before returning — so the spinner never stays live across a `?`, a
    /// stdin prompt, or stdout output. The closure's return value (often a
    /// `Result`) is returned unchanged so callers can `?` it after the clear.
    pub fn during<T>(&self, msg: &str, f: impl FnOnce() -> T) -> T {
        let pb = self.spinner(&self.label(msg));
        let out = f();
        pb.finish_and_clear();
        out
    }

    /// Clear every bar in the group (call once all work is done).
    pub fn clear(&self) {
        let _ = self.mp.clear();
    }
}

impl Default for Steps {
    fn default() -> Self {
        Self::new()
    }
}

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

    #[test]
    fn during_returns_closure_value() {
        let steps = Steps::with_total(2);
        let out = steps.during("step one", || 41 + 1);
        assert_eq!(out, 42);
    }

    #[test]
    fn numbered_mode_advances_counter() {
        let steps = Steps::with_total(3);
        assert_eq!(steps.label("a"), "[1/3] a");
        assert_eq!(steps.label("b"), "[2/3] b");
        assert_eq!(steps.label("c"), "[3/3] c");
    }

    #[test]
    fn unnumbered_mode_passes_through() {
        let steps = Steps::new();
        assert_eq!(steps.label("a"), "a");
        assert_eq!(steps.label("b"), "b");
    }
}
```

- [ ] **Step 4: Run the helper tests — expect PASS**

Run: `cargo test -p devkit-common progress`
Expected: the four `progress::tests::*` tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-common/Cargo.toml crates/devkit-common/src/lib.rs crates/devkit-common/src/progress.rs
git commit -m "feat(common): add shared progress Steps helper"
```

---

## Task 2: Rewire the `issue` binary onto the shared helper

The `issue` binary currently uses `crate::spin::Steps` in `prs.rs`, the status module, and `dashboard.rs`. Repoint them at `devkit_common::progress::Steps` and delete the local module.

**Files:**
- Delete: `src/bin/issue/spin.rs`
- Modify: `src/bin/issue/main.rs` (remove the `mod spin;` declaration)
- Modify: every file referencing `crate::spin` (at minimum `prs.rs`, the status module, `dashboard.rs`)

- [ ] **Step 1: Find every reference**

Run: `rg -n "crate::spin|mod spin" src/bin/issue`
Note each hit — these are the lines to change.

- [ ] **Step 2: Replace the references**

For each `crate::spin::Steps`, change it to `devkit_common::progress::Steps`. (The method calls — `.spinner(...)`, `.bar(...)`, `.clear()` — are unchanged; the new helper keeps them.) Remove the `mod spin;` line from `main.rs`.

- [ ] **Step 3: Delete the old module**

```bash
git rm src/bin/issue/spin.rs
```

- [ ] **Step 4: Build + test the binary — expect green, no `spin` left**

Run: `cargo test -p devkit --bin issue 2>&1 | tail -20`
Run: `rg -n "crate::spin|mod spin" src/bin/issue` (expected: no output)
Expected: compiles and existing `issue` tests pass.

- [ ] **Step 5: Commit**

```bash
git add -A src/bin/issue
git commit -m "refactor(issue): use shared devkit_common::progress helper"
```

---

## Task 3: `issue checkout-pr` spinners

Unnumbered `Steps` (the flow branches on identifier kind and `resolve` may prompt). The spinner in `resolve` wraps only the network probes and is cleared before `prompt_choice` reads stdin.

**Files:**
- Modify: `src/bin/issue/checkout.rs`

- [ ] **Step 1: Thread `Steps` into `resolve` and wrap its probes**

Change `resolve`'s signature to accept the shared `Steps` and wrap the network probes. Replace the `Ident::Linear` and `Ident::Fuzzy` arms (currently `checkout.rs:144-191`) so the Linear/GitHub calls run under a spinner that is cleared before any prompt:

```rust
fn resolve(target: &str, key: Option<&str>, repo: &str, steps: &Steps) -> Result<Resolved> {
    match classify(target)? {
        Ident::Pr(n) => Ok(Resolved {
            pr_number: n,
            linear_id: None,
            linear_title: None,
        }),
        Ident::Linear(id) => {
            let key = key.context("Linear id given but LINEAR_API_KEY is not set")?;
            steps.during(&format!("Resolving Linear issue {id}…"), || {
                resolve_linear(&id, None, key)
            })
        }
        Ident::Fuzzy(n) => {
            // No Linear key → a bare number is a GitHub PR.
            let Some(key) = key else {
                return Ok(Resolved {
                    pr_number: n,
                    linear_id: None,
                    linear_title: None,
                });
            };
            // Probe both sides under a spinner; clear it before any prompt.
            let (exists, candidates) = steps.during(&format!("Resolving {n}…"), || {
                let exists = pr_exists(n, repo)?;
                let candidates = linear::issues_by_number(n, key)?;
                Ok::<_, anyhow::Error>((exists, candidates))
            })?;
            let is_tty = std::io::stdin().is_terminal();
            match decide_fuzzy(exists, &candidates, is_tty) {
                FuzzyDecision::ErrorNone => {
                    anyhow::bail!("no PR or Linear issue found for {n}")
                }
                FuzzyDecision::ErrorAmbiguous => anyhow::bail!(
                    "ambiguous {n} — rerun as #{n} (GitHub PR) or PREFIX-{n} (Linear)"
                ),
                FuzzyDecision::UsePr => Ok(Resolved {
                    pr_number: n,
                    linear_id: None,
                    linear_title: None,
                }),
                FuzzyDecision::UseLinear(r) => resolve_linear(&r.id, Some(r.title), key),
                FuzzyDecision::Prompt(cands) => match prompt_choice(exists, &cands, n)? {
                    None => Ok(Resolved {
                        pr_number: n,
                        linear_id: None,
                        linear_title: None,
                    }),
                    Some(r) => resolve_linear(&r.id, Some(r.title), key),
                },
            }
        }
    }
}
```

Add the import at the top of the file: `use devkit_common::progress::Steps;`.

- [ ] **Step 2: Wrap the sequential network/git phases in `run`**

In `run` (`checkout.rs:254`), construct the helper after the `args.apps` validation and wrap each blocking phase. Replace the block from the `resolve` call (`checkout.rs:268`) through the `gh pr checkout` cleanup (`checkout.rs:346`):

```rust
    let steps = Steps::new();
    let resolved = resolve(&args.target, key.as_deref(), monorepo_s, &steps)?;

    let meta: PrMeta = steps
        .during(&format!("Fetching PR #{}…", resolved.pr_number), || {
            gh_json(
                &[
                    "pr",
                    "view",
                    &resolved.pr_number.to_string(),
                    "--json",
                    "number,title,headRefName",
                ],
                monorepo_s,
            )
        })
        .with_context(|| format!("fetching PR #{}", resolved.pr_number))?;
```

…(the `ctx` / `wt_name` / `worktree` / `ensure!(!worktree.exists())` block is unchanged)… then wrap the fetch and worktree-add:

```rust
    steps.during("Fetching from origin…", || git(&["fetch", "origin"], monorepo_s))?;
    steps.during("Creating worktree…", || {
        git(
            &[
                "worktree",
                "add",
                "--detach",
                worktree_s,
                &cfg.defaults.baseline_ref,
            ],
            monorepo_s,
        )
    })?;
```

…and wrap the `gh pr checkout` inside the existing `with_cleanup` closure (`checkout.rs:325-330`):

```rust
        steps
            .during(&format!("Checking out PR #{}…", meta.number), || {
                capture(
                    "gh",
                    &["pr", "checkout", &meta.number.to_string()],
                    Some(worktree_s),
                )
            })
            .with_context(|| format!("checking out PR #{}", meta.number))?;
```

- [ ] **Step 3: Wrap the optional setup phase**

Wrap the `prep_apps` call in the `if args.setup` block (`checkout.rs:348-363`):

```rust
        steps.during("Preparing apps…", || {
            crate::setup::prep_apps(
                &worktree,
                &meta.head_ref_name,
                &args.apps,
                catalog,
                &setup_ctx,
                &cfg.templates.variables,
            )
        })?;
```

- [ ] **Step 4: Build + test — expect green**

Run: `cargo test -p devkit --bin issue checkout 2>&1 | tail -20`
Expected: compiles; the `checkout::tests::*` unit tests (classify/decide_fuzzy/slugify/with_cleanup/template) still pass — they don't touch `resolve`/`run`, so the signature change is contained.

- [ ] **Step 5: Commit**

```bash
git add src/bin/issue/checkout.rs
git commit -m "feat(issue): show progress spinners during checkout-pr"
```

---

## Task 4: `issue setup` spinners (numbered)

Numbered `Steps::with_total(N)`: `N = 2 + (apps non-empty ? 1 : 0)` — `git fetch`, `git worktree add`, and (if any apps) `prep_apps`.

**Files:**
- Modify: `src/bin/issue/setup.rs`

- [ ] **Step 1: Add the import**

At the top of `setup.rs`: `use devkit_common::progress::Steps;`.

- [ ] **Step 2: Build the helper and wrap the blocking calls**

In `run`, after `let monorepo_s = ...` (`setup.rs:141`) and before `git(&["fetch", "origin"], monorepo_s)?;`, add:

```rust
    let total = 2 + usize::from(!args.apps.is_empty());
    let steps = Steps::with_total(total);
```

Replace the `git fetch origin` call (`setup.rs:142`):

```rust
    steps.during("Fetching from origin…", || git(&["fetch", "origin"], monorepo_s))?;
```

The branch-existence check (`setup.rs:143-150`) stays as-is (local, instant). Wrap the `git worktree add` (`setup.rs:151-161`):

```rust
    steps.during("Creating worktree…", || {
        git(
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                worktree.to_str().unwrap(),
                &cfg.defaults.baseline_ref,
            ],
            monorepo_s,
        )
    })?;
```

Then wrap `prep_apps` (`setup.rs:180`), but only number a step when there is work:

```rust
    if args.apps.is_empty() {
        prep_apps(&worktree, &branch, &args.apps, catalog, &ctx, vars)?;
    } else {
        steps.during("Preparing apps…", || {
            prep_apps(&worktree, &branch, &args.apps, catalog, &ctx, vars)
        })?;
    }
```

- [ ] **Step 3: Build + test — expect green**

Run: `cargo test -p devkit --bin issue setup 2>&1 | tail -20`
Expected: compiles; existing `setup` tests (prep-file rendering, template defaults) pass — they call `write_prep_files`/`render` directly, not `run`.

- [ ] **Step 4: Commit**

```bash
git add src/bin/issue/setup.rs
git commit -m "feat(issue): show progress spinners during setup"
```

---

## Task 5: `issue info` and `issue end` spinners

Both unnumbered `Steps::new()` — `info` branches on `--cache-only` / discovered, and `end` interleaves a `confirm` prompt between gather and per-worktree removal.

**Files:**
- Modify: `src/bin/issue/info.rs`
- Modify: `src/bin/issue/end.rs`

- [ ] **Step 1: `info` — wrap the live-fetch calls**

Add `use devkit_common::progress::Steps;` at the top of `info.rs`. In `run`, construct `let steps = Steps::new();` just before the `if cache_only {` block (`info.rs:65`). The `cache_only` branch is untouched (no fetch, no spinner). In the `else if discovered` branch (`info.rs:75-104`), wrap the three network calls:

```rust
    } else if discovered {
        steps
            .during("Fetching PR status…", || st::fetch_prs(&d))?
            .apply_best(&mut row);
        if row.issue_id != "UNKNOWN" {
            let linear = steps.during("Fetching Linear status…", || {
                devkit_common::linear::states(
                    std::slice::from_ref(&row.issue_id),
                    devkit_common::secrets::resolve("LINEAR_API_KEY").as_deref(),
                )
            });
            if let Some(s) = linear.get(&row.issue_id) {
                row.linear_kind = Some(s.kind.clone());
                row.linear_name = Some(s.name.clone());
            }
        }
        let reason = st::reason_not_finished(&row, has_key, false);
        row.finished = reason.is_none();
        row.reason_not_finished = reason;
        linear_workspace =
            steps.during("Resolving Linear workspace…", devkit_common::linear::workspace_url_key);
        // (the info_cache::write block is unchanged)
```

And in the final `else` branch (main-clone case, `info.rs:105-109`):

```rust
    } else {
        linear_workspace =
            steps.during("Resolving Linear workspace…", devkit_common::linear::workspace_url_key);
    }
```

- [ ] **Step 2: `info` build + test — expect green**

Run: `cargo test -p devkit --bin issue info 2>&1 | tail -20`
Expected: compiles; `info` unit tests (pick_index/local_row/apply_cached_pr) pass — they don't call `run`.

- [ ] **Step 3: `end` — wrap `gather` and per-worktree `cleanup`**

Add `use devkit_common::progress::Steps;` at the top of `end.rs`. In `run`, construct `let steps = Steps::new();` as the first line of the function body (`end.rs:134`). Wrap both `gather` calls — the `clean_worktree` one (`end.rs:139`) and the normal one (`end.rs:152`):

```rust
        let report = steps.during("Fetching PR + Linear status…", || gather(start, &[]))?;
```
```rust
        let report = steps.during("Fetching PR + Linear status…", || gather(start, ids))?;
```

Then, in the removal loop (the `for row in &targets { ... }` block after line 170, where `cleanup(&row.worktree, &row.issue_id, force)` is called after the `confirm` gate), wrap the `cleanup` call so the spinner appears only after the prompt is answered:

```rust
        let label = /* existing label */;
        match steps.during(&format!("Removing {label}…"), || {
            cleanup(&row.worktree, &row.issue_id, force)
        }) {
            // (existing match arms on the CleanupError result are unchanged)
        }
```

(Read lines 170-end first to keep the exact existing `match`/error handling — only the `cleanup(...)` call is wrapped in `steps.during(...)`; the arms stay identical.)

- [ ] **Step 4: `end` build + test — expect green**

Run: `cargo test -p devkit --bin issue end 2>&1 | tail -20`
Expected: compiles; `end` tests (`select_explicit` and any cleanup tests) pass.

- [ ] **Step 5: Commit**

```bash
git add src/bin/issue/info.rs src/bin/issue/end.rs
git commit -m "feat(issue): show progress spinners during info and end"
```

---

## Task 6: `issue review` spinners

Unnumbered `Steps::new()`. Wrap the push, the `gh pr list`, the chosen PR action (`gh pr edit`/`gh pr create`), and the Slack post.

**Files:**
- Modify: `src/bin/issue/review.rs`

- [ ] **Step 1: Add the import and helper**

Add `use devkit_common::progress::Steps;` at the top of `review.rs`. In `run` (`review.rs:178`), construct `let steps = Steps::new();` before the branch-read (`review.rs:193`). The `git rev-parse` calls (local, instant) stay unwrapped.

- [ ] **Step 2: Wrap the network calls**

Wrap the push (`review.rs:200`):

```rust
    if !args.no_push {
        steps
            .during("Pushing branch…", || git(&["push", "-u", "origin", &branch], &start))
            .context("git push failed (refusing to force-push)")?;
    }
```

Wrap the existing-PR lookup (`review.rs:247`):

```rust
    let existing: Option<PrView> = steps
        .during("Looking up existing PR…", || {
            gh_json::<Vec<PrView>>(
                &[
                    "pr", "list", "--head", &branch, "--state", "all", "--json",
                    "number,state,url", "--limit", "1",
                ],
                &start,
            )
        })?
        .into_iter()
        .next();
```

Wrap the `gh pr edit --add-reviewer` (`review.rs:269`) inside the `PrAction::AddReviewer` arm:

```rust
            steps
                .during("Adding reviewer…", || {
                    capture(
                        "gh",
                        &["pr", "edit", &pr.number.to_string(), "--add-reviewer", &reviewer],
                        Some(&start),
                    )
                })
                .context("gh pr edit --add-reviewer failed")?;
```

Wrap the `gh pr create` (`review.rs:289`) inside the `PrAction::Create` arm (keep the existing arg list and the `.lines()...` post-processing — only the `capture(...)` call is wrapped):

```rust
            let out = steps
                .during("Creating PR…", || {
                    capture(
                        "gh",
                        &[ /* existing pr create args unchanged */ ],
                        Some(&start),
                    )
                })
                .context("gh pr create failed")?;
```

Wrap the Slack post (`review.rs:335`):

```rust
            steps.during("Notifying reviewer on Slack…", || {
                slack::post_message(&token, &person.slack, &text)
            })?;
```

- [ ] **Step 3: Build + test — expect green**

Run: `cargo test -p devkit --bin issue review 2>&1 | tail -20`
Expected: compiles; `review` unit tests (e.g. `action_for`, `guard_branch`) pass.

- [ ] **Step 4: Commit**

```bash
git add src/bin/issue/review.rs
git commit -m "feat(issue): show progress spinners during review"
```

---

## Task 7: `devrun up` spinners

Unnumbered `Steps::new()` (the flow loops over role groups and short-circuits on `dry_run`). Wrap the baseline refresh and the server launch (the readiness wait — the longest single wait in the suite).

**Files:**
- Modify: `src/bin/devrun/main.rs`

- [ ] **Step 1: Add the import and helper**

Add `use devkit_common::progress::Steps;` to the imports at the top of `src/bin/devrun/main.rs`. In `cmd_up` (`main.rs:414`), construct `let steps = Steps::new();` before the `groups` vector is built (`main.rs:459`).

- [ ] **Step 2: Wrap the baseline refresh**

Wrap `baseline::ensure_fresh` (`main.rs:476`) inside the `Role::Baseline` arm:

```rust
                steps.during("Refreshing baseline…", || {
                    baseline::ensure_fresh(&issue_holder, &bp, &cfg.defaults.baseline_ref)
                })?;
```

- [ ] **Step 3: Wrap the server launch**

Wrap `run::launch` (`main.rs:528`). The app count is known here, so name it:

```rust
        let statuses = steps.during(
            &format!("Starting {} server(s) [{}]…", apps.len(), grp_role.as_str()),
            || run::launch(&plans, holder, *grp_role, supervise, true),
        )?;
```

(The `dry_run` branch `continue`s before this line, so no spinner shows for a dry run.)

- [ ] **Step 4: Build + test — expect green**

Run: `cargo test -p devkit --bin devrun 2>&1 | tail -20`
Expected: compiles; existing `devrun` tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/bin/devrun/main.rs
git commit -m "feat(devrun): show progress spinners during up"
```

---

## Task 8: `devkit auth` and `devkit doctor` spinners

`auth`: a single network validation — unnumbered `Steps::new()`. `doctor`: a runtime-counted set of validations — numbered `Steps::with_total(N)` where `N` counts the set tokens (Linear key + Slack token).

**Files:**
- Modify: `src/bin/devkit/auth.rs`
- Modify: `src/bin/devkit/doctor.rs`

- [ ] **Step 1: `auth` — wrap the validation**

Add `use devkit_common::progress::Steps;` to `auth.rs`. In `run` (`auth.rs:18`), after `let token = acquire(provider, token)?;` (the prompt happens inside `acquire`, before the spinner), construct `let steps = Steps::new();` and wrap each validation:

```rust
        Provider::Linear => {
            let id = steps
                .during("Validating Linear API key…", || linear::validate(&token))
                .context("validating Linear API key")?;
            store_linear(&path, &token, &id)?;
            // (unchanged println)
        }
        Provider::Slack => {
            let id = steps
                .during("Validating Slack token…", || slack::validate(&token))
                .context("validating Slack token")?;
            store_slack(&path, &token)?;
            // (unchanged println)
        }
```

- [ ] **Step 2: `doctor` — thread `Steps` into `gather` and wrap the validations**

Add `use devkit_common::progress::Steps;` to `doctor.rs`. Change `gather`'s signature to `fn gather(steps: &Steps) -> Vec<Row>` and wrap the two network validations (`doctor.rs:65` and `doctor.rs:81`):

```rust
            check: match secrets::resolve("LINEAR_API_KEY") {
                Some(v) => steps.during("Validating Linear API key…", || validate_linear(&v)),
                None => Check::Unset(HINT_LINEAR),
            },
```
```rust
            check: match secrets::resolve("SLACK_TOKEN") {
                Some(v) => steps.during("Validating Slack token…", || validate_slack(&v)),
                None => Check::Unset(HINT_SLACK),
            },
```

In `run` (`doctor.rs:129`), compute the total from the set tokens, build a numbered `Steps`, and pass it in:

```rust
pub fn run(json: bool) -> Result<()> {
    let total = usize::from(secrets::resolve("LINEAR_API_KEY").is_some())
        + usize::from(secrets::resolve("SLACK_TOKEN").is_some());
    let steps = Steps::with_total(total);
    let rows = gather(&steps);
    steps.clear();
    // (unchanged json/human print + exit logic)
}
```

- [ ] **Step 3: Build + test — expect green**

Run: `cargo test -p devkit --bin devkit 2>&1 | tail -20`
Expected: compiles; existing `devkit` tests (`auth`/`doctor` classification helpers) pass.

- [ ] **Step 4: Commit**

```bash
git add src/bin/devkit/auth.rs src/bin/devkit/doctor.rs
git commit -m "feat(devkit): show progress spinners during auth and doctor"
```

---

## Task 9: Workspace gate + docs

- [ ] **Step 1: Format**

Run: `cargo fmt --all`

- [ ] **Step 2: Full clippy gate — expect zero warnings**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings. (Watch for `new_without_default` on `Steps` — Task 1 already adds `impl Default`.)

- [ ] **Step 3: Full test gate — expect all green**

Run: `cargo test --workspace 2>&1 | tail -30`
Expected: every test passes; stdout-asserting command tests are unaffected because bars draw on stderr and are hidden off-TTY.

- [ ] **Step 4: Manual TTY smoke check (not automatable)**

In a real terminal in the worktree, run a low-risk command that blocks and confirm a spinner animates on stderr then clears:
Run: `cargo run --bin devkit -- doctor`
Expected: a `Validating …` spinner appears while tokens validate, then clears before the report table prints. Then confirm piping hides it:
Run: `cargo run --bin devkit -- doctor 2>/dev/null | cat` (expected: clean table, no spinner glyphs).

- [ ] **Step 5: Note the helper in AGENTS.md**

In `AGENTS.md`, under the `crates/devkit-common` row of the Layout table, add `progress` to the listed modules (e.g. `… ui (tables/links), progress (TTY spinners), linear, slack, supervise`).

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "chore: format, document progress helper, finalize spinner pass"
```

---

## Self-Review

**Spec coverage:** All eight commands from the spec table have a task (3–8); the shared-helper move + numbering/`during` extension is Task 1; the `issue` rewire that keeps `status`/`prs`/`dashboard` working is Task 2; the invariant gate (off-TTY hidden, output unchanged) is verified in Task 9. No spec requirement is unaddressed.

**Placeholders:** Concrete code/commands in every step. The only deliberately-not-inlined region is the `end` removal loop's `match` arms (Task 5 Step 3) and the `gh pr create` arg list (Task 6 Step 2), where the instruction is to wrap an existing call and leave the surrounding code identical — the executor reads those lines first. These are wrap-in-place edits, not unspecified logic.

**Type consistency:** `Steps::new()` / `Steps::with_total(usize)` / `during<T>(&str, FnOnce() -> T) -> T` / `spinner` / `bar` / `clear` are used identically across every task. `resolve`'s new `steps: &Steps` parameter is introduced in Task 3 and not referenced elsewhere. `gather(&Steps)` in `doctor` (Task 8) is self-contained to that file.
