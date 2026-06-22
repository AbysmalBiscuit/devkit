# Step progress bars + parallel fetches for `issue` status / prs / dashboard

## Problem

The `issue` CLI shows a single cyan spinner while it fetches, then prints a
table. Two costs:

- **No visible structure.** One spinner hides that several independent fetches
  are happening; the user can't tell what is slow.
- **Serial fetches.** Independent network round-trips run back-to-back instead
  of overlapping, so wall-clock time is the *sum* of the fetches rather than the
  *slowest* one:
  - `status::gather` runs `gh pr list` (slow) and then `linear::states` (slow)
    sequentially, even though Linear only needs the locally-discovered worktree
    ids — not the GitHub result.
  - `prs::run` resolves the Linear workspace, then fetches PRs, sequentially.
  - `prs::gather` itself makes **four** GitHub round-trips (`gh api user`, one
    `gh pr list` for mine, two `gh pr list --search` for reviews).

## Goals

1. Replace the single spinner with **numbered step bars** (no emoji), keeping
   the current cyan accent. Independent fetches run **concurrently** and show as
   multiple bars at once.
2. Use a **determinate fill bar** wherever we iterate over a known count; use an
   honest **spinner** where the work is a single opaque/batched network call.
   Never fake a `0→100%` over one network call.
3. **Collapse GitHub fetches to a single request** where multiple are made
   today, so "fetch the status of all my PRs" is one round-trip.
4. When all fetches finish, **clear the bars** and print only the table /
   dashboard — no lingering progress lines.

## Non-goals

- No determinate bar for single batched network calls (a per-item fill would
  require N round-trips and be *slower* — the opposite of the goal).
- No change to how tables/charts render, to the finished-verdict logic, or to
  the MCP/JSON facades' output (they stay silent — see below).

## Where a real fill bar is honest

| Operation | Known count up front? | Bar |
|---|---|---|
| `status` — worktree dirty-check | yes — `worktree::discover` returns M, then a `git status` per worktree | **determinate** `0→M` |
| `status` — `gh pr list` | no — count only known after the call; classification is in-memory | spinner |
| `status` / `dashboard` — Linear states | id count known, but **one batched GraphQL request** returns all at once | spinner |
| `prs` — GitHub PR fetch | no — single list/search result | spinner |
| `dashboard` — Linear issue history | **paginated**, total pages unknown until the last page | spinner with a **rising count** |
| `dashboard` — pr_timeline / commit_dates | single `gh` / `git` call each | spinner |

The only genuine fill bar is the worktree dirty-check in `status`.

## Architecture

`indicatif` stays out of `devkit-issue` and `devkit-common` — the library
crates remain the pure, serializable, no-rendering facades described in
`AGENTS.md`. All bar orchestration lives in the `src/bin/issue` CLI. The library
changes are limited to (a) running independent fetches concurrently inside the
existing silent entry points, and (b) exposing the granular pieces the CLI needs
to drive its own bars.

### 1. Bar helper — `src/bin/issue/spin.rs`

Keep the existing `spinner(msg)`. Add a small `MultiProgress`-based group:

- A `Steps` handle wrapping a `MultiProgress`. Hidden when stderr is not a TTY
  (same rule as `spinner` today), so pipes, redirects, MCP, and tests print
  nothing.
- `Steps::spinner(prefix, msg)` — adds an animated spinner bar styled
  `[{prefix}] {spinner:.cyan} {wide_msg}`. The `[{prefix}]` is rendered only
  when the group has more than one step; a lone step shows just the spinner +
  message (no `[1/1]`).
- `Steps::bar(prefix, msg, len)` — adds a **determinate** bar
  (`[{prefix}] {spinner:.cyan} {wide_msg} {pos}/{len}`) for known-count loops.
- `Steps::clear()` — clears every bar (called once all work is done, before
  printing results).

Each added bar is a normal `indicatif::ProgressBar`, so callers tick / set
messages / finish through indicatif directly; the helper only owns the
`MultiProgress`, the shared styles, the TTY gate, and the numbering rule.

### 2. `status` — split the library, drive 3 concurrent bars

`devkit-issue::status` is refactored into composable pieces (all real logic
stays in the lib):

- `discover(start, ids) -> Discovered` — local only: `worktree::discover`, the
  `ids` filter, and the main-repo path. Holds row skeletons (branch, issue id,
  `dirty = false` placeholder), the issue ids, and the main path. Fast; reveals
  the worktree count `M`.
- `dirty_of(path) -> bool` — the one-worktree dirty check
  (`!git status --porcelain` empty). Lets the CLI loop + tick a determinate bar;
  the silent path uses the same helper.
- `fetch_prs(&Discovered) -> Prs` — the single `gh pr list --state all --limit
  500 --json number,state,url,headRefName` round-trip (this already fetches all
  worktree PRs in **one** request). `Prs` is an opaque newtype over the private
  `Pr`. Short-circuits to empty when there are no issue worktrees.
- `assemble(Discovered, Prs, linear_states) -> StatusReport` — attaches best
  PR + Linear state + finished verdict (the existing per-row logic).
- `gather(start, ids)` keeps its **exact signature** and stays silent, now wired
  as `discover → fill dirty → (fetch_prs ‖ linear::states) → assemble` via
  `std::thread::scope`. MCP, the dashboard's direct call, and the existing tests
  keep working — and inherit the parallelism.

CLI `status.rs` orchestrates the bars over the same pieces:

```
[1/4] Discovering worktrees…                 spinner (fast, local) → M, ids
then thread::scope, 3 bars concurrently:
  [2/4] Checking M worktrees   ███░ k/M      DETERMINATE fill (dirty_of loop, local)
  [3/4] Fetching PRs from GitHub…             spinner (fetch_prs)
  [4/4] Fetching Linear states…               spinner (linear::states, also workspace_url_key)
join → Steps::clear() → assemble → render the table
```

The dirty-check loop is local and independent of the network, so it fills while
the two network spinners run.

### 3. `prs` — one GitHub request, 2 concurrent bars

Collapse the four GitHub round-trips in `prs::gather` into a **single**
`gh api graphql` request that aliases the three searches and the viewer login:

```graphql
query {
  viewer { login }
  mine:            search(query: "is:pr is:open author:@me",            type: ISSUE, first: 100) { nodes { ...prFields } }
  reviewRequested: search(query: "is:pr is:open review-requested:@me",  type: ISSUE, first: 100) { nodes { ...prFields } }
  reviewedBy:      search(query: "is:pr is:open reviewed-by:@me",       type: ISSUE, first: 100) { nodes { ...prFields } }
}
```

`prFields` selects everything the classifiers need:
`number, url, headRefName, isDraft, reviewDecision, mergeable`,
`statusCheckRollup { state }` (via `... on PullRequest`),
`author { login }`, the latest reviews (author login, state, submittedAt), and
review requests (requested-reviewer login). The exact GraphQL field paths
(notably `statusCheckRollup` and the "latest review per author" selection) are
verified against the GitHub GraphQL schema during implementation; the
verification is a task in the plan.

Consequences:

- `gather` no longer needs its internal `thread::scope` — it is one request.
- `checks_of` switches from a per-check array to the rollup **state** enum
  (`SUCCESS→ok`, `FAILURE`/`ERROR→fail`, `PENDING`/`EXPECTED→run`, none→`-`).
  Its unit tests are rewritten to the new input shape; the fail/run/ok/`-`
  contract is preserved.
- The `reviews` / `latestReviews` / `reviewRequests` JSON structs are re-shaped
  to the GraphQL response; `mine_action`, `reviewer_state`, `review_text`,
  `has_replied`, and `issue_of` keep their behavior and their tests (tests
  updated only where the input struct shape changes).

CLI `prs.rs` then runs the two independent calls concurrently:

```
thread::scope, 2 bars:
  [1/2] Resolving Linear workspace…           spinner (workspace_url_key)
  [2/2] Fetching PRs from GitHub…             spinner (prs::gather — now 1 request; also resolve_repo for the cache)
join → Steps::clear() → render the two tables
```

### 4. `dashboard`

- **Status panel**: reuse `status.rs`'s bar orchestration (extracted as a
  shared `fn` returning `StatusReport` with bars) instead of the current silent
  `status::gather` call — the panel previously showed no progress at all.
- **PR tables**: call `prs::run` (inherits the 2-bar treatment).
- **Linear issue history**: spinner whose message shows a **rising count**
  ("Loading Linear issue history… 137 issues") as pages stream in. Needs a tiny
  per-page callback added to `linear::assigned_issue_history` (a
  `FnMut(usize)` invoked with the running total after each page); the silent
  callers pass a no-op.
- **PR + commit history**: `pr_timeline` and `commit_dates` are independent →
  run them in a `thread::scope` as 2 concurrent spinners.

Each dashboard phase clears its own bars before the chart for that phase prints.

## Behavior when not a TTY

Every bar (spinner or determinate) is hidden when stderr is not a terminal, so
MCP (`devkit-mcp`'s `issue.status` / `issue.prs` actions, which call the silent
`gather` functions), piped/redirected runs, and `cargo test` produce no progress
output. Result tables print to stdout exactly as today.

## Testing (TDD)

- New lib tests: `discover` + `dirty_of` + `assemble` compose to the same
  `StatusReport` the monolithic path produced (table-driven on the pure parts).
- `checks_of` tests rewritten to the rollup-state input; fail/run/ok/`-`
  contract asserted.
- Existing `best_pr`, `reason_not_finished`, `mine_action`, `reviewer_state`,
  `review_text`, `issue_of`, and Linear query tests stay green (struct-shape
  updates only where the GraphQL response differs).
- Bars are not asserted — hidden when non-TTY, like today's spinner.
  `cargo test --workspace` remains the merge gate; `cargo clippy --workspace
  --all-targets -- -D warnings` stays clean.

## Open questions

None outstanding — issue-history rising count, kept step labels, and
clear-bars-then-print (no per-step "done" line) are all settled above.
