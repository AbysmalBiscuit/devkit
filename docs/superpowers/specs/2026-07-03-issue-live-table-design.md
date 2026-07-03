# Live table rendering for `issue` — design

Draw the triage table the moment its structure is known, fill cells in as each
data source lands, and keep the final stdout output byte-identical to today.

## Status of this spec

Brainstormed with timing data but **scope decisions are assumed, not
confirmed** — the user was away for the clarifying questions. Assumptions,
each marked inline where it bites:

- **A1 (scope):** `issue` (status) and `issue info` get live rendering;
  `issue prs` gets stale-while-revalidate as a phase-2 follow-up.
- **A2 (pending look):** v1 pending cells are a dim `…`; animated in-cell
  spinners are a later polish pass on the same widget.
- **A3 (output contract):** the live table animates on stderr and is cleared;
  the final table prints to stdout exactly as today.

## Problem

Timing runs in a real monorepo show the wait and the renderable data are
inverted:

| command | wall | structure known at | dominated by |
|---|---|---|---|
| `issue` | 3.2s | ~10ms (`git worktree list`, 3ms) | `gh pr` 3.19s, 46× `git status` (25s serial, 9× overlapped) |
| `issue info` | 2.7s | ~20ms | `gh pr` 2.41s + Linear 299ms, run **serially** |
| `issue prs` | 5.9s | n/a — one GraphQL supplies everything | `github graphql` 5.40s |

For `issue`, the row identities (ISSUE, BRANCH) come from discovery in
milliseconds; three *independent* sources — the `git status` fan-out (TREE),
one `gh pr list` (PR), one Linear GraphQL call (LINEAR) — already run on
separate threads in `gather_with_bars` and merely `join` before anything
renders. VERDICT is a pure function of the three. The user stares at spinners
for 3.2s to see a table that was structurally known at 10ms.

## Goal UX

Mid-flight (stderr, TTY only):

```
ISSUE WORKTREES
 ISSUE     BRANCH                        TREE   PR          LINEAR     VERDICT
 DBI-1058  lev/dbi-1058-labos-scheduler  clean  OPEN #3512  …          …
 DBI-1102  lev/dbi-1102-fix-foo          …      MERGED #9   …          …
⠋ Checking 12 worktrees [=====>      ] 5/12
⠋ Fetching Linear states…
✓ PRs fetched
```

Cells pop in per source: TREE cells fill one by one as the dirty fan-out
streams, PR fills as one column when `gh pr list` lands, LINEAR likewise, and
each row's VERDICT appears when its three inputs are complete. When all
sources are done the stderr block clears and the final table prints to stdout
— byte-identical to the current output (A3).

Off-TTY (pipes, MCP, tests): no live rendering at all; behavior and output
are unchanged.

## Design

### 1. Cell formatter extraction (pure refactor, no behavior change)

`triage::render` currently computes each cell inline (`issue_disp`,
`pr_disp`, `linear_disp`, tree, verdict). Extract them as pure functions in
`src/bin/issue/triage.rs`:

```rust
pub(crate) fn issue_cell(row: &IssueWorktree, workspace: Option<&str>) -> String;
pub(crate) fn pr_cell(row: &IssueWorktree) -> String;
pub(crate) fn linear_cell(row: &IssueWorktree, has_key: bool) -> String;
pub(crate) fn tree_cell(dirty: bool) -> String;
pub(crate) fn verdict_cell(row: &IssueWorktree, offline: bool) -> String;
```

`render` calls them; the live path calls the same functions. Sharing is what
guarantees live and final output agree. These also become unit-testable,
which they are not today.

### 2. `LiveTable` widget (`devkit-common`, new module `livetable`)

A live-updating table block over `indicatif::MultiProgress`, following the
`progress::Steps` conventions: stderr, hidden when stderr is not a terminal.

```rust
pub enum Cell { Ready(String), Pending }   // Pending renders dim "…" (A2)

pub struct LiveTable { /* headers, title, rows, MultiProgress, line bars */ }

impl LiveTable {
    pub fn new(title: &str, headers: &[&str], nrows: usize) -> LiveTable;
    pub fn set(&mut self, row: usize, col: usize, cell: Cell);
    pub fn redraw(&mut self);                    // re-render + push to bars
    pub fn spinner(&self, msg: &str) -> ProgressBar;  // status line below the table
    pub fn bar(&self, msg: &str, len: u64) -> ProgressBar;
    pub fn finish(self);                         // clear the whole stderr block
}
```

Mechanics:

- `redraw` renders the table through `ui::table` (same comfy-table config as
  the final render) into a string, splits it into physical lines, and maps
  each line onto a `{wide_msg}`-template `ProgressBar` in the
  `MultiProgress`. Bars are added/removed when the physical line count
  changes — comfy-table's `Dynamic` arrangement can wrap a logical row onto
  several lines in a narrow terminal, so the mapping is *lines*, not rows.
- Status spinners/bars join the same `MultiProgress` after the table lines,
  giving the "indicatif stuff under the table" layout for free.
- Redraws happen only when data arrives (≤ ~4 relayouts per run). Column
  widths may shift between redraws as `…` becomes real content; accepted —
  the table converges within a few seconds. A ticker-driven in-cell spinner
  animation (A2 follow-up) plugs into the same `redraw`.
- Off-TTY the `MultiProgress` is hidden (same check as `Steps::target`);
  every method is a cheap no-op on the drawing side, so call sites need no
  branching.

### 3. Streaming gather for `issue` (status)

`gather_with_bars` (in `src/bin/issue/status.rs`) becomes event-driven.
Source threads stay as they are; instead of `join`-then-`assemble`, each
sends over one `mpsc::channel`:

```rust
enum Update {
    Dirty(usize, bool),                       // streamed per worktree
    Prs(Result<st::Prs>),                     // one shot
    Linear(HashMap<String, LinearState>, Option<String>), // states + workspace
}
```

- **Dirty streaming:** add `st::dirty_stream(paths, f: impl Fn(usize, bool))`
  alongside `dirty_many` (which stays for `gather`/`gather_local`/MCP). Same
  bounded thread pool, but each completed `git status` reports immediately —
  TREE cells pop in one by one, which with 46 checks is where the table feels
  most alive.
- The main thread `recv`s until all sources have reported, applies each
  update to its `IssueWorktree` rows, recomputes VERDICT for rows whose three
  inputs are all present, and calls `redraw`.
- On completion: `finish()` the live table, run the existing
  `assemble`-equivalent finalization, and print via `triage::render` to
  stdout, plus the existing "N finished" / no-key footers. stdout is
  byte-identical to today (A3).
- The library `st::gather` (MCP, dashboard) and `--json` paths are untouched.

**Error handling:** a `gh` failure currently fails the whole command; keep
that. On `Update::Prs(Err(e))`, `finish()` (clear the block) *before*
propagating, so the anyhow report isn't printed under a half-drawn live
region. Linear errors stay soft (empty map → `unknown` cells), as today.

### 4. `issue info`

Two changes:

- **Parallelize** the currently-serial PR → Linear → workspace chain
  (`info.rs`) with the same thread-scope pattern `status.rs` uses — worth
  ~1s of wall time on its own.
- Render the single-row table through `LiveTable`: skeleton immediately
  (ISSUE/BRANCH/TREE known locally), PR/LINEAR/VERDICT pending, filled as
  the two fetches land. `--cache-only` and `--json` skip the live path.

### 5. Phase 2 (separate spec/plan): `issue prs` stale-while-revalidate

One GraphQL query supplies the whole table, so nothing can fill
progressively. Instead: render the previous run's snapshot (the diff cache
at `~/.cache/devkit/pr-status/<repo>.json` already stores it) immediately,
dimmed under an unmissable `as of last run — refreshing ⠋` banner, then
overwrite with fresh data; the existing `old → new` diff cells then show
what changed while you watched. Deliberately deferred: showing stale PR
state has a real act-on-stale-data risk and deserves its own design pass.
The cache would need to grow from diff-keys to full row snapshots.

## Testing

- Extracted cell formatters: direct unit tests (off-TTY ⇒ colour helpers
  pass through, so assertions are plain strings).
- `LiveTable`: rendering to string is a pure function of cells — snapshot
  tests without a TTY; plus the `Steps`-style invariant test that all bars
  are hidden off-TTY.
- Update application (`Update` → row mutation → verdict recompute): pure
  functions, tested with synthetic updates in arbitrary arrival orders
  (PR-first, Linear-first, dirty interleaved).
- `dirty_stream`: results match `dirty_many` on the same paths; callbacks
  arrive exactly once per path. Poll, don't sleep (Windows CI rule).
- Existing rendered-table tests must pass unchanged — that is the A3
  contract check.

## Non-goals

- No full TUI (ratatui) and no persistent refresh loop — one-shot commands
  that render faster, not a dashboard mode (`issue dashboard` already
  exists).
- No change to `--json`, MCP handlers, or any off-TTY output.
- No new dependencies: indicatif + comfy-table already do everything needed.

## Phasing

1. Extract cell formatters (pure refactor, tests).
2. `LiveTable` widget in `devkit-common` with snapshot tests.
3. Wire `issue` status: `Update` channel + `dirty_stream`.
4. Parallelize + wire `issue info`.
5. Polish: in-cell spinner animation (ticker in `LiveTable`).
6. Phase 2: `issue prs` stale-while-revalidate (own design).

## Unresolved questions

1. Confirm A1–A3 (scope, pending-cell look, stdout contract).
2. `issue prs` SWR: is showing last-run PR state for ~5s acceptable given
   merge decisions may be made from it?
3. Is `issue info` worth the live table at all (single row), or only the
   parallelization?
4. Should the in-cell spinner polish (phase 5) happen at all, or is dim `…`
   the end state?
