# Design: consolidate the issue tooling into one `issue` binary

**Date:** 2026-06-20
**Status:** approved

## Goal

Replace three separate binaries — `issue-prep`, `issue-end`, `pr-status` — with a
single `issue` binary that groups the whole issue lifecycle under subcommands, and
add two new capabilities: a `dashboard` view (an at-a-glance triage plus ported
terminal timeline plots) and a `review` command that mechanizes the deterministic
parts of shipping a worktree for review.

The engine stays project-agnostic; every project-specific value (people aliases, PR
base branch) lives in `devkit.toml`, consistent with the existing config-driven
conventions.

## CLI surface

One binary, `issue`, with a global `-C/--dir` flag (carried over from `issue-end` /
`issue-prep`). Bare `issue` with no subcommand runs `issue status` (preserves
`issue-end`'s current default).

| Subcommand | Replaces / Source | Synopsis |
|---|---|---|
| `issue setup` | `issue-prep` | `--issue <ID> --slug <s> --apps a,b [--dry-run] [--config <p>]` |
| `issue status` | `issue-end status` | `[ids…]` — worktree triage table |
| `issue end` | `issue-end clean` | `[ids…] [-y] [--force] [--pr-only] [--clean-worktree]` |
| `issue prs` | `pr-status` | `[-m] [-r] [-R owner/repo] [--no-cache]` |
| `issue dashboard` | new | `[--bucket auto\|day\|week\|month] [--chart bar\|line] [--mode absolute\|proportional] [--all-roles] [--author <email>] [--no-plots]` |
| `issue review` | mechanical parts of `/migration-review` | `<slack-body> --to <alias> [--reviewer <gh>] [--base <ref>] [--pr-title <T>] [--pr-body <B>] [--no-push]` |

There is intentionally **no** `start` subcommand: cold-starting a session inside a
worktree is a Claude Code workflow (the `/issue-start` slash command), not a
mechanical step worth compiling.

## Architecture

A single new crate `crates/issue` producing the `issue` binary. The three replaced
crates (`issue-prep`, `issue-end`, `pr-status`) are deleted. Logic is split into
focused modules rather than one large `main.rs` (the three sources total ~1060 lines
today, plus the new dashboard and review code):

```
crates/issue/src/
├─ main.rs        CLI root: clap derive, global -C/--dir, dispatch
├─ setup.rs       `issue setup`   ← issue-prep (worktree/env/bun/ports, JSON out)
├─ triage.rs      shared core: Row, build_rows, reason_not_finished, render
├─ status.rs      `issue status`  ← issue-end status
├─ end.rs         `issue end`     ← issue-end clean (cleanup)
├─ prs.rs         `issue prs`     ← pr-status (gh fetch, diff cache, tables);
│                                    fetch/render fns are pub(crate) for reuse
├─ review.rs      `issue review`  ← mechanical migration-review
└─ dashboard/
   ├─ mod.rs      orchestrate: at-a-glance + plots
   ├─ data.rs     live fetch: Linear history, gh PR/commit timelines
   ├─ bucket.rs   day/week/month bucketing + state-replay (pure, tested)
   └─ chart.rs    terminal bar (hand-rolled) + line (textplots) rendering
```

### Shared library additions (`devkit-common`, `devkit-ports`)

- `devkit-common::linear` gains a history-fetching query alongside the existing
  `states()`:
  - `assigned_issue_history(key) -> Vec<IssueHistory>` — paginated (`first: 50`,
    cursor) over `issues(filter: { assignee: { isMe: { eq: true } } })`, each node
    carrying `identifier`, `createdAt`, current `state { name type color }`, and
    `history(first: 50) { nodes { createdAt fromState{name type color}
    toState{name type color} } }`.
  - `viewer_created_at(key) -> DateTime` — `query { viewer { createdAt } }`, the
    timeline origin.
  - Returns empty / errors gracefully exactly like `states()` (no key → no data).
- `devkit-common::slack` (new module): `post_message(token, channel, text) -> Result<()>`
  posting `chat.postMessage` via `ureq` with a Bearer token; checks the `ok` field
  in the response and surfaces Slack's `error` string on failure.
- `devkit-ports::config` gains:
  - `[people]` map: `people: HashMap<String, Person>` where
    `Person { slack: String, github: Option<String> }`.
  - `defaults.pr_base: String` (default `"staging"`) — the base branch for
    `issue review` PR creation.

### Module reuse seams

- `triage.rs` owns the worktree-triage core (`Row`, `build_rows`,
  `reason_not_finished`, `render`) so `status`, `end`, **and** the dashboard's
  at-a-glance section share one implementation.
- `prs.rs` exposes its fetch + render functions (`fetch_mine`, `fetch_reviews`,
  `mine_table`, `reviews_table`) as `pub(crate)` so `dashboard` renders the PR
  tables through the same gh path and diff format — no second implementation.

## Subcommand detail

### `issue setup` — verbatim port of `issue-prep`

Same flags, same behavior: create the worktree off `defaults.baseline_ref`, symlink
env files, write per-app `prep_env`, run `bun install` once, reserve ports via the
registry facade, print the JSON summary. `--dry-run` computes would-be ports without
reserving. No behavioral change; this is a relocation into `setup.rs`.

### `issue status` / `issue end` — port of `issue-end`

`issue-end`'s `Status` and `Clean` subcommands become top-level `issue status` and
`issue end`. The shared triage logic (`build_rows`, `reason_not_finished`, `render`,
`gather`, `cleanup`, `select_explicit`) moves to `triage.rs` / `end.rs`. All flags
(`-y`, `--force`, `--pr-only`, `--clean-worktree`) and the finished-gate semantics
(PR merged + Linear done + clean) are unchanged. `issue end` reads as "finish the
issue" — the cleanup verb.

### `issue prs` — verbatim port of `pr-status`

The full pr-status binary (gh fetch of mine/reviews, the per-repo diff cache under
`$XDG_CACHE_HOME/devkit/pr-status/`, the two tables, the action colour key) moves to
`prs.rs` unchanged. The diff-cache format and location are preserved so existing
caches keep working.

### `issue dashboard` — new

Renders top-to-bottom:

1. **At-a-glance (always)** — the `triage.rs` worktree table, then the `prs.rs` "my
   open PRs" and "awaiting my review" tables (reusing those modules). `--no-plots`
   stops after this section.
2. **Timeline plots (live-fetched)** — faithful ports of the two Python dashboards
   (`dashboard_issues.py`, `dashboard_prs.py`), switched to live data sources:

   - **Issues by status over time.** From `linear::assigned_issue_history`. For each
     bucket, compute every issue's workflow state **at the period end** by replaying
     its transitions (`state_at`: start from the state before the first recorded
     transition, apply each transition whose timestamp ≤ end). Stack statuses in
     Linear lifecycle order (`triage → backlog → unstarted → started → completed →
     canceled`, then by name); drop statuses that never appear at any period end;
     colour each from its own Linear state hex. Timeline origin = `viewer.createdAt`.
   - **PRs opened/merged over time.** Live `gh pr list --author @me --state all
     --json number,createdAt,mergedAt,additions,deletions` (replaces the committed
     archive). Tally `createdAt` → opened, `mergedAt` → merged per bucket.
     `--all-roles` additionally includes reviewed PRs via the same
     `review-requested:@me` / `reviewed-by:@me` search `prs.rs` already uses.
   - **Commits over time.** `git log --author=<email> --format=%aI` in the
     configured monorepo (`defaults.worktree_root`/monorepo); author defaults to
     `git config user.email`, overridable with `--author`. Tally by author-date.

   **Bucketing** (`bucket.rs`, pure + tested) matches the Python exactly: day /
   Monday-anchored week / month; `auto` picks the finest bucket whose bar count fits
   the terminal width (`max(8, (width - 12) / 2)` bars). Terminal width comes from
   `$COLUMNS`, else a `nix` `TIOCGWINSZ` ioctl (nix is already a workspace dep),
   else fallback 100. Daily resolution draws faint vertical gridlines before each
   Monday. `--mode proportional` normalizes each period's status mix to 100%.

   **Footer summary line** mirrors the Python: total assigned / open now (state type
   not completed|canceled) / PRs opened·merged / commit count / lines ±.

#### Charting (`chart.rs`)

Both `--chart bar` and `--chart line` are supported, matching the Python.

- **Bars** — hand-rolled. Unicode eighth-blocks (`▁▂▃▄▅▆▇█`) for height, stacked
  segments coloured per status (issues chart) or grouped opened/merged (PR chart),
  via ANSI truecolor from each Linear state's hex. No maintained Rust crate renders
  stacked terminal bars well, so this stays in-house; it's pure and unit-testable
  (value matrix + width → rows of cells).
- **Lines** — rendered with the maintained `textplots` crate (braille canvas), one
  series per status / metric, non-stacked. Line plotting is the fiddly part, so per
  the "use a crate if hand-rolling is hard" decision it gets a dependency rather
  than a bespoke renderer. `textplots` is added to the `issue` crate only, **without
  the optional `tool` feature** (that feature pulls the obsolete `structopt`; the
  library core does not). Maintenance confirmed: v0.8.7 (Feb 2025), ~1M downloads,
  ~46k/month, 57 reverse dependencies, active repository.

### `issue review` — mechanical migration-review

Generic (not kysely-specific). All prose — commit message, PR title/body, the Slack
body — stays AI-authored and is passed in as args/flags; the binary does only the
deterministic git/gh/Slack plumbing.

Flow (`review.rs`):

1. Resolve the current branch (`git rev-parse --abbrev-ref HEAD`); **refuse** if it
   is `staging` or `main`.
2. Resolve `--to <alias>` against `devkit.toml` `[people.<alias>]` → `{ slack,
   github }`; error on unknown alias. `--reviewer` defaults to the alias's `github`
   (error if neither is available and a reviewer is needed).
3. Unless `--no-push`: `git push -u origin <branch>`. **Never force-push** — on a
   non-fast-forward rejection, surface the exact error and stop.
4. Detect the PR (`gh pr view --json number,state,url`):
   - **No PR** → require `--pr-title`; `gh pr create --base <base> --reviewer <gh>
     --title <T> --body <B>` (`--base` defaults to `defaults.pr_base`); capture the
     URL from output.
   - **OPEN** → `gh pr edit --add-reviewer <gh>`; reuse the existing URL.
   - **MERGED / CLOSED** → stop and report; nothing to review.
5. Compose `text = "<slack-body> <pr_url>"`.
6. Deliver (**both** mode):
   - If `$SLACK_TOKEN` is set → `slack::post_message(token, slack_id, text)`.
   - Else → print `{ "slack_id", "text", "pr_url", "github", "branch" }` as JSON to
     stdout for the `/migration-review` slash command to forward via the Slack MCP
     tool.

The decision points (branch guard, PR-state → action, default reviewer resolution,
message composition) are pure functions and unit-tested; the git/gh/Slack calls are
thin wrappers.

## Caller updates (out of scope — documented, not edited)

External callers (slash commands, scripts under `~/.claude` / `~/.local/bin`) are
**not** edited by this work. Instead, the final phase writes a single migration note
to the **base** (non-worktree) repo at `docs/issue-binary-migration.md` —
**uncommitted**, for the user to action manually afterward. It lists, with old → new
command mappings, every caller that needs repointing:

- `~/.claude/commands/issue-setup.md` → `issue setup …`
- `~/.claude/commands/issue-end.md` → `issue status` / `issue end`
- `~/.claude/commands/migration-review.md` → use `issue review` for the mechanical
  push/PR/Slack steps (the command keeps authoring the prose and, in JSON-fallback
  mode, forwards the Slack payload via MCP).
- `~/.claude/scripts/issue-end-scan.sh`, `issue-end-cleanup.sh` → `issue status` /
  `issue end` (note whether the binary makes them redundant).
- `pr-status.sh` (if present in `~/.local/bin`) → `issue prs`.

The note is generated by inspecting the actual files during the final phase, so the
mappings are concrete. No file outside this repository worktree is modified.

## Testing

`cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings`
remain the merge gate.

- **Migrated tests move with their code and stay green:** `best_pr`,
  `reason_not_finished`, `branch_name`, the pr-status logic tests (`checks_of`,
  `mine_action`, `review_text`, `reviewer_state`, `diff_cell`, `issue_of`), and the
  `linear::build_query` test.
- **New pure functions get unit tests with fixed clocks / widths (no network):**
  `bucket_starts`, `state_at` (replay), `bucket_index`/`tally`, `choose_bucket`,
  the bar-cell matrix builder in `chart.rs`, and the review decision logic
  (branch guard, PR-state → action, reviewer/message composition).
- The multiprocess flock race test (`devkit-ports/tests/registry.rs`) is untouched.

## Execution phases

Built in a git worktree off `main`. Phased so value lands incrementally and the gate
stays green throughout:

1. **Consolidation.** Scaffold `crates/issue`; move setup / status / end / prs into
   modules; delete `issue-prep`, `issue-end`, `pr-status`. All existing tests pass.
   (Pure refactor — no behavior change.)
2. **`issue review`.** Add the `[people]` config + `defaults.pr_base`, the
   `devkit-common::slack` module, and `review.rs`.
3. **`issue dashboard` — at-a-glance.** Reuse `triage` + `prs` for the live
   top-section view; `--no-plots` complete here.
4. **`issue dashboard` — timelines.** Add the Linear history query, `bucket.rs`,
   `chart.rs` (bars + lines), and `data.rs` live fetch.
5. **Migration note.** Inspect the external callers and write
   `docs/issue-binary-migration.md` (uncommitted) into the base repo with concrete
   old → new mappings for the user to action manually.

## Invariants preserved

- The registry facade and reserve-before-bind discipline are untouched — `issue
  setup` calls the same `registry::{alloc, snapshot}` path as `issue-prep`.
- `prd` remains a rejected `doppler_config`; `RESERVATION_GRACE_SECS` is unchanged.
- The pr-status diff-cache format and location are preserved.
- Each subcommand path keeps installing `report::install_panic_hook("issue")`.

## Open questions

None outstanding. The four micro-decisions (subcommand name `setup`; bare `issue` →
`status`; bars hand-rolled + lines via `textplots`; commit author from `git config
user.email` with `--author` override) and the two `review` decisions (both-mode
Slack delivery; full push+PR+reviewer+send scope) are settled.
