# Live table rendering for `issue` — design

Draw the triage table the moment its structure is known, fill cells in as each
data source lands, and keep the final stdout output byte-identical to today.
Also make step-driven commands print a persistent numbered step log instead of
overwriting one spinner line.

## Decisions (confirmed with the user)

- **D1 (scope):** `issue` (status), `issue info`, *and* `issue prs` — all
  now. `prs` uses stale-while-revalidate with stale rows rendered dim.
- **D2 (pending look):** animated in-cell spinners from v1 (braille frames,
  ticker-driven), not static placeholders.
- **D3 (output contract):** the live table animates on stderr and is cleared;
  the final table prints to stdout exactly as today. OSC8 links render fine
  in the live stderr table too — terminals hyperlink by bytes, not stream —
  but stdout is what redirects/pipes/MCP capture, so it stays the contract.
- **D4 (step log):** step-driven commands (`issue checkout-pr`, `setup`,
  `review`, `end`) keep completed steps on screen as numbered `✓` lines
  instead of clearing each spinner.

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
 DBI-1058  lev/dbi-1058-labos-scheduler  clean  OPEN #3512  ⠹          ⠹
 DBI-1102  lev/dbi-1102-fix-foo          ⠹      MERGED #9   ⠹          ⠹
⠹ Checking 12 worktrees [=====>      ] 5/12
⠹ Fetching Linear states…
✓ PRs fetched
```

Pending cells animate a braille spinner frame (D2); a ~100ms ticker rotates
the frame and redraws. Cells pop in per source: TREE cells fill one by one as
the dirty fan-out streams, PR fills as one column when `gh pr list` lands,
LINEAR likewise, and each row's VERDICT appears when its three inputs are
complete. When all sources are done the stderr block clears and the final
table prints to stdout — byte-identical to the current output (D3).

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
pub enum Cell {
    Ready(String),
    Stale(String),  // prs SWR: last-run value, rendered dim (D1)
    Pending,        // renders the current braille spinner frame (D2)
}

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
- A ~100ms ticker thread advances the shared spinner frame and calls
  `redraw` while any cell is `Pending` (D2); it stops when the last pending
  cell resolves. Data arrivals also trigger an immediate redraw. Column
  widths may shift when a 1-glyph spinner becomes real content; accepted —
  the table converges within a few seconds, and `Dynamic` arrangement
  re-measures every redraw anyway.
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
  byte-identical to today (D3).
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

### 5. `issue prs` — stale-while-revalidate

One GraphQL query supplies the whole table, so nothing can fill
progressively. Instead: render the previous run's snapshot immediately with
every cell `Cell::Stale` — dim (D1) under an `as of last run — refreshing ⠋`
banner — then overwrite with fresh data when the fetch lands; the existing
`old → new` diff cells then show exactly what changed while you watched.

- The diff cache at `~/.cache/devkit/pr-status/<repo>.json` currently stores
  only the diffed fields (`review`/`check`/`action` keyed by PR number); it
  grows to full row snapshots (`MinePrView`/`ReviewPrView` serialized) so
  the stale table has PR numbers, URLs, authors, and issue ids to render.
  Old-format caches deserialize as absent → first run after upgrade simply
  has no stale table, same as `--no-cache`.
- A PR present in the cache but gone from fresh results disappears at
  overwrite; new PRs appear. No tombstone rendering.
- `--no-cache` skips the stale render as well as the diff.
- Dim is the staleness affordance: dim rows + the banner mean "read-only
  until it settles". The fresh overwrite restores normal colours.

### 6. Persistent step log for step-driven commands

`issue checkout-pr` (also `setup`, `review`, `end`) runs each step through
`Steps::during`, which `finish_and_clear`s the spinner — the user watches a
single line overwrite itself and ends with no record of what happened.
Change: a persist mode on `Steps` where a completed step stays on screen as
a numbered line, and the next step's spinner appears below it:

```
✓ 1. Resolving Linear issue ENG-123 (312ms)
✓ 2. Fetching PR #3512 (1.2s)
⠋ 3. Creating worktree…
```

- `Steps::persistent()` (and `persistent_with_total(n)`) construct the mode;
  `during` then ends with `finish_with_message` (`✓ n. msg (elapsed)`)
  instead of clearing. Finished bars are plain flushed lines in indicatif,
  so stdin prompts and stdout output between steps still work — no bar is
  *active* across a prompt, which is the property the old clearing behavior
  existed to protect.
- On a step that returns an error the bar finishes as `✗ n. msg` before the
  error propagates, so the failed step is identifiable in the log.
- Off-TTY behavior is unchanged: hidden entirely (pipes/MCP/tests see no
  step noise). The step log is a TTY affordance, not output.
- Call sites opt in by constructor choice; `Steps::new` keeps the clearing
  behavior for flows that genuinely want a transient spinner (e.g. the
  concurrent bars in `status` that the live table replaces anyway).

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
- `prs` snapshot cache: round-trip serialization; old-format cache
  deserializes as absent (no stale table) rather than erroring.
- Persistent `Steps`: off-TTY bars stay hidden (existing invariant test
  pattern); numbered labels advance; error path finishes `✗`.
- Existing rendered-table tests must pass unchanged — that is the D3
  contract check.

## Non-goals

- No full TUI (ratatui) and no persistent refresh loop — one-shot commands
  that render faster, not a dashboard mode (`issue dashboard` already
  exists).
- No change to `--json`, MCP handlers, or any off-TTY output.
- No new dependencies: indicatif + comfy-table already do everything needed.

## Phasing

1. Extract cell formatters (pure refactor, tests).
2. `LiveTable` widget in `devkit-common` (cells, ticker animation, status
   lines) with snapshot tests.
3. Wire `issue` status: `Update` channel + `dirty_stream`.
4. Parallelize + wire `issue info`.
5. `issue prs`: snapshot cache upgrade + stale-while-revalidate render.
6. Persistent step-log mode on `Steps`; adopt in `checkout-pr`, `setup`,
   `review`, `end`.

## Unresolved questions

None — D1–D4 confirmed 2026-07-03.
