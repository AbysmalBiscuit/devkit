# Issue Binary Consolidation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the `issue-prep`, `issue-end`, and `pr-status` binaries with a single `issue` binary whose subcommands (`setup`, `status`, `end`, `prs`, `dashboard`, `review`) cover the whole issue lifecycle, adding a terminal dashboard and a mechanical review-shipping command.

**Architecture:** One new workspace crate `crates/issue` produces the `issue` binary. Logic is split into focused modules: ports of the three deleted binaries (`setup`/`triage`/`status`/`end`/`prs`), plus new `review` and `dashboard/{mod,data,bucket,chart}`. Shared library code grows by a Linear history query (`devkit-common::linear`), a Slack poster (`devkit-common::slack`), and config additions (`[people]`, `defaults.pr_base`). Date math uses `chrono`; line charts use `textplots`; bar charts are hand-rolled Unicode blocks.

**Tech Stack:** Rust 2024, clap (derive), serde/serde_json, toml, ureq (Linear/Slack HTTP), `gh`/`git` CLI wrappers, `chrono` (date bucketing), `textplots` (line charts), `nix` (terminal width ioctl).

---

## Reference: source material

The three binaries being ported (read these before starting — the ports are near-verbatim moves):

- `crates/issue-prep/src/main.rs` (128 lines) → `setup.rs`
- `crates/issue-end/src/main.rs` (345 lines) → `triage.rs` + `status.rs` + `end.rs`
- `crates/pr-status/src/main.rs` (591 lines) → `prs.rs`

The two Python dashboards being ported (the precise spec for bucketing/replay/charts):

- `~/Git/example/issues_dispatch/dashboard_issues.py` (Linear issues by status)
- `~/Git/example/issues_dispatch/dashboard_prs.py` (PRs opened/merged + commits)

The full design: `docs/superpowers/specs/2026-06-20-issue-binary-consolidation-design.md`.

## File structure

**New crate `crates/issue/`:**

| File | Responsibility |
|---|---|
| `Cargo.toml` | crate manifest; deps incl. `chrono`, `textplots` |
| `src/main.rs` | clap root, global `-C/--dir` + `--config`, dispatch to subcommands |
| `src/setup.rs` | `issue setup` — verbatim `issue-prep` |
| `src/triage.rs` | shared worktree-triage core: `Pr`, `best_pr`, `Row`, `build_rows`, `reason_not_finished`, `gather`, `render` |
| `src/status.rs` | `issue status` — `cmd_status` |
| `src/end.rs` | `issue end` — `cmd_clean`, `cleanup`, `select_explicit`, `confirm`, `CleanupError` |
| `src/prs.rs` | `issue prs` — verbatim `pr-status` minus its `main`/CLI; orchestration as `run()` |
| `src/review.rs` | `issue review` — branch guard, push, PR create/reuse, reviewer, Slack |
| `src/dashboard/mod.rs` | orchestrate at-a-glance + timelines |
| `src/dashboard/data.rs` | live fetch: Linear history, gh PR list, git log |
| `src/dashboard/bucket.rs` | pure date bucketing + state replay + tally |
| `src/dashboard/chart.rs` | terminal bar (hand-rolled) + line (textplots) rendering, terminal width |

**Shared library changes:**

| File | Change |
|---|---|
| `crates/devkit-common/src/linear.rs` | add `assigned_issue_history`, `viewer_created_at`, `AssignedIssue`, `StateRef` |
| `crates/devkit-common/src/slack.rs` | new — `post_message(token, channel, text)` |
| `crates/devkit-common/src/lib.rs` | add `pub mod slack;` |
| `crates/devkit-ports/src/config.rs` | add `[people]` map + `Person`, `defaults.pr_base` |
| `configs/example.toml` | add `pr_base` + `[people.igor]` |

**Deleted:** `crates/issue-prep/`, `crates/issue-end/`, `crates/pr-status/`.

**Docs:** `CLAUDE.md`, `README.md` updated; `../docs/issue-binary-migration.md` (base repo, uncommitted) written in the final phase.

---

# Phase 1 — Consolidation (pure refactor, no behavior change)

Scaffold `crates/issue`, move setup/status/end/prs into modules, delete the three old crates. All existing tests move with their code and stay green.

## Task 1.1: Scaffold the `issue` crate with a trivial binary

**Files:**
- Create: `crates/issue/Cargo.toml`
- Create: `crates/issue/src/main.rs`

- [ ] **Step 1: Write the crate manifest**

`crates/issue/Cargo.toml`:

```toml
[package]
name = "issue"
edition.workspace = true
version.workspace = true

[[bin]]
name = "issue"
path = "src/main.rs"

[dependencies]
anyhow.workspace = true
clap.workspace = true
serde = { workspace = true }
serde_json.workspace = true
devkit-common.workspace = true
devkit-ports.workspace = true
chrono.workspace = true
textplots.workspace = true
```

- [ ] **Step 2: Add the two new workspace dependencies**

In `Cargo.toml` (workspace root) under `[workspace.dependencies]`, add:

```toml
chrono = { version = "0.4", default-features = false, features = ["std"] }
textplots = { version = "0.8", default-features = false }
```

(`chrono` without `clock` avoids the `iana-time-zone` dependency; current time comes from `SystemTime`. `textplots` without default features skips its optional `tool` binary feature that pulls obsolete `structopt`.)

- [ ] **Step 3: Write a placeholder main**

`crates/issue/src/main.rs`:

```rust
use anyhow::Result;

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("issue");
    println!("issue: not yet wired");
    Ok(())
}
```

- [ ] **Step 4: Build to verify the crate resolves and deps download**

Run: `cargo build -p issue`
Expected: compiles clean; `chrono` and `textplots` download.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/issue/Cargo.toml crates/issue/src/main.rs
git commit -m "feat(issue): scaffold consolidated issue crate"
```

## Task 1.2: Port `issue-prep` → `setup.rs`

**Files:**
- Create: `crates/issue/src/setup.rs`
- Modify: `crates/issue/src/main.rs`

This is a near-verbatim move of `crates/issue-prep/src/main.rs`. The body of its `main()` becomes `setup::run(cli)`, parameterised by the setup args.

- [ ] **Step 1: Create `setup.rs` from the issue-prep source**

Copy `crates/issue-prep/src/main.rs` into `crates/issue/src/setup.rs`, then transform:
- Delete the `use clap::Parser;` line and the `#[derive(Parser)] struct Cli {…}` block (the args move to `main.rs`'s subcommand).
- Delete `fn main()`'s signature/hook; rename it to `pub fn run(args: SetupArgs) -> Result<()>` and replace each `cli.<field>` with `args.<field>`.
- Add a `SetupArgs` struct the subcommand fills:

```rust
pub struct SetupArgs {
    pub issue: String,
    pub slug: String,
    pub apps: Vec<String>,
    pub dry_run: bool,
    pub dir: Option<String>,
    pub config: Option<String>,
}
```

- Keep `branch_name`, `Prepared`, and the `#[cfg(test)] mod tests` (the `branch_uses_prefix_and_slug` test) unchanged.
- Keep all imports the body uses: `anyhow::{Context, Result}`, `devkit_common::cmd::{capture, git}`, `devkit_ports::config::expand_tilde`, `devkit_ports::load`, `devkit_ports::registry::{self, Data, Role}`, `std::collections::BTreeMap`, `std::path::Path`.

The resulting `run` opens exactly as the old `main` did (minus the hook/parse):

```rust
pub fn run(args: SetupArgs) -> Result<()> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());
    let loaded = load::load(args.config.as_deref().map(Path::new), Path::new(&start))?;
    let cfg = &loaded.config;
    let catalog = &loaded.catalog;
    // … identical to issue-prep main body, with cli.* → args.* …
}
```

- [ ] **Step 2: Declare the module and dispatch in `main.rs`**

Replace `crates/issue/src/main.rs` with the real CLI root:

```rust
use anyhow::Result;
use clap::{Parser, Subcommand};

mod setup;

#[derive(Parser)]
#[command(name = "issue", about = "Issue lifecycle: setup, status, end, prs, dashboard, review")]
struct Cli {
    #[arg(short = 'C', long = "dir", global = true)]
    dir: Option<String>,
    #[arg(long, global = true)]
    config: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Prepare an issue worktree: branch, env symlinks, deps, reserved ports.
    Setup {
        #[arg(long)]
        issue: String,
        #[arg(long)]
        slug: String,
        #[arg(long, value_delimiter = ',')]
        apps: Vec<String>,
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("issue");
    let cli = Cli::parse();
    match cli.cmd {
        Some(Cmd::Setup { issue, slug, apps, dry_run }) => setup::run(setup::SetupArgs {
            issue, slug, apps, dry_run, dir: cli.dir, config: cli.config,
        }),
        None => {
            println!("issue: run `issue --help`");
            Ok(())
        }
    }
}
```

- [ ] **Step 3: Build and run the setup tests**

Run: `cargo test -p issue setup`
Expected: PASS (`branch_uses_prefix_and_slug`).

- [ ] **Step 4: Smoke-test the dry-run path against the real config**

Run: `cargo run -p issue -- -C ~/Git/example/monorepo setup --issue ENG-1 --slug eng-1-smoke --apps api --dry-run`
Expected: prints a `Prepared` JSON blob with a port for `api`, plus `(dry-run: …)` on stderr.

- [ ] **Step 5: Commit**

```bash
git add crates/issue/src/setup.rs crates/issue/src/main.rs
git commit -m "feat(issue): port issue-prep to issue setup"
```

## Task 1.3: Port `issue-end` triage core → `triage.rs`

**Files:**
- Create: `crates/issue/src/triage.rs`
- Modify: `crates/issue/src/main.rs`

Move the shared, pure-ish triage code (everything `status` and `end` both use). `Row` and its fields become `pub(crate)` so sibling modules and the dashboard can read them.

- [ ] **Step 1: Create `triage.rs` with the shared core**

From `crates/issue-end/src/main.rs`, move into `crates/issue/src/triage.rs`: `Pr`, `state_rank`, `best_pr`, `Row`, `build_rows`, `reason_not_finished`, `Gathered`, `gather`, `pr_label`, `render`, and the `#[cfg(test)] mod tests` block (best_pr_*, finished_when_merged_done_clean, not_finished_when_dirty, pr_only_ignores_linear). Apply these edits:

- Make shared items `pub(crate)`: `pub(crate) struct Pr`, `pub(crate) fn best_pr`, `pub(crate) struct Row` **and all its fields**, `pub(crate) fn build_rows`, `pub(crate) fn reason_not_finished`, `pub(crate) type Gathered`, `pub(crate) fn gather`, `pub(crate) fn render`.
- Keep `state_rank`, `pr_label` private (only used within `triage.rs`).
- Imports at the top:

```rust
use anyhow::{Context, Result};
use devkit_common::cmd::{git, gh_json};
use devkit_common::linear::{self, LinearState};
use devkit_common::ui;
use devkit_common::worktree;
use serde::Deserialize;
use std::collections::HashMap;
```

- `Pr` keeps `#[derive(Debug, Clone, Deserialize)]`; `Row` keeps `#[derive(Debug, Clone)]`.
- The functions' bodies are unchanged from issue-end (same `build_rows`, `reason_not_finished`, `gather`, `render` logic).

- [ ] **Step 2: Declare the module**

In `crates/issue/src/main.rs`, add `mod triage;` below `mod setup;`. (No dispatch yet — status/end arrive next.)

- [ ] **Step 3: Run the triage tests**

Run: `cargo test -p issue triage`
Expected: PASS — `best_pr_prefers_merged_over_open`, `best_pr_higher_number_within_same_state`, `best_pr_none_for_unknown_head`, `finished_when_merged_done_clean`, `not_finished_when_dirty`, `pr_only_ignores_linear`.

- [ ] **Step 4: Commit**

```bash
git add crates/issue/src/triage.rs crates/issue/src/main.rs
git commit -m "feat(issue): extract shared worktree-triage core"
```

## Task 1.4: Port `issue status` → `status.rs`

**Files:**
- Create: `crates/issue/src/status.rs`
- Modify: `crates/issue/src/main.rs`

- [ ] **Step 1: Create `status.rs`**

`crates/issue/src/status.rs` (the old `cmd_status`, renamed to `run`, with the message repointed from `issue-end clean` to `issue end`):

```rust
use crate::triage::{gather, render};
use anyhow::Result;

pub fn run(start: &str, ids: &[String]) -> Result<()> {
    let (rows, states, has_key, url_key) = gather(start, ids)?;
    let finished = render(&rows, &states, has_key, url_key.as_deref());
    if finished > 0 {
        println!("\n{finished} finished. Run `issue end` to remove them.");
    }
    if !has_key {
        println!("\nLINEAR_API_KEY unset — Linear gate skipped. Create a key at https://linear.app/settings/api");
    }
    Ok(())
}
```

- [ ] **Step 2: Wire the `Status` subcommand**

In `main.rs`: add `mod status;`. Add to `enum Cmd`:

```rust
    /// Read-only report of every issue worktree (optionally filtered by ID).
    Status { ids: Vec<String> },
```

Add to the `match`, and make the bare-`issue` default run status:

```rust
        Some(Cmd::Status { ids }) => status::run(&start(&cli.dir), &ids),
        None => status::run(&start(&cli.dir), &[]),
```

Add a helper above `main`:

```rust
fn start(dir: &Option<String>) -> String {
    dir.clone().unwrap_or_else(|| ".".to_string())
}
```

- [ ] **Step 3: Build and smoke-test**

Run: `cargo run -p issue -- -C ~/Git/example/monorepo status`
Expected: prints the `ISSUE WORKTREES` table (or `(none)`); exit 0.

- [ ] **Step 4: Commit**

```bash
git add crates/issue/src/status.rs crates/issue/src/main.rs
git commit -m "feat(issue): port issue-end status to issue status"
```

## Task 1.5: Port `issue end` → `end.rs`

**Files:**
- Create: `crates/issue/src/end.rs`
- Modify: `crates/issue/src/main.rs`

- [ ] **Step 1: Create `end.rs`**

From `crates/issue-end/src/main.rs`, move `select_explicit`, `confirm`, `CleanupError`, `cleanup`, and `cmd_clean` (renamed `run`) into `crates/issue/src/end.rs`. Edits:
- Import the shared core: `use crate::triage::{gather, render, reason_not_finished, Row};`
- Other imports: `use anyhow::{Context, Result};`, `use devkit_common::cmd::git;`, `use std::io::{self, Write};`, `use std::path::Path;`
- `select_explicit` takes `&[Row]` and reads `r.worktree`, `r.issue_id`, `r.branch` — these are now `pub(crate)` fields (Task 1.3), so no signature change.
- `cmd_clean` becomes `pub fn run(start: &str, ids: &[String], yes: bool, force: bool, pr_only: bool, clean_worktree: bool) -> Result<()>`; body unchanged.

- [ ] **Step 2: Wire the `End` subcommand**

In `main.rs`: add `mod end;`. Add to `enum Cmd`:

```rust
    /// Remove FINISHED worktrees (PR merged + Linear done + clean).
    End {
        ids: Vec<String>,
        #[arg(short = 'y', long)]
        yes: bool,
        #[arg(long)]
        force: bool,
        #[arg(long = "pr-only")]
        pr_only: bool,
        #[arg(long = "clean-worktree")]
        clean_worktree: bool,
    },
```

Add to the `match`:

```rust
        Some(Cmd::End { ids, yes, force, pr_only, clean_worktree }) =>
            end::run(&start(&cli.dir), &ids, yes, force, pr_only, clean_worktree),
```

- [ ] **Step 3: Build and verify the no-op clean path**

Run: `cargo run -p issue -- -C ~/Git/example/monorepo end` (no worktrees finished → safe)
Expected: prints the table then `Nothing finished to clean up.`; exit 0. Does not remove anything.

- [ ] **Step 4: Commit**

```bash
git add crates/issue/src/end.rs crates/issue/src/main.rs
git commit -m "feat(issue): port issue-end clean to issue end"
```

## Task 1.6: Port `pr-status` → `prs.rs`

**Files:**
- Create: `crates/issue/src/prs.rs`
- Modify: `crates/issue/src/main.rs`

Move the entire `pr-status` source. Its `main()` orchestration becomes `run(mine, reviews, repo, no_cache)`; its CLI struct is dropped (flags move to the subcommand). The fetch/render functions become `pub(crate)` for dashboard reuse.

- [ ] **Step 1: Create `prs.rs`**

Copy `crates/pr-status/src/main.rs` into `crates/issue/src/prs.rs`, then:
- Delete `use clap::Parser;` and the `#[derive(Parser)] struct Cli {…}`.
- Replace `fn main() -> Result<()> { … }` with `pub fn run(mine: bool, reviews: bool, repo: Option<String>, no_cache: bool) -> Result<()> { … }`. Inside, delete the panic-hook line and the `Cli::parse()` line, and replace the derived fields:
  - `cli.mine` → `mine`, `cli.reviews` → `reviews`, `cli.no_cache` → `no_cache`
  - `cli.repo.as_deref()` → `repo.as_deref()`
- Make these `pub(crate)` for reuse by the dashboard: `MinePr`, `ReviewPr`, `Check`, `Review`, `Author`, `ReviewRequest`, `fn fetch_mine`, `fn fetch_reviews`, `fn resolve_repo`, `fn mine_table`, `fn reviews_table`, and the `Me` struct.
- Keep everything else (pure logic, diff cache, `gh_json` wrapper, the `std::thread::scope` block, the `#[cfg(test)] mod tests`) unchanged.

- [ ] **Step 2: Wire the `Prs` subcommand**

In `main.rs`: add `mod prs;`. Add to `enum Cmd`:

```rust
    /// At-a-glance triage of your GitHub PRs via gh.
    Prs {
        #[arg(short = 'm', long)]
        mine: bool,
        #[arg(short = 'r', long)]
        reviews: bool,
        #[arg(short = 'R', long)]
        repo: Option<String>,
        #[arg(long = "no-cache")]
        no_cache: bool,
    },
```

Add to the `match`:

```rust
        Some(Cmd::Prs { mine, reviews, repo, no_cache }) => prs::run(mine, reviews, repo, no_cache),
```

- [ ] **Step 3: Run the pr-status logic tests under the new crate**

Run: `cargo test -p issue prs`
Expected: PASS — `checks_fail_run_ok_empty`, `approved_green_merges`, `approved_with_failing_ci`, `changes_requested_action`, `draft_action`, `review_text_variants`, `reviewer_state_requested_needs_review`, `reviewer_state_approved_done`, `issue_of_finds_swe`, `issue_of_finds_non_swe_prefix`, `diff_cell_shows_change`.

- [ ] **Step 4: Smoke-test**

Run: `cargo run -p issue -- prs -m`
Expected: prints `MY OPEN PRs` table (or `(none)`); exit 0.

- [ ] **Step 5: Commit**

```bash
git add crates/issue/src/prs.rs crates/issue/src/main.rs
git commit -m "feat(issue): port pr-status to issue prs"
```

## Task 1.7: Delete the three old crates

**Files:**
- Delete: `crates/issue-prep/`, `crates/issue-end/`, `crates/pr-status/`

- [ ] **Step 1: Remove the directories**

```bash
git rm -r crates/issue-prep crates/issue-end crates/pr-status
```

(The workspace `members = ["crates/*"]` glob needs no edit.)

- [ ] **Step 2: Full gate — build, test, clippy**

Run: `cargo build --release`
Expected: builds `portman`, `devrun`, `issue` (3 binaries).

Run: `cargo test --workspace`
Expected: all green; the migrated tests now live under `issue`.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: zero warnings. (If clippy flags an unused `pub(crate)` import in `prs.rs`/`triage.rs` from the not-yet-wired dashboard reuse, add `#[allow(dead_code)]` on that specific item with a one-line comment that the dashboard consumes it — it is wired in Phase 3.)

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "refactor: delete issue-prep, issue-end, pr-status (folded into issue)"
```

## Task 1.8: Update CLAUDE.md and README for the new binary set

**Files:**
- Modify: `CLAUDE.md`
- Modify: `README.md`

- [ ] **Step 1: Update `CLAUDE.md`**

- In the `## Commands` block, change the `cargo build --release` comment from `# all five binaries → target/release` to `# all three binaries → target/release`.
- In the `## Layout` table, replace the two rows:

```
| `issue-prep` / `issue-end` | worktree setup / triage + cleanup |
| `pr-status` | GitHub PR triage with a per-repo diff cache |
```

with:

```
| `issue` | issue lifecycle: `setup`, `status`, `end`, `prs`, `dashboard`, `review` |
```

- In the opening sentence, change "two library crates and five binaries" to "two library crates and three binaries".

- [ ] **Step 2: Update `README.md`**

Run `rg -n "issue-prep|issue-end|pr-status|five binaries" README.md` and repoint each hit to the corresponding `issue <subcommand>` form. (Exact lines depend on the current README; replace command examples like `issue-end status` → `issue status`, `pr-status -m` → `issue prs -m`, `issue-prep --issue …` → `issue setup --issue …`.)

- [ ] **Step 3: Verify no stale references remain in tracked docs**

Run: `rg -n "issue-prep|issue-end|pr-status" -g '!docs/superpowers/**' .`
Expected: no matches outside the spec/plan under `docs/superpowers/` (those describe the migration and may mention old names).

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md README.md
git commit -m "docs: update binary layout for the consolidated issue command"
```

---

# Phase 2 — `issue review`

Add the `[people]` config + `defaults.pr_base`, the `devkit-common::slack` poster, and `review.rs` with its pure decision functions.

## Task 2.1: Add `[people]` and `defaults.pr_base` to config

**Files:**
- Modify: `crates/devkit-ports/src/config.rs`
- Modify: `configs/example.toml`

- [ ] **Step 1: Write a failing test for the new config fields**

In `crates/devkit-ports/src/config.rs`, extend the `tests` module's `SAMPLE` to include the new keys and add a test:

```rust
    #[test]
    fn parses_people_and_pr_base() {
        let withppl = format!(
            "{SAMPLE}\npr_base = \"staging\"\n[people.igor]\nslack = \"U0XXXXXXXXX\"\ngithub = \"exampleuser\"\n"
        );
        // pr_base sits under [defaults]; move it there for the assertion:
        let cfg_src = withppl.replace(
            "[defaults]\n",
            "[defaults]\npr_base = \"staging\"\n",
        ).replace("\npr_base = \"staging\"\n[people", "\n[people");
        let c = Config::parse(&cfg_src).unwrap();
        assert_eq!(c.defaults.pr_base, "staging");
        let igor = c.people.get("igor").unwrap();
        assert_eq!(igor.slack, "U0XXXXXXXXX");
        assert_eq!(igor.github.as_deref(), Some("exampleuser"));
    }
```

To keep the test simple and robust, instead replace `SAMPLE` usage with a dedicated literal in the test:

```rust
    #[test]
    fn parses_people_and_pr_base() {
        let src = r#"
[defaults]
worktree_root = "~/Git/example"
branch_prefix = "lev/"
baseline_ref = "origin/staging"
baseline_path = "~/Git/example/_baseline"
doppler_config = "dev_local"
doppler_yaml = "~/Git/example/monorepo/doppler.yaml"
pr_base = "staging"
[apps.api]
base_port = 9100
launch = ["nitro", "dev", "--port", "{port}"]
[people.igor]
slack = "U0XXXXXXXXX"
github = "exampleuser"
"#;
        let c = Config::parse(src).unwrap();
        assert_eq!(c.defaults.pr_base, "staging");
        let igor = c.people.get("igor").unwrap();
        assert_eq!(igor.slack, "U0XXXXXXXXX");
        assert_eq!(igor.github.as_deref(), Some("exampleuser"));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p devkit-ports parses_people_and_pr_base`
Expected: FAIL — `no field people on Config` / `no field pr_base`.

- [ ] **Step 3: Add the fields and the `Person` type**

In `crates/devkit-ports/src/config.rs`:

Add to `struct Config`:

```rust
    #[serde(default)]
    pub people: HashMap<String, Person>,
```

Add to `struct Defaults`:

```rust
    #[serde(default = "default_pr_base")]
    pub pr_base: String,
```

Add the default fn and the `Person` struct:

```rust
fn default_pr_base() -> String {
    "staging".to_string()
}

#[derive(Debug, Deserialize)]
pub struct Person {
    pub slack: String,
    #[serde(default)]
    pub github: Option<String>,
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p devkit-ports parses_people_and_pr_base`
Expected: PASS. Also run `cargo test -p devkit-ports config` to confirm `parses_sample` and `rejects_prd` still pass (both omit the new keys, which default).

- [ ] **Step 5: Add the real values to `configs/example.toml`**

Add `pr_base = "staging"` to the `[defaults]` block (after `doppler_yaml`), and append a people section:

```toml
[people.igor]
slack  = "U0XXXXXXXXX"
github = "exampleuser"
```

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-ports/src/config.rs configs/example.toml
git commit -m "feat(config): add [people] aliases and defaults.pr_base"
```

## Task 2.2: Add the `devkit-common::slack` poster

**Files:**
- Create: `crates/devkit-common/src/slack.rs`
- Modify: `crates/devkit-common/src/lib.rs`

- [ ] **Step 1: Write a failing test for the response parser**

The HTTP call itself isn't unit-tested, but the Slack response check is pure. Create `crates/devkit-common/src/slack.rs` with a private `check_response` and a test:

```rust
use anyhow::{bail, Result};

/// Post a message to a Slack channel/user id via chat.postMessage.
pub fn post_message(token: &str, channel: &str, text: &str) -> Result<()> {
    let resp: serde_json::Value = ureq::post("https://slack.com/api/chat.postMessage")
        .set("Authorization", &format!("Bearer {token}"))
        .send_json(ureq::json!({ "channel": channel, "text": text }))?
        .into_json()?;
    check_response(&resp)
}

/// Slack returns `{ "ok": true }` or `{ "ok": false, "error": "..." }`.
fn check_response(resp: &serde_json::Value) -> Result<()> {
    if resp.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        return Ok(());
    }
    let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("unknown error");
    bail!("Slack chat.postMessage failed: {err}");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ok_true_passes() {
        assert!(check_response(&serde_json::json!({ "ok": true })).is_ok());
    }
    #[test]
    fn ok_false_surfaces_error() {
        let e = check_response(&serde_json::json!({ "ok": false, "error": "channel_not_found" }))
            .unwrap_err();
        assert!(e.to_string().contains("channel_not_found"));
    }
}
```

- [ ] **Step 2: Declare the module**

In `crates/devkit-common/src/lib.rs`, add `pub mod slack;` (keep the list alphabetical — between `report` and `ui`... actually after `report`):

```rust
pub mod cmd;
pub mod linear;
pub mod paths;
pub mod report;
pub mod slack;
pub mod ui;
pub mod worktree;
```

- [ ] **Step 3: Run the slack tests**

Run: `cargo test -p devkit-common slack`
Expected: PASS — `ok_true_passes`, `ok_false_surfaces_error`.

- [ ] **Step 4: Commit**

```bash
git add crates/devkit-common/src/slack.rs crates/devkit-common/src/lib.rs
git commit -m "feat(common): add slack chat.postMessage poster"
```

## Task 2.3: Implement `review.rs` pure decision logic

**Files:**
- Create: `crates/issue/src/review.rs`
- Modify: `crates/issue/src/main.rs`

Build the pure functions first (branch guard, PR-state → action, reviewer/message composition), test them, then wire the git/gh/Slack plumbing.

- [ ] **Step 1: Write failing tests for the pure decision functions**

Create `crates/issue/src/review.rs`:

```rust
use anyhow::{bail, Context, Result};
use devkit_common::cmd::{capture, git, gh_json};
use devkit_common::slack;
use devkit_ports::config::Person;
use serde::Deserialize;
use std::collections::HashMap;

/// What to do given an existing PR's state (or none) for the current branch.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PrAction {
    Create,
    AddReviewer,
    Stop(String),
}

/// Branches we must never open a review against.
pub(crate) fn guard_branch(branch: &str) -> Result<()> {
    if branch == "staging" || branch == "main" || branch == "HEAD" {
        bail!("refusing to review from base branch `{branch}` — switch to a feature branch");
    }
    Ok(())
}

/// Map a detected PR state to the next action.
pub(crate) fn action_for(pr_state: Option<&str>) -> PrAction {
    match pr_state {
        None => PrAction::Create,
        Some("OPEN") => PrAction::AddReviewer,
        Some("MERGED") => PrAction::Stop("PR already merged — nothing to review".into()),
        Some("CLOSED") => PrAction::Stop("PR is closed — nothing to review".into()),
        Some(other) => PrAction::Stop(format!("unexpected PR state `{other}`")),
    }
}

/// Resolve the GitHub reviewer handle: explicit flag wins, else the alias's github.
pub(crate) fn resolve_reviewer(explicit: Option<&str>, person: &Person) -> Result<String> {
    if let Some(r) = explicit {
        return Ok(r.to_string());
    }
    person.github.clone().context("no --reviewer given and alias has no `github` handle")
}

/// The Slack body with the PR URL appended.
pub(crate) fn compose_text(body: &str, pr_url: &str) -> String {
    format!("{body} {pr_url}")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn person(gh: Option<&str>) -> Person {
        Person { slack: "U1".into(), github: gh.map(String::from) }
    }
    #[test]
    fn guard_rejects_base_branches() {
        assert!(guard_branch("staging").is_err());
        assert!(guard_branch("main").is_err());
        assert!(guard_branch("lev/eng-1-fix").is_ok());
    }
    #[test]
    fn action_maps_pr_state() {
        assert_eq!(action_for(None), PrAction::Create);
        assert_eq!(action_for(Some("OPEN")), PrAction::AddReviewer);
        assert!(matches!(action_for(Some("MERGED")), PrAction::Stop(_)));
        assert!(matches!(action_for(Some("CLOSED")), PrAction::Stop(_)));
    }
    #[test]
    fn reviewer_prefers_explicit_then_alias() {
        assert_eq!(resolve_reviewer(Some("octocat"), &person(Some("exampleuser"))).unwrap(), "octocat");
        assert_eq!(resolve_reviewer(None, &person(Some("exampleuser"))).unwrap(), "exampleuser");
        assert!(resolve_reviewer(None, &person(None)).is_err());
    }
    #[test]
    fn compose_appends_url() {
        assert_eq!(compose_text("please review", "https://gh/pr/1"), "please review https://gh/pr/1");
    }
}
```

`Person`'s fields must be constructible here — they are already `pub` (Task 2.1). `slack`/`gh_json`/`capture`/`git`/`HashMap`/`Deserialize` are imported for Step 3; if clippy warns they're unused until then, complete Step 3 in the same commit.

- [ ] **Step 2: Run the tests to verify they fail (module not declared yet)**

Add `mod review;` to `main.rs`, then run: `cargo test -p issue review`
Expected: PASS for the four pure tests once the module compiles. (If the unused plumbing imports block compilation under `-D warnings` in tests, they don't — tests build without `-D warnings`. They are exercised in Step 3.)

- [ ] **Step 3: Add the orchestration `run` and the `SlackIntent` fallback**

Append to `review.rs`:

```rust
pub struct ReviewArgs {
    pub body: String,
    pub to: String,
    pub reviewer: Option<String>,
    pub base: Option<String>,
    pub pr_title: Option<String>,
    pub pr_body: Option<String>,
    pub no_push: bool,
    pub dir: Option<String>,
    pub config: Option<String>,
}

#[derive(Deserialize)]
struct PrView {
    number: u64,
    state: String,
    url: String,
}

#[derive(serde::Serialize)]
struct SlackIntent<'a> {
    slack_id: &'a str,
    text: &'a str,
    pr_url: &'a str,
    github: &'a str,
    branch: &'a str,
}

pub fn run(args: ReviewArgs) -> Result<()> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());
    let loaded = devkit_ports::load::load(
        args.config.as_deref().map(std::path::Path::new),
        std::path::Path::new(&start),
    )?;
    let people: &HashMap<String, Person> = &loaded.config.people;
    let person = people
        .get(&args.to)
        .with_context(|| format!("unknown person alias `{}` — add it under [people] in devkit.toml", args.to))?;
    let reviewer = resolve_reviewer(args.reviewer.as_deref(), person)?;

    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"], &start)?.trim().to_string();
    guard_branch(&branch)?;

    if !args.no_push {
        // Never force-push; surface the rejection verbatim.
        git(&["push", "-u", "origin", &branch], &start)
            .context("git push failed (refusing to force-push)")?;
    }

    let existing: Option<PrView> = gh_json::<Vec<PrView>>(
        &["pr", "list", "--head", &branch, "--state", "all", "--json", "number,state,url", "--limit", "1"],
        &start,
    )?.into_iter().next();

    let pr_url = match action_for(existing.as_ref().map(|p| p.state.as_str())) {
        PrAction::Stop(reason) => bail!("{reason}"),
        PrAction::AddReviewer => {
            let pr = existing.expect("AddReviewer implies an existing PR");
            capture("gh", &["pr", "edit", &pr.number.to_string(), "--add-reviewer", &reviewer], Some(&start))
                .context("gh pr edit --add-reviewer failed")?;
            pr.url
        }
        PrAction::Create => {
            let base = args.base.clone().unwrap_or_else(|| loaded.config.defaults.pr_base.clone());
            let title = args.pr_title.clone().context("--pr-title is required to create a PR")?;
            let body = args.pr_body.clone().unwrap_or_default();
            let out = capture(
                "gh",
                &["pr", "create", "--base", &base, "--reviewer", &reviewer, "--title", &title, "--body", &body],
                Some(&start),
            ).context("gh pr create failed")?;
            out.lines().rev().find(|l| l.contains("://")).unwrap_or("").trim().to_string()
        }
    };

    let text = compose_text(&args.body, &pr_url);

    match std::env::var("SLACK_TOKEN").ok().filter(|t| !t.is_empty()) {
        Some(token) => {
            slack::post_message(&token, &person.slack, &text)?;
            println!("Sent to {} ({}). PR: {pr_url}", args.to, person.slack);
        }
        None => {
            let intent = SlackIntent {
                slack_id: &person.slack,
                text: &text,
                pr_url: &pr_url,
                github: &reviewer,
                branch: &branch,
            };
            println!("{}", serde_json::to_string_pretty(&intent)?);
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Wire the `Review` subcommand in `main.rs`**

Add to `enum Cmd`:

```rust
    /// Push, open/reuse a PR, add a reviewer, and Slack them the body + PR link.
    Review {
        /// Slack message body (PR URL is appended automatically).
        body: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        reviewer: Option<String>,
        #[arg(long)]
        base: Option<String>,
        #[arg(long = "pr-title")]
        pr_title: Option<String>,
        #[arg(long = "pr-body")]
        pr_body: Option<String>,
        #[arg(long = "no-push")]
        no_push: bool,
    },
```

Add to the `match`:

```rust
        Some(Cmd::Review { body, to, reviewer, base, pr_title, pr_body, no_push }) =>
            review::run(review::ReviewArgs {
                body, to, reviewer, base, pr_title, pr_body, no_push,
                dir: cli.dir, config: cli.config,
            }),
```

- [ ] **Step 5: Run tests and clippy**

Run: `cargo test -p issue review`
Expected: PASS (`guard_rejects_base_branches`, `action_maps_pr_state`, `reviewer_prefers_explicit_then_alias`, `compose_appends_url`).

Run: `cargo clippy -p issue --all-targets -- -D warnings`
Expected: zero warnings.

- [ ] **Step 6: Smoke-test the JSON fallback (no SLACK_TOKEN, no push)**

Run from a worktree on a feature branch:
`env -u SLACK_TOKEN cargo run -p issue -- review "please review" --to igor --no-push`
Expected: with no PR it errors `--pr-title is required to create a PR` (correct guard), or with an open PR it prints a `SlackIntent` JSON object. It must never force-push.

- [ ] **Step 7: Commit**

```bash
git add crates/issue/src/review.rs crates/issue/src/main.rs
git commit -m "feat(issue): add review subcommand (push, PR, reviewer, slack)"
```

---

# Phase 3 — `issue dashboard` at-a-glance

The top section: the worktree-triage table plus the two PR tables, reusing `triage` and `prs`. `--no-plots` stops here. Timelines arrive in Phase 4.

## Task 3.1: Dashboard module that reuses triage + prs

**Files:**
- Create: `crates/issue/src/dashboard/mod.rs`
- Modify: `crates/issue/src/main.rs`

- [ ] **Step 1: Create the dashboard module with the at-a-glance view**

`crates/issue/src/dashboard/mod.rs`:

```rust
use crate::{prs, triage};
use anyhow::Result;

pub struct DashboardArgs {
    pub bucket: String,
    pub chart: String,
    pub mode: String,
    pub all_roles: bool,
    pub author: Option<String>,
    pub no_plots: bool,
    pub dir: Option<String>,
    pub config: Option<String>,
}

pub fn run(args: DashboardArgs) -> Result<()> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());

    // At-a-glance: worktree triage, then my PRs + PRs awaiting my review.
    let (rows, states, has_key, url_key) = triage::gather(&start, &[])?;
    triage::render(&rows, &states, has_key, url_key.as_deref());
    println!();
    prs::run(true, true, None, false)?;

    if args.no_plots {
        return Ok(());
    }

    // Timelines (Phase 4).
    Ok(())
}
```

- [ ] **Step 2: Wire the `Dashboard` subcommand in `main.rs`**

Add `mod dashboard;`. Add to `enum Cmd`:

```rust
    /// Combined at-a-glance view plus issue/PR/commit timelines.
    Dashboard {
        #[arg(long, default_value = "auto")]
        bucket: String,
        #[arg(long, default_value = "bar")]
        chart: String,
        #[arg(long, default_value = "absolute")]
        mode: String,
        #[arg(long = "all-roles")]
        all_roles: bool,
        #[arg(long)]
        author: Option<String>,
        #[arg(long = "no-plots")]
        no_plots: bool,
    },
```

Add to the `match`:

```rust
        Some(Cmd::Dashboard { bucket, chart, mode, all_roles, author, no_plots }) =>
            dashboard::run(dashboard::DashboardArgs {
                bucket, chart, mode, all_roles, author, no_plots,
                dir: cli.dir, config: cli.config,
            }),
```

(`bucket`/`chart`/`mode` are `String` with clap `default_value`; Phase 4 validates them. Using `String` avoids unused `ValueEnum` warnings now.)

- [ ] **Step 3: Build and smoke-test the at-a-glance view**

Run: `cargo run -p issue -- -C ~/Git/example/monorepo dashboard --no-plots`
Expected: prints the `ISSUE WORKTREES` table, then `MY OPEN PRs` and `PRs AWAITING MY REVIEW`; exit 0.

- [ ] **Step 4: clippy**

Run: `cargo clippy -p issue --all-targets -- -D warnings`
Expected: zero warnings. (This consumes the `pub(crate)` reuse seams from Phase 1 — remove any temporary `#[allow(dead_code)]` added in Task 1.7 Step 2 now that `triage::gather`/`render` and `prs::run` are called from the dashboard.)

- [ ] **Step 5: Commit**

```bash
git add crates/issue/src/dashboard/mod.rs crates/issue/src/main.rs
git commit -m "feat(issue): dashboard at-a-glance view (triage + PR tables)"
```

---

# Phase 4 — `issue dashboard` timelines

Pure date bucketing + state replay (`bucket.rs`), the live fetch (`data.rs`), the chart renderers (`chart.rs`), and wiring them into the dashboard. Build pure-and-tested first, then I/O.

## Task 4.1: Linear history query in `devkit-common::linear`

**Files:**
- Modify: `crates/devkit-common/src/linear.rs`

- [ ] **Step 1: Write a failing test for the assigned-issues query builder**

Split the query string out so it's testable without a network call. Add to `linear.rs`'s `tests` module:

```rust
    #[test]
    fn assigned_query_paginates() {
        assert!(assigned_query(None).contains("issues(first: 50"));
        assert!(assigned_query(None).contains("assignee: { isMe: { eq: true } }"));
        assert!(assigned_query(Some("CUR")).contains("after: \"CUR\""));
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p devkit-common assigned_query_paginates`
Expected: FAIL — `cannot find function assigned_query`.

- [ ] **Step 3: Add the types, query builder, and fetchers**

Add to `crates/devkit-common/src/linear.rs`:

```rust
#[derive(Debug, Clone, serde::Deserialize)]
pub struct StateRef {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub color: String,
}

#[derive(Debug, Clone)]
pub struct AssignedIssue {
    pub identifier: String,
    pub created_at: String,
    pub state: StateRef,
    /// (createdAt, fromState, toState) for each recorded transition, unsorted.
    pub history: Vec<(String, Option<StateRef>, Option<StateRef>)>,
}

/// GraphQL for one page of issues assigned to me, with state + transition history.
fn assigned_query(after: Option<&str>) -> String {
    let cursor = match after {
        Some(c) => format!(", after: \"{c}\""),
        None => String::new(),
    };
    format!(
        "query {{ issues(first: 50{cursor}, filter: {{ assignee: {{ isMe: {{ eq: true }} }} }}) \
         {{ nodes {{ identifier createdAt \
         state {{ name type color }} \
         history(first: 50) {{ nodes {{ createdAt \
         fromState {{ name type color }} toState {{ name type color }} }} }} }} \
         pageInfo {{ hasNextPage endCursor }} }} }}"
    )
}

/// Every issue assigned to me, paginated. Empty on no key / network error.
pub fn assigned_issue_history(key: &str) -> Result<Vec<AssignedIssue>> {
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
        if block["pageInfo"]["hasNextPage"].as_bool() == Some(true) {
            after = block["pageInfo"]["endCursor"].as_str().map(String::from);
        } else {
            return Ok(out);
        }
    }
}

/// createdAt of my Linear account — the timeline origin.
pub fn viewer_created_at(key: &str) -> Result<String> {
    let resp: serde_json::Value = ureq::post("https://api.linear.app/graphql")
        .set("Authorization", key)
        .send_json(ureq::json!({ "query": "query { viewer { createdAt } }" }))?
        .into_json()?;
    resp["data"]["viewer"]["createdAt"]
        .as_str()
        .map(String::from)
        .context("viewer.createdAt missing from Linear response")
}
```

Add `use anyhow::Context;` to the imports if not already present (the file currently imports `anyhow::Result` only — change to `use anyhow::{Context, Result};`).

- [ ] **Step 4: Run the test**

Run: `cargo test -p devkit-common assigned_query_paginates`
Expected: PASS. Also `cargo test -p devkit-common linear` — existing `query_aliases_each_id`, `empty_ids_no_query` still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-common/src/linear.rs
git commit -m "feat(common): Linear assigned-issue history + viewer origin queries"
```

## Task 4.2: Pure date bucketing in `dashboard/bucket.rs`

**Files:**
- Create: `crates/issue/src/dashboard/bucket.rs`
- Modify: `crates/issue/src/dashboard/mod.rs`

Ports the Python `bucket_starts`, `bucket_index`, `label_for`, `choose_bucket`, plus the issue state-replay (`parse_issue` + `state_at`) and the `tally` helper. All pure, all tested with fixed clocks.

- [ ] **Step 1: Write failing tests**

Create `crates/issue/src/dashboard/bucket.rs`:

```rust
use chrono::{DateTime, Datelike, Duration, Months, NaiveDate, Utc};
use devkit_common::linear::{AssignedIssue, StateRef};
use std::collections::HashMap;

/// Parse an RFC3339 timestamp to UTC. Linear uses `…Z`; git `%aI` uses `+01:00`.
pub fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc))
}

fn midnight(d: DateTime<Utc>) -> DateTime<Utc> {
    d.date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc()
}

/// Period-start datetimes spanning first..=now for the chosen bucket.
pub fn bucket_starts(first: DateTime<Utc>, now: DateTime<Utc>, bucket: &str) -> Vec<DateTime<Utc>> {
    let start = match bucket {
        "day" => midnight(first),
        "month" => NaiveDate::from_ymd_opt(first.year(), first.month(), 1)
            .unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc(),
        _ => {
            let m = midnight(first);
            m - Duration::days(m.weekday().num_days_from_monday() as i64)
        }
    };
    let step = |d: DateTime<Utc>| match bucket {
        "day" => d + Duration::days(1),
        "month" => d + Months::new(1),
        _ => d + Duration::days(7),
    };
    let mut out = Vec::new();
    let mut cur = start;
    while cur <= now {
        out.push(cur);
        cur = step(cur);
    }
    out
}

/// Index of the period containing `t`, or None if `t` precedes the first period.
pub fn bucket_index(starts: &[DateTime<Utc>], t: DateTime<Utc>) -> Option<usize> {
    let mut idx = None;
    for (i, s) in starts.iter().enumerate() {
        if *s <= t { idx = Some(i); } else { break; }
    }
    idx
}

pub fn label_for(start: DateTime<Utc>, bucket: &str) -> String {
    if bucket == "month" { start.format("%b %Y").to_string() } else { start.format("%b %d").to_string() }
}

/// Finest bucket whose bar count fits `width`: day, else week, else month.
pub fn choose_bucket(first: DateTime<Utc>, now: DateTime<Utc>, width: usize) -> &'static str {
    let span_days = (now - first).num_days() + 1;
    let max_bars = std::cmp::max(8, (width.saturating_sub(12)) / 2) as i64;
    if span_days <= max_bars { "day" }
    else if span_days / 7 <= max_bars { "week" }
    else { "month" }
}

/// Tally timestamps into per-bucket counts.
pub fn tally(starts: &[DateTime<Utc>], dates: &[DateTime<Utc>]) -> Vec<u32> {
    let mut counts = vec![0u32; starts.len()];
    for d in dates {
        if let Some(i) = bucket_index(starts, *d) {
            counts[i] += 1;
        }
    }
    counts
}

// --- issue state replay ---------------------------------------------------------

/// A single issue reduced to: created time, the state before its first transition,
/// and its transitions sorted ascending by time.
pub struct Replay {
    pub created: Option<DateTime<Utc>>,
    pub initial: String,
    pub transitions: Vec<(DateTime<Utc>, String)>,
}

/// Build a `Replay` and record every state's (kind, color) into `meta`.
pub fn parse_issue(iss: &AssignedIssue, meta: &mut HashMap<String, (String, String)>) -> Replay {
    meta.entry(iss.state.name.clone())
        .or_insert((iss.state.kind.clone(), iss.state.color.clone()));
    let mut raw: Vec<(DateTime<Utc>, Option<String>, String)> = Vec::new();
    for (when, from, to) in &iss.history {
        for s in [from, to].into_iter().flatten() {
            meta.entry(s.name.clone()).or_insert((s.kind.clone(), s.color.clone()));
        }
        if let (Some(t), Some(to_state)) = (parse_ts(when), to) {
            raw.push((t, from.as_ref().map(|s: &StateRef| s.name.clone()), to_state.name.clone()));
        }
    }
    raw.sort_by_key(|x| x.0);
    let initial = raw.first().and_then(|(_, f, _)| f.clone())
        .unwrap_or_else(|| iss.state.name.clone());
    Replay {
        created: parse_ts(&iss.created_at),
        initial,
        transitions: raw.into_iter().map(|(t, _, to)| (t, to)).collect(),
    }
}

/// The issue's workflow state as of time `t`, or None if not yet created.
pub fn state_at(r: &Replay, t: DateTime<Utc>) -> Option<String> {
    match r.created {
        Some(c) if c <= t => {}
        _ => return None,
    }
    let mut state = r.initial.clone();
    for (when, to) in &r.transitions {
        if *when <= t { state = to.clone(); } else { break; }
    }
    Some(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dt(s: &str) -> DateTime<Utc> { parse_ts(s).unwrap() }

    #[test]
    fn daily_buckets_are_inclusive() {
        let starts = bucket_starts(dt("2026-01-01T08:00:00Z"), dt("2026-01-03T23:00:00Z"), "day");
        assert_eq!(starts.len(), 3);
        assert_eq!(label_for(starts[0], "day"), "Jan 01");
    }
    #[test]
    fn weekly_buckets_anchor_on_monday() {
        // 2026-01-01 is a Thursday; the week start is Monday 2025-12-29.
        let starts = bucket_starts(dt("2026-01-01T00:00:00Z"), dt("2026-01-10T00:00:00Z"), "week");
        assert_eq!(starts[0].weekday(), chrono::Weekday::Mon);
        assert_eq!(starts[0].format("%Y-%m-%d").to_string(), "2025-12-29");
    }
    #[test]
    fn monthly_steps_by_calendar_month() {
        let starts = bucket_starts(dt("2026-01-15T00:00:00Z"), dt("2026-03-02T00:00:00Z"), "month");
        assert_eq!(starts.len(), 3);
        assert_eq!(label_for(starts[1], "month"), "Feb 2026");
    }
    #[test]
    fn bucket_index_before_first_is_none() {
        let starts = bucket_starts(dt("2026-01-02T00:00:00Z"), dt("2026-01-04T00:00:00Z"), "day");
        assert_eq!(bucket_index(&starts, dt("2026-01-01T00:00:00Z")), None);
        assert_eq!(bucket_index(&starts, dt("2026-01-03T12:00:00Z")), Some(1));
    }
    #[test]
    fn choose_bucket_widens_with_span() {
        let first = dt("2026-01-01T00:00:00Z");
        assert_eq!(choose_bucket(first, dt("2026-01-05T00:00:00Z"), 100), "day");
        assert_eq!(choose_bucket(first, dt("2027-01-01T00:00:00Z"), 100), "week");
        assert_eq!(choose_bucket(first, dt("2031-01-01T00:00:00Z"), 100), "month");
    }
    #[test]
    fn tally_counts_per_bucket() {
        let starts = bucket_starts(dt("2026-01-01T00:00:00Z"), dt("2026-01-03T00:00:00Z"), "day");
        let dates = vec![dt("2026-01-01T05:00:00Z"), dt("2026-01-01T09:00:00Z"), dt("2026-01-03T01:00:00Z")];
        assert_eq!(tally(&starts, &dates), vec![2, 0, 1]);
    }
    #[test]
    fn state_at_replays_transitions() {
        let iss = AssignedIssue {
            identifier: "ENG-1".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            state: StateRef { name: "Done".into(), kind: "completed".into(), color: "#0f0".into() },
            history: vec![
                ("2026-01-02T00:00:00Z".into(),
                 Some(StateRef { name: "Todo".into(), kind: "unstarted".into(), color: "#888".into() }),
                 Some(StateRef { name: "In Progress".into(), kind: "started".into(), color: "#00f".into() })),
                ("2026-01-04T00:00:00Z".into(),
                 Some(StateRef { name: "In Progress".into(), kind: "started".into(), color: "#00f".into() }),
                 Some(StateRef { name: "Done".into(), kind: "completed".into(), color: "#0f0".into() })),
            ],
        };
        let mut meta = HashMap::new();
        let r = parse_issue(&iss, &mut meta);
        assert_eq!(r.initial, "Todo");
        assert_eq!(state_at(&r, dt("2025-12-31T00:00:00Z")), None);          // before creation
        assert_eq!(state_at(&r, dt("2026-01-01T12:00:00Z")).as_deref(), Some("Todo"));
        assert_eq!(state_at(&r, dt("2026-01-03T00:00:00Z")).as_deref(), Some("In Progress"));
        assert_eq!(state_at(&r, dt("2026-01-05T00:00:00Z")).as_deref(), Some("Done"));
        assert!(meta.contains_key("In Progress"));
    }
}
```

- [ ] **Step 2: Declare the submodule**

In `crates/issue/src/dashboard/mod.rs`, add at the top: `mod bucket;` (and `mod chart;`, `mod data;` placeholders are added in their tasks).

- [ ] **Step 3: Run the bucket tests**

Run: `cargo test -p issue bucket`
Expected: PASS — all eight tests (`daily_buckets_are_inclusive`, `weekly_buckets_anchor_on_monday`, `monthly_steps_by_calendar_month`, `bucket_index_before_first_is_none`, `choose_bucket_widens_with_span`, `tally_counts_per_bucket`, `state_at_replays_transitions`).

- [ ] **Step 4: Commit**

```bash
git add crates/issue/src/dashboard/bucket.rs crates/issue/src/dashboard/mod.rs
git commit -m "feat(issue): pure date bucketing and issue state replay"
```

## Task 4.3: Chart rendering in `dashboard/chart.rs`

**Files:**
- Create: `crates/issue/src/dashboard/chart.rs`
- Modify: `crates/issue/src/dashboard/mod.rs`

The hand-rolled stacked/grouped bar renderer (pure cell allocation + ANSI I/O) and the textplots line renderer, plus terminal width.

- [ ] **Step 1: Write a failing test for the pure cell allocator**

Create `crates/issue/src/dashboard/chart.rs`:

```rust
use chrono::{DateTime, Datelike, Utc};
use textplots::{Chart, ColorPlot, Shape};

/// (r,g,b) parsed from a Linear `#rrggbb` hex; falls back to mid-grey.
pub fn hex_rgb(hex: &str) -> (u8, u8, u8) {
    let h = hex.trim_start_matches('#');
    if h.len() >= 6 {
        let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(128);
        (p(0), p(2), p(4))
    } else {
        (128, 128, 128)
    }
}

/// Allocate `rows` vertical cells among stacked segment `values`, scaled so the
/// tallest possible column (`max_total`) fills `rows`. Largest-remainder rounding
/// keeps the visible cell total faithful. Returns segment indices bottom→top.
pub fn stack_column(values: &[u32], max_total: u32, rows: usize) -> Vec<usize> {
    let total: u32 = values.iter().sum();
    if total == 0 || max_total == 0 || rows == 0 {
        return Vec::new();
    }
    let scale = rows as f64 / max_total as f64;
    // Ideal (fractional) cell height per segment.
    let ideal: Vec<f64> = values.iter().map(|&v| v as f64 * scale).collect();
    let target: usize = ideal.iter().sum::<f64>().round() as usize;
    let mut floors: Vec<usize> = ideal.iter().map(|x| x.floor() as usize).collect();
    let mut assigned: usize = floors.iter().sum();
    // Distribute the remaining cells to the largest fractional remainders.
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&a, &b| (ideal[b] - ideal[b].floor()).partial_cmp(&(ideal[a] - ideal[a].floor())).unwrap());
    let mut oi = 0;
    while assigned < target && !order.is_empty() {
        floors[order[oi % order.len()]] += 1;
        assigned += 1;
        oi += 1;
    }
    let mut cells = Vec::with_capacity(target);
    for (idx, &h) in floors.iter().enumerate() {
        for _ in 0..h {
            cells.push(idx);
        }
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hex_rgb_parses() {
        assert_eq!(hex_rgb("#ff8800"), (255, 136, 0));
        assert_eq!(hex_rgb("bad"), (128, 128, 128));
    }
    #[test]
    fn stack_column_scales_to_max() {
        // Tallest column (max_total=4) fills all 4 rows.
        assert_eq!(stack_column(&[4], 4, 4).len(), 4);
        // Half-height column fills ~2 of 4 rows.
        assert_eq!(stack_column(&[2], 4, 4).len(), 2);
        // Two segments split proportionally, indices bottom→top.
        assert_eq!(stack_column(&[2, 2], 4, 4), vec![0, 0, 1, 1]);
    }
    #[test]
    fn stack_column_empty_when_zero() {
        assert!(stack_column(&[0, 0], 4, 4).is_empty());
        assert!(stack_column(&[1], 4, 0).is_empty());
    }
}
```

- [ ] **Step 2: Run the allocator tests**

In `dashboard/mod.rs` add `mod chart;` (if not present), then run: `cargo test -p issue chart`
Expected: PASS — `hex_rgb_parses`, `stack_column_scales_to_max`, `stack_column_empty_when_zero`.

- [ ] **Step 3: Add the renderers and terminal width (I/O, not unit-tested)**

Append to `chart.rs`:

```rust
const BLOCK_HEIGHT: usize = 12;

/// Terminal width: $COLUMNS, else TIOCGWINSZ, else 100.
pub fn term_width() -> usize {
    if let Ok(c) = std::env::var("COLUMNS") {
        if let Ok(n) = c.trim().parse::<usize>() {
            if n > 0 { return n; }
        }
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let mut ws: libc_winsize = libc_winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
        let fd = std::io::stdout().as_raw_fd();
        // SAFETY: ws is a plain POD struct sized for struct winsize; TIOCGWINSZ fills it.
        let rc = unsafe { ioctl_winsize(fd, &mut ws) };
        if rc == 0 && ws.ws_col > 0 {
            return ws.ws_col as usize;
        }
    }
    100
}

#[repr(C)]
struct libc_winsize { ws_row: u16, ws_col: u16, ws_xpixel: u16, ws_ypixel: u16 }

#[cfg(unix)]
unsafe fn ioctl_winsize(fd: i32, ws: *mut libc_winsize) -> i32 {
    // TIOCGWINSZ is 0x5413 on Linux.
    unsafe extern "C" { fn ioctl(fd: i32, request: u64, ...) -> i32; }
    unsafe { ioctl(fd, 0x5413, ws) }
}

fn ansi(rgb: (u8, u8, u8), s: &str) -> String {
    format!("\x1b[38;2;{};{};{}m{s}\x1b[0m", rgb.0, rgb.1, rgb.2)
}

/// Render stacked vertical bars. `series[k][b]` = value of status k in bucket b.
pub fn render_stacked_bars(
    title: &str,
    labels: &[String],
    series: &[Vec<u32>],
    names: &[String],
    colors: &[(u8, u8, u8)],
    starts: &[DateTime<Utc>],
    daily_gridlines: bool,
) {
    println!("\n{title}");
    let n = labels.len();
    let max_total: u32 = (0..n).map(|b| series.iter().map(|s| s[b]).sum::<u32>()).max().unwrap_or(0);
    // Build each bucket's bottom→top cell stack.
    let columns: Vec<Vec<usize>> = (0..n)
        .map(|b| stack_column(&series.iter().map(|s| s[b]).collect::<Vec<_>>(), max_total, BLOCK_HEIGHT))
        .collect();
    for row in (0..BLOCK_HEIGHT).rev() {
        let mut line = String::new();
        for (b, col) in columns.iter().enumerate() {
            // A faint separator just before each Monday in daily resolution.
            if daily_gridlines && b > 0 && starts[b].weekday() == chrono::Weekday::Mon {
                line.push_str(&ansi((99, 105, 122), "│"));
            } else if b > 0 {
                line.push(' ');
            }
            match col.get(row) {
                Some(&k) => line.push_str(&ansi(colors[k], "█")),
                None => line.push(' '),
            }
        }
        println!("{line}");
    }
    // Sparse x labels (~every other) and a legend.
    let step = std::cmp::max(1, n / 10);
    let mut axis = String::new();
    for (b, lab) in labels.iter().enumerate() {
        if b % step == 0 {
            axis.push_str(lab);
            axis.push(' ');
        }
    }
    println!("{axis}");
    let legend: Vec<String> = names.iter().zip(colors).map(|(nm, c)| ansi(*c, &format!("■ {nm}"))).collect();
    println!("{}", legend.join("  "));
}

/// Render one non-stacked line per series via textplots (braille canvas).
pub fn render_lines(title: &str, series: &[Vec<u32>], names: &[String], colors: &[(u8, u8, u8)]) {
    println!("\n{title}");
    let n = series.first().map(|s| s.len()).unwrap_or(0);
    if n == 0 {
        println!("  (no data)");
        return;
    }
    let width = (term_width().saturating_sub(12)).clamp(40, 220) as u32;
    let points: Vec<Vec<(f32, f32)>> = series
        .iter()
        .map(|s| s.iter().enumerate().map(|(i, &v)| (i as f32, v as f32)).collect())
        .collect();
    let mut chart = Chart::new(width * 2, 60, 0.0, (n.saturating_sub(1)) as f32);
    // textplots' builder borrows each Shape for the chart's lifetime.
    let shapes: Vec<Shape> = points.iter().map(|p| Shape::Lines(p)).collect();
    let mut plot = &mut chart;
    for (sh, col) in shapes.iter().zip(colors) {
        plot = plot.linecolorplot(sh, rgb::RGB8::new(col.0, col.1, col.2));
    }
    plot.display();
    let legend: Vec<String> = names.iter().zip(colors).map(|(nm, c)| ansi(*c, &format!("─ {nm}"))).collect();
    println!("{}", legend.join("  "));
}
```

Add `use textplots::ColorPlot;` is already in the imports; also add `rgb` — textplots re-exports it. If `rgb::RGB8` is not in scope, import via `use textplots::rgb;` or add the `rgb` crate. Confirm the exact path during Step 4 (textplots 0.8 re-exports `rgb`).

- [ ] **Step 4: Build and verify the chart module compiles**

Run: `cargo build -p issue`
Expected: compiles. If `rgb::RGB8` does not resolve, add `rgb = "0.8"` to `crates/issue/Cargo.toml` and the workspace deps, and `use rgb::RGB8;`. If textplots' `ColorPlot`/`linecolorplot` signature differs, adjust to the 0.8 API (the chained-builder pattern shown matches `ColorPlot::linecolorplot(&mut self, &Shape, RGB8) -> &mut Chart`).

- [ ] **Step 5: Run chart tests + clippy**

Run: `cargo test -p issue chart`
Expected: PASS.

Run: `cargo clippy -p issue --all-targets -- -D warnings`
Expected: zero warnings (render functions are `pub` and consumed in Task 4.5; if clippy flags them as unused before then, complete 4.5 in the same phase before the phase-end gate).

- [ ] **Step 6: Commit**

```bash
git add crates/issue/src/dashboard/chart.rs crates/issue/src/dashboard/mod.rs crates/issue/Cargo.toml Cargo.toml
git commit -m "feat(issue): terminal bar and line chart rendering"
```

## Task 4.4: Live timeline fetch in `dashboard/data.rs`

**Files:**
- Create: `crates/issue/src/dashboard/data.rs`
- Modify: `crates/issue/src/dashboard/mod.rs`

Thin I/O layer: Linear assigned-issue history, gh PR list (opened/merged stamps), git commit dates. No pure logic worth unit-testing lives here; the tested transforms are in `bucket.rs`.

- [ ] **Step 1: Create `data.rs`**

`crates/issue/src/dashboard/data.rs`:

```rust
use anyhow::Result;
use chrono::{DateTime, Utc};
use devkit_common::cmd::{capture, gh_json};
use devkit_common::linear::{self, AssignedIssue};
use serde::Deserialize;

use super::bucket::parse_ts;

/// Linear issues assigned to me, with history (empty if no key / on error).
pub fn issues() -> Vec<AssignedIssue> {
    let Some(key) = std::env::var("LINEAR_API_KEY").ok() else { return Vec::new() };
    match linear::assigned_issue_history(&key) {
        Ok(v) => v,
        Err(e) => { eprintln!("Linear history fetch failed: {e}"); Vec::new() }
    }
}

/// Timeline origin: my Linear account creation, else the earliest issue createdAt.
pub fn origin(issues: &[AssignedIssue]) -> Option<DateTime<Utc>> {
    if let Some(key) = std::env::var("LINEAR_API_KEY").ok() {
        if let Ok(s) = linear::viewer_created_at(&key) {
            if let Some(d) = parse_ts(&s) { return Some(d); }
        }
    }
    issues.iter().filter_map(|i| parse_ts(&i.created_at)).min()
}

#[derive(Deserialize)]
struct PrTimes {
    #[serde(rename = "createdAt", default)]
    created_at: Option<String>,
    #[serde(rename = "mergedAt", default)]
    merged_at: Option<String>,
    #[serde(default)]
    additions: i64,
    #[serde(default)]
    deletions: i64,
}

/// (opened stamps, merged stamps, total additions, total deletions) for my PRs.
pub fn pr_timeline(all_roles: bool) -> (Vec<DateTime<Utc>>, Vec<DateTime<Utc>>, i64, i64) {
    let fetch = |search: &str| -> Vec<PrTimes> {
        gh_json(
            &["pr", "list", "--search", search, "--state", "all", "--limit", "500",
              "--json", "createdAt,mergedAt,additions,deletions"],
            ".",
        ).unwrap_or_default()
    };
    let mut prs = fetch("author:@me");
    if all_roles {
        prs.extend(fetch("reviewed-by:@me"));
    }
    let opened: Vec<_> = prs.iter().filter_map(|p| p.created_at.as_deref().and_then(parse_ts)).collect();
    let merged: Vec<_> = prs.iter().filter_map(|p| p.merged_at.as_deref().and_then(parse_ts)).collect();
    let add = prs.iter().map(|p| p.additions).sum();
    let del = prs.iter().map(|p| p.deletions).sum();
    (opened, merged, add, del)
}

/// Author-dates of every commit by `author` in `repo`.
pub fn commit_dates(repo: &str, author: &str) -> Vec<DateTime<Utc>> {
    let out = capture("git", &["-C", repo, "log", &format!("--author={author}"), "--format=%aI"], None)
        .unwrap_or_default();
    out.lines().filter_map(|l| parse_ts(l.trim())).collect()
}
```

- [ ] **Step 2: Declare the submodule and build**

In `dashboard/mod.rs` add `mod data;`. Run: `cargo build -p issue`
Expected: compiles.

- [ ] **Step 3: Commit**

```bash
git add crates/issue/src/dashboard/data.rs crates/issue/src/dashboard/mod.rs
git commit -m "feat(issue): live dashboard data fetch (Linear/gh/git)"
```

## Task 4.5: Wire the timelines into the dashboard

**Files:**
- Modify: `crates/issue/src/dashboard/mod.rs`

Assemble: fetch → bucket → render the three timelines + footer. Replace the Phase-3 `// Timelines (Phase 4).` placeholder.

- [ ] **Step 1: Implement timeline assembly**

Replace the body after the `if args.no_plots { return Ok(()); }` guard in `dashboard/mod.rs` with:

```rust
    use chrono::Utc;
    use std::collections::HashMap;
    let now: chrono::DateTime<Utc> = std::time::SystemTime::now().into();
    let width = chart::term_width();

    // --- Issues by status over time ---
    let issues = data::issues();
    if issues.is_empty() {
        println!("\n(no Linear issues — set LINEAR_API_KEY for the issue timeline)");
    } else if let Some(first) = data::origin(&issues) {
        let b = if args.bucket == "auto" { bucket::choose_bucket(first, now, width).to_string() } else { args.bucket.clone() };
        let starts = bucket::bucket_starts(first, now, &b);
        let ends: Vec<_> = (0..starts.len())
            .map(|i| std::cmp::min(*starts.get(i + 1).unwrap_or(&now), now))
            .collect();
        let labels: Vec<String> = starts.iter().map(|s| bucket::label_for(*s, &b)).collect();

        let mut meta: HashMap<String, (String, String)> = HashMap::new();
        let replays: Vec<_> = issues.iter().map(|i| bucket::parse_issue(i, &mut meta)).collect();

        // Lifecycle stacking order: type rank, then name.
        let type_rank = |k: &str| match k {
            "triage" => 0, "backlog" => 1, "unstarted" => 2,
            "started" => 3, "completed" => 4, "canceled" => 5, _ => 99,
        };
        let mut names: Vec<String> = meta.keys().cloned().collect();
        names.sort_by(|a, b| {
            type_rank(&meta[a].0).cmp(&type_rank(&meta[b].0)).then_with(|| a.cmp(b))
        });

        let mut series: Vec<Vec<u32>> = names.iter().map(|_| vec![0u32; starts.len()]).collect();
        for (si, name) in names.iter().enumerate() {
            for (bi, end) in ends.iter().enumerate() {
                series[si][bi] = replays.iter()
                    .filter(|r| bucket::state_at(r, *end).as_deref() == Some(name.as_str()))
                    .count() as u32;
            }
        }
        // Drop statuses that never appear.
        let keep: Vec<usize> = (0..names.len()).filter(|&i| series[i].iter().any(|&v| v > 0)).collect();
        let names: Vec<String> = keep.iter().map(|&i| names[i].clone()).collect();
        let mut series: Vec<Vec<u32>> = keep.iter().map(|&i| series[i].clone()).collect();
        let colors: Vec<(u8, u8, u8)> = names.iter().map(|n| chart::hex_rgb(&meta[n].1)).collect();

        if args.mode == "proportional" {
            for bi in 0..starts.len() {
                let total: u32 = series.iter().map(|s| s[bi]).sum();
                if total > 0 {
                    for s in series.iter_mut() {
                        s[bi] = (s[bi] as f64 / total as f64 * 100.0).round() as u32;
                    }
                }
            }
        }

        let title = format!("My Linear issues by status — per {b}, {}", args.mode);
        if args.chart == "line" {
            chart::render_lines(&title, &series, &names, &colors);
        } else {
            chart::render_stacked_bars(&title, &labels, &series, &names, &colors, &starts, b == "day");
        }
    }

    // Footer for issues.
    let open_now = issues.iter().filter(|i| i.state.kind != "completed" && i.state.kind != "canceled").count();
    if !issues.is_empty() {
        println!("\nTotal assigned: {}   open now: {open_now}", issues.len());
    }

    // --- PRs opened/merged + commits over time ---
    let (opened, merged, add, del) = data::pr_timeline(args.all_roles);
    let author = match args.author.clone() {
        Some(a) => a,
        None => capture_email(&start),
    };
    let monorepo = format!(
        "{}/monorepo",
        devkit_ports::config::expand_tilde(&loaded_worktree_root(&args)?).to_string_lossy()
    );
    let commits = data::commit_dates(&monorepo, &author);

    let mut stamps: Vec<chrono::DateTime<Utc>> = Vec::new();
    stamps.extend(opened.iter().copied());
    stamps.extend(merged.iter().copied());
    stamps.extend(commits.iter().copied());
    if let Some(&first) = stamps.iter().min() {
        let b = if args.bucket == "auto" { bucket::choose_bucket(first, now, width).to_string() } else { args.bucket.clone() };
        let starts = bucket::bucket_starts(first, now, &b);
        let labels: Vec<String> = starts.iter().map(|s| bucket::label_for(*s, &b)).collect();
        let c_commits = bucket::tally(&starts, &commits);
        let c_opened = bucket::tally(&starts, &opened);
        let c_merged = bucket::tally(&starts, &merged);

        let cyan = (0u8, 200u8, 200u8);
        let orange = (255u8, 150u8, 0u8);
        let green = (0u8, 200u8, 0u8);
        if args.chart == "line" {
            chart::render_lines(&format!("Commits per {b}"), &[c_commits.clone()], &["commits".into()], &[cyan]);
            chart::render_lines(&format!("PRs per {b}"), &[c_opened.clone(), c_merged.clone()],
                &["opened".into(), "merged".into()], &[orange, green]);
        } else {
            chart::render_stacked_bars(&format!("Commits per {b}"), &labels, &[c_commits.clone()],
                &["commits".into()], &[cyan], &starts, b == "day");
            chart::render_stacked_bars(&format!("PRs opened/merged per {b}"), &labels,
                &[c_opened.clone(), c_merged.clone()], &["opened".into(), "merged".into()],
                &[orange, green], &starts, b == "day");
        }
    }
    println!(
        "\nPRs: {} opened, {} merged   Commits: {}   Lines: +{add} / -{del}",
        opened.len(), merged.len(), commits.len()
    );
    Ok(())
```

Add these helpers at the bottom of `dashboard/mod.rs`:

```rust
fn capture_email(start: &str) -> String {
    devkit_common::cmd::git(&["config", "user.email"], start)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// The configured worktree_root (for locating the monorepo where commits land).
fn loaded_worktree_root(args: &DashboardArgs) -> anyhow::Result<String> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());
    let loaded = devkit_ports::load::load(
        args.config.as_deref().map(std::path::Path::new),
        std::path::Path::new(&start),
    )?;
    Ok(loaded.config.defaults.worktree_root.clone())
}
```

Add the needed imports at the top of `mod.rs`: `use devkit_common::cmd::capture;` and `use devkit_ports;` are referenced; ensure `use crate::dashboard::{bucket, chart, data};` resolves via the `mod` declarations (they're submodules, so reference them as `bucket::`, `chart::`, `data::` directly — already declared `mod bucket; mod chart; mod data;`).

- [ ] **Step 2: Build**

Run: `cargo build -p issue`
Expected: compiles. Resolve any borrow/import errors (e.g. `capture` import) until clean.

- [ ] **Step 3: clippy**

Run: `cargo clippy -p issue --all-targets -- -D warnings`
Expected: zero warnings.

- [ ] **Step 4: Smoke-test all four render paths**

```bash
cargo run -p issue -- -C ~/Git/example/monorepo dashboard --bucket month
cargo run -p issue -- -C ~/Git/example/monorepo dashboard --bucket month --chart line
cargo run -p issue -- -C ~/Git/example/monorepo dashboard --bucket week --mode proportional
cargo run -p issue -- -C ~/Git/example/monorepo dashboard --no-plots
```
Expected: each prints the at-a-glance tables; the first three add the issue/commit/PR charts and a footer (`Total assigned …`, `PRs: … Commits: … Lines: …`). With `LINEAR_API_KEY` unset, the issue chart is replaced by the "set LINEAR_API_KEY" note but PR/commit charts still render. No panics.

- [ ] **Step 5: Full gate**

Run: `cargo test --workspace`
Expected: all green.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: zero warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/issue/src/dashboard/mod.rs
git commit -m "feat(issue): assemble dashboard issue/PR/commit timelines"
```

---

# Phase 5 — Migration note for external callers

Inspect the external callers and write an uncommitted migration note into the **base** repo (`/home/lev/Git/lev/devkit`, not this worktree) with concrete old → new mappings. Nothing outside this worktree's tracked files is edited.

## Task 5.1: Inspect external callers and write the migration note

**Files:**
- Create (uncommitted, base repo): `/home/lev/Git/lev/devkit/docs/issue-binary-migration.md`

- [ ] **Step 1: Find every external reference to the old binary names**

```bash
rg -n "issue-prep|issue-end|pr-status" ~/.claude/commands ~/.claude/scripts ~/.local/bin 2>/dev/null
```
Record each file and the exact command line it uses.

- [ ] **Step 2: Write the migration note**

Create `/home/lev/Git/lev/devkit/docs/issue-binary-migration.md` (the base repo's `docs/`, **not** this worktree). Use this skeleton, filling the "Found in" lines from Step 1's actual output:

```markdown
# Migrating callers to the consolidated `issue` binary

`issue-prep`, `issue-end`, and `pr-status` are gone; all of it now lives under one
`issue` binary. Update these callers by hand (this file is intentionally uncommitted).

| Old command | New command |
|---|---|
| `issue-prep --issue X --slug Y --apps a,b` | `issue setup --issue X --slug Y --apps a,b` |
| `issue-end` / `issue-end status` | `issue status` |
| `issue-end clean [flags]` | `issue end [flags]` |
| `pr-status [-m|-r|-R|--no-cache]` | `issue prs [-m|-r|-R|--no-cache]` |

The global `-C/--dir` and `--config` flags now sit on `issue` itself (before the
subcommand), e.g. `issue -C ~/Git/example/monorepo status`.

## Files to update
<!-- one bullet per hit from `rg`, with the specific line to change -->
- `~/.claude/commands/issue-setup.md` — replace `issue-prep …` with `issue setup …`
- `~/.claude/commands/issue-end.md` — replace `issue-end status`/`issue-end clean` with `issue status`/`issue end`
- `~/.claude/commands/migration-review.md` — the mechanical push/PR/Slack steps are now `issue review "<body>" --to <alias> [--pr-title …]`; the command keeps authoring prose and, when `$SLACK_TOKEN` is unset, forwards the emitted `SlackIntent` JSON via the Slack MCP tool.
- `~/.claude/scripts/issue-end-scan.sh`, `issue-end-cleanup.sh` — repoint to `issue status` / `issue end`; note if the binary makes the script redundant.
- `~/.local/bin/pr-status.sh` (if present) — repoint to `issue prs`.

## New: `issue dashboard`
`issue dashboard [--bucket auto|day|week|month] [--chart bar|line] [--mode absolute|proportional] [--all-roles] [--author <email>] [--no-plots]`
replaces the Python `dashboard_issues.py` / `dashboard_prs.py` (live data; archive seam later).
```

Remove any bullet whose file did not appear in Step 1, and add any that did.

- [ ] **Step 3: Verify the note exists and is outside version control**

```bash
test -f /home/lev/Git/lev/devkit/docs/issue-binary-migration.md && echo present
git -C /home/lev/Git/lev/devkit status --porcelain docs/issue-binary-migration.md
```
Expected: `present`; the status line shows it as untracked (`??`) — confirming it is not committed.

- [ ] **Step 4: No commit**

This note is deliberately uncommitted. Do not `git add` it. Phase 5 produces no commit in this worktree.

---

## Self-Review

Checked against the spec (`2026-06-20-issue-binary-consolidation-design.md`):

- **CLI surface** — every subcommand mapped: `setup` (1.2), `status` (1.4), `end` (1.5), `prs` (1.6), `dashboard` (3.1 + 4.5), `review` (2.3); bare `issue` → `status` (1.4 Step 2). ✓
- **Module layout** — matches the spec's tree: `main/setup/triage/status/end/prs/review` + `dashboard/{mod,data,bucket,chart}`. ✓
- **Shared lib additions** — `linear::assigned_issue_history` + `viewer_created_at` (4.1), `slack::post_message` (2.2), config `[people]` + `defaults.pr_base` (2.1). ✓
- **Reuse seams** — `triage` items `pub(crate)` (1.3); `prs` fetch/render `pub(crate)` (1.6); dashboard consumes both (3.1, 4.5). ✓
- **Charting** — bars hand-rolled (`stack_column` + `render_stacked_bars`, tested allocator), lines via `textplots` (`render_lines`); both `--chart` modes wired (4.5). ✓
- **Bucketing fidelity** — day/Monday-week/month, `auto` = `max(8,(width-12)/2)`, weekly gridlines, proportional mode, footer — all ported and tested against fixed clocks (4.2, 4.5). ✓
- **review semantics** — branch guard, never force-push, create-vs-reuse, reviewer default from alias, both-mode Slack delivery (2.3). ✓
- **Invariants** — registry path unchanged (setup is a verbatim port); `prd` rejection untouched; diff-cache format preserved (prs verbatim); panic hook installs as `"issue"`. ✓
- **Caller updates** — out of scope; documented via uncommitted note (Phase 5). ✓
- **Placeholder scan** — no TBD/TODO; every code step shows complete code or a precise move instruction with the exact edits.
- **Type consistency** — `Row`/`Pr` fields `pub(crate)`; `AssignedIssue`/`StateRef` shared between `linear` and `bucket`; `Person` constructed in `review` tests uses the `pub` fields from 2.1; `DashboardArgs`/`SetupArgs`/`ReviewArgs` field names consistent across `main.rs` dispatch and module `run` signatures.

**Two implementation risks flagged for the executor (verify during the relevant task, not blockers):**
1. **textplots 0.8 API** (Task 4.3 Step 4): the `ColorPlot::linecolorplot` chained-builder signature and the `rgb::RGB8` path are asserted from the crate's documented 0.8 surface; if they differ, adapt the call and (if needed) add the `rgb` crate. The bucketing/allocation logic — the tested part — does not depend on this.
2. **TIOCGWINSZ ioctl** (Task 4.3 Step 3): hand-declared rather than via a `libc` dep, to honor the minimal-dependency ethos. `$COLUMNS` is checked first and a 100-col fallback always applies, so a wrong ioctl constant degrades gracefully rather than breaking output.

## Note on a second new dependency

The spec named `textplots` as the only new dependency. This plan adds a **second**: `chrono` (date bucketing), because faithfully reproducing the Python calendar math (Monday-anchored weeks, calendar-month stepping, weekday gridlines, RFC3339 parsing of both Linear `Z` and git `+01:00` stamps) by hand is exactly the "hard, so use a maintained crate" case the user authorized. It is added with `default-features = false, features = ["std"]` to avoid the `clock`/`iana-time-zone` chain; current time comes from `SystemTime`. Flag this to the user at execution handoff.
