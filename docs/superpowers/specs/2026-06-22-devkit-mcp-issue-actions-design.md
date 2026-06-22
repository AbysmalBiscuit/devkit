# devkit MCP server — `issue` read actions (phase 3) — design

## Goal

Extend the devkit MCP server's action registry with the two **read-only** `issue`
operations — `issue.status` and `issue.prs` — so a coding agent can inspect issue
worktrees and triage GitHub PRs over MCP. The two-tool meta shape (`devkit_describe` +
`devkit_call`) does not change. Unlike `devrun`, whose logic already lived in library
facades, `issue`'s logic lives in the binary with data-gathering and rendering
interleaved, so phase 3's substance is a **library extraction** that makes these actions
possible without shelling out.

## Scope

Phase 3 adds two read-only `issue` actions:

- `issue.status` — list issue worktrees (optionally filtered by ID), with each
  worktree's PR state, Linear state, and a "finished / not finished + reason" verdict.
- `issue.prs` — triage your GitHub PRs: the PRs you authored and the PRs awaiting your
  review, each with computed semantic fields (review state, CI state, next action).

Both are strictly read-only: `git worktree list`, `gh pr list`, `gh api user`, and
Linear GraphQL queries. No mutations, no file writes, no stdin.

### Non-goals (phase 3)

- **`issue.review` and `issue.end`.** These mutate (push branches, open/edit PRs, post
  to Slack; remove worktrees and delete branches). They are deferred to a later,
  confirm-gated phase. This phase ships the safe read-only pair first, mirroring how
  `devrun` started conservative.
- **`issue setup` and `issue dashboard`.** `setup` is long-running and `dashboard` is
  slow and rendering-heavy — neither is a request/response fit. Out of scope entirely.
- **The `issue.prs` diff cache.** The CLI keeps a per-repo snapshot to render
  "old → new, changed since last run" cells; that is a human-UX feature and stays
  CLI-only (see *The PR diff-cache decision* below).
- **Streaming / progress.** The `indicatif` spinner (`spin.rs`) is CLI-only progress and
  does not move into the library.

## Background — what phase 3 builds on

The data-gathering and rendering are already *loosely* coupled in the binary; the
blocker is that gathering returns un-serializable types and the useful verdict is
computed inside rendering.

- **`src/bin/issue/status.rs`** (`run(start, ids)`) is a thin wrapper: it calls
  `triage::gather(start, ids)` then `triage::render(...)` and prints a finished count.
- **`src/bin/issue/triage.rs`** is the shared gatherer. `build_rows` discovers worktrees
  (`worktree::discover` → `git worktree list`), lists PRs (`gh pr list --state all`),
  and matches the best PR per branch into a `Row { worktree, branch, issue_id, dirty,
  pr_number, pr_state, pr_url }`. `gather` then filters by `ids` and queries Linear
  (`linear::states`, `linear::workspace_url_key`). `render` formats with colors/links
  **and computes `reason_not_finished` per row** — the verdict lives in the renderer
  today.
- **`src/bin/issue/prs.rs`** (`run(mine, reviews, repo, no_cache)`) resolves the default
  (`want_mine = mine || !reviews`, `want_reviews = reviews || !mine` → neither flag ⇒
  both), fetches concurrently (`fetch_mine`, `fetch_reviews`, `gh api user`), then
  renders `mine_table`/`reviews_table`. Its pure mappers already exist and are unit-
  tested: `issue_of`, `checks_of`, `review_text`, `has_replied`, `mine_action`,
  `reviewer_state`, `paint_action`, `diff_cell`. A diff cache (`Snap`,
  `~/.cache/devkit/pr-status/<repo>.json`) is loaded and saved around rendering.
- **Reusable library code already in `devkit-common`:** `worktree::discover`,
  `cmd::{git, gh_json}`, `linear::{states, workspace_url_key}`, `ui` (rendering — stays
  in the binary). **In `devkit-ports`:** `config::Person`, `load::load`.

Two facts drive the design:

1. **The verdict belongs in the data layer.** `reason_not_finished` is decision logic,
   not formatting. Moving it from `triage::render` into the gatherer makes it the single
   source of truth for both the CLI table and the MCP response.
2. **`prs` is already mostly pure.** Its mappers are extracted and tested; only the gh
   fetches and the rendering wrap them. The facade keeps the mappers and the fetches and
   drops the rendering and the cache.

## Architecture

### Facade home — new crate `devkit-issue`

The extraction lands in a **new library crate `devkit-issue`**, not an existing one.
The placement rule the workspace follows is *domain ownership*: `devrun`'s facade went
into `devkit-ports` because it **is** port/registry/supervision logic. Issue triage is a
distinct domain (worktrees + `gh` PRs + Linear) that needs **both** `devkit-common`
*and* `devkit-ports::{load, config}`. `devkit-common` is the base layer and cannot
depend on `devkit-ports`, so no existing crate can host it without a layering violation.
A dedicated crate keeps the dependency graph a clean DAG and matches the existing
"one crate per coherent concern" shape (common / ports / locks):

```
devkit-common  ←  devkit-ports  ←  devkit-issue
devkit-mcp     ←  { devkit-common, devkit-ports, devkit-locks, devkit-issue }
```

### Extraction, no shelling

The MCP handler must not shell out to the `issue` binary (matching the v1/phase-2 "thin
adapter over facades" stance, for structured output and to avoid a binary-path
dependency). Two facade modules are extracted:

**`crates/devkit-issue/src/status.rs`** — the gatherer, returning serializable data with
the verdict computed in the data layer:

```rust
#[derive(Serialize)]
pub struct IssueWorktree {
    pub worktree: String,
    pub branch: String,
    pub issue_id: String,
    pub dirty: bool,
    pub pr_number: Option<u64>,
    pub pr_state: String,                  // MERGED | OPEN | CLOSED | NO_PR
    pub pr_url: Option<String>,
    pub linear_state: Option<String>,
    pub finished: bool,
    pub reason_not_finished: Option<String>,   // moved out of triage::render
}

pub struct StatusReport {
    pub worktrees: Vec<IssueWorktree>,
    pub finished_count: usize,
    pub has_linear_key: bool,
}

pub fn gather(start: &str, ids: &[String]) -> Result<StatusReport>;
```

**`crates/devkit-issue/src/prs.rs`** — the gh fetches plus the already-pure mappers,
returning the computed semantic fields:

```rust
#[derive(Serialize)]
pub struct MinePrView {
    pub number: u64, pub url: String, pub issue_id: String,
    pub review_state: String,   // awaiting | approved | changes | …
    pub check_state: String,    // ok | fail | run
    pub action: String,         // MERGE | address changes | …
}

#[derive(Serialize)]
pub struct ReviewPrView {
    pub number: u64, pub url: String, pub author: String,
    pub my_vote: String, pub action: String,
}

pub struct PrsReport { pub mine: Vec<MinePrView>, pub reviews: Vec<ReviewPrView> }

pub fn gather(root: &str, mine: bool, reviews: bool, repo: Option<&str>) -> Result<PrsReport>;
```

The pure mappers (`mine_action`, `reviewer_state`, `checks_of`, `review_text`,
`has_replied`, `issue_of`) and their tests move into the crate. `paint_action` and
`diff_cell` are rendering and stay in the binary.

### MCP handler

A new module `crates/devkit-mcp/src/issue.rs` exposes `actions() -> Vec<Action>`, wired
into the registry with one `extend` line in `actions::actions()`. This mirrors how
`ports`, `locks`, and `devrun` register today; the tool shape is untouched. Handlers
follow the established pattern: a `Deserialize` arg struct, a `fn() -> Value` schema,
deserialize → call facade → `serde_json::to_value`.

### Binary refactor (single source of truth)

Per the `devrun` precedent, the binary is refactored to consume the facade so logic
lives in exactly one place:

- `src/bin/issue/status.rs` and `triage.rs`: rendering (`render`) consumes the crate's
  `IssueWorktree` structs; `build_rows`/`gather` and the `reason_not_finished` verdict
  move into `devkit-issue`. The binary keeps `render` and the colors/links/`ui` calls.
- `src/bin/issue/prs.rs`: calls `prs::gather`, keeping only the table rendering and the
  diff cache (load/save).
- `src/bin/issue/end.rs` is **out of scope for behavior**, but its `triage::gather` data
  call is repointed to `devkit_issue::status::gather` (mechanical) so a duplicate
  gatherer is not left behind. Its cleanup/mutation/prompt code is untouched.

## Action catalog (phase 3)

| Action | Args | Facade call |
|---|---|---|
| `issue.status` | `root?` (default `"."`), `ids?: string[]` | `status::gather(root, ids)` → serialize `StatusReport` |
| `issue.prs` | `root?` (default `"."`), `mine?: bool`, `reviews?: bool`, `repo?: string` | `prs::gather(root, mine, reviews, repo)` → serialize `PrsReport` |

**Argument detail:**

- `issue.status`: `root` is the directory whose worktrees are enumerated (default `"."`,
  matching the CLI's `start`). `ids` filters to matching issue IDs (case-insensitive,
  same as the CLI).
- `issue.prs`: with neither `mine` nor `reviews` set, **both** are returned (matching
  the CLI's `mine || !reviews` default). `repo` overrides repo detection. The diff cache
  is not consulted (the facade is stateless). `root` anchors `gh` repo detection: the
  MCP server's process CWD is wherever the server was launched, **not** the agent's
  worktree, so the CLI's implicit-CWD detection cannot be relied on. The facade runs
  `gh` with `root` as the working directory (or passes `--repo` when `repo` is given);
  the CLI passes its own `-C/--dir`-derived start, so both share one code path.

## The PR diff-cache decision

The CLI keeps a `~/.cache/devkit/pr-status/<repo>.json` snapshot to render
"old → new (changed since last run)" cells. This is a **human-UX feature that must not
cross into MCP**: an agent has no "last view," and writing the cache from an MCP call
would silently corrupt the human's CLI deltas on their next run. The facade `prs::gather`
is therefore **pure and stateless** — no cache read, no cache write. Cache load/save
stays entirely in the binary's render path.

## Identity and targeting

- **`root`** (`issue.status`) is an explicit argument, defaulting to `"."`. No implicit
  CWD magic beyond that default; worktree discovery runs relative to `root`.
- `issue.prs` anchors `gh` detection on `root` (default `"."`); `repo`, when given,
  overrides detection entirely. Because the MCP server's CWD is not the agent's worktree,
  the facade must not depend on implicit process CWD.

## Side-effect posture

Both actions are read-only. They run `git worktree list`, `gh pr list`, `gh api user`,
and Linear GraphQL queries — no mutations, no file writes, no stdin. No confirm-gating is
needed; that mechanism is reserved for the deferred `review`/`end` phase.

## Error and conflict semantics

Same as v1/phase 2:

- **Protocol/validation failures** (malformed JSON-RPC, unknown action, schema mismatch)
  → a JSON-RPC **error response**.
- **Action ran but failed** (a facade `anyhow` error — e.g. `gh` not authenticated, a
  Linear query failure, an unknown directory) → a `tools/call` result with
  `isError: true` and the full error chain.

Structured results (the worktree rows, PR views) are returned as JSON, not rendered
tables — the agent receives machine-readable data with the computed verdicts/actions
already filled in.

## Testing

TDD throughout; `cargo test --workspace` is the gate.

- **Crate unit tests:** the pure `prs` mappers move with their existing tests. Add a
  `reason_not_finished` verdict test now that it lives in the data layer (table-driven
  over PR/Linear/dirty combinations). Network-dependent `gather` paths are not
  unit-tested in CI (no live `gh`/Linear), matching the `devrun::launch` precedent.
- **MCP integration tests** (`tests/mcp.rs`, mirroring the `devrun` ones):
  `issue.status` and `issue.prs` appear in `devkit_describe`; each returns a schema;
  argument validation and error paths are covered. Happy paths needing live `gh`/Linear
  stay out of CI.
- **Gate:** `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D
  warnings`, `cargo fmt --all --check`. CI tests poll for state rather than sleeping a
  fixed interval (the Windows-runner rule).

## Sequencing

1. Create the `devkit-issue` crate; extract `status::gather` (with the verdict) and
   `prs::gather` (with the pure mappers); move their unit tests. Refactor the `issue`
   binary (`status.rs`/`triage.rs`/`prs.rs`, and repoint `end.rs`'s gather call) to
   consume the crate. This is the cross-cutting change and lands first so the binary and
   the future MCP handler share one code path.
2. Add `crates/devkit-mcp/src/issue.rs` with the two handlers + schemas; register via
   `actions::actions()`. Add MCP integration tests.
3. Docs: README, `AGENTS.md` layout (new crate + `devkit-mcp` row), and flip the
   `issue` actions bullet in `docs/next-steps.md` from "deferred" to "phase 3 shipped
   (read-only)", noting `review`/`end` remain deferred.

## Resolved decisions

1. **Surface** — two read-only actions: `issue.status`, `issue.prs`. `review`/`end`
   deferred; `setup`/`dashboard` out of scope.
2. **Facade home** — a new crate `devkit-issue` (domain ownership; clean layering).
3. **Verdict moves to the data layer** — `reason_not_finished` is computed in
   `status::gather`, the single source for CLI and MCP.
4. **`issue.prs` is stateless** — the diff cache stays CLI-only; the facade neither reads
   nor writes it.
5. **No shelling** — the MCP handler calls the `devkit-issue` facade directly.
6. **Binary refactor** — `issue` consumes the facade (single source of truth); `end.rs`'s
   gather call is repointed but its mutation code is untouched.
7. **`issue.prs` default** — neither flag ⇒ both `mine` and `reviews` (CLI parity).

## Open questions

None blocking. To confirm during the plan phase:

- Whether `triage::render` should take the crate's `IssueWorktree` directly, or a thin
  binary-local view, so CLI colors/links and the MCP rows stay in sync without leaking
  `ui` concerns into the crate.
- Exact module split inside `devkit-issue` (a shared `gh`/PR-matching helper between
  `status` and `prs`, vs. keeping the `prs` fetches self-contained).
- Whether `issue.status` should expose the Linear workspace URL key (used by the CLI to
  build issue hyperlinks) in its JSON, or omit it as a rendering-only concern.
