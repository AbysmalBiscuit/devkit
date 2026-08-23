# A pluggable issue tracker: Linear, GitHub Issues, or none

**Date:** 2026-08-23
**Status:** ready to plan. Three separable deliveries; only the first two are
prerequisites for each other. No adversarial review round yet.

`issue` is a Linear client wearing a generic name. Every command that touches a
tracker calls `devkit_common::linear` directly, and the Linear data model has
leaked into three layers that have no business knowing it: the serialized status
report, the branch-name id scan, and the string vocabulary the dashboard sorts
on. This spec puts a `Tracker` seam under those call sites, moves Linear behind
it unchanged, and adds a GitHub Issues implementation selected by config.

It also carries an unrelated config change that happens to share a reviewer:
`[defaults]` path values gain `${VAR}` expansion and resolve relative paths
against the layer that declared them, so a project's `devkit.toml` can be
committed and shared instead of being per-machine by construction.

## Problem

devkit's engine is project-agnostic by design. The tracker is the one place that
isn't. A project using GitHub Issues can run `issue setup --slug <name>` today
and get a worktree, but it loses the title-derived slug, the summary file, the
tracker column in `issue status`, the state gate in `issue end`, and the entire
issue timeline in `issue dashboard`. The degradation is silent and it is spelled
"no Linear key" in the UI.

The leak is not evenly distributed. Eleven distinct `linear::` functions are
called from eighteen sites, and most of them are thin:

| Function | Sites | What it does |
|---|---|---|
| `workspace_url_key` | 5 | builds an issue URL prefix |
| `states` | 3 | batch state lookup for N ids |
| `validate` | 2 | credential check for `auth` / `doctor` |
| `issue_title` | 1 | slug source (`setup.rs:222`) |
| `issue_details` | 1 | summary-file fields (`setup.rs:242`) |
| `issue_pr` | 1 | issue → its PR (`checkout.rs:155`) |
| `issues_by_number` | 1 | disambiguate a bare number (`checkout.rs:190`) |
| `issues_for_prs` | 1 | PR → its issues, opt-in |
| `assigned_issue_history_with_progress` | 1 | dashboard timeline |
| `viewer_created_at` | 1 | dashboard timeline origin |
| `pr_number_from_url` | 1 | parses a *GitHub* URL; misfiled, not tracker work |

The deeper leak is the vocabulary. Linear's state `type` strings —
`triage`/`backlog`/`unstarted`/`started`/`completed`/`canceled` — are already
devkit's internal contract, stringly typed and unvalidated:

- `devkit-issue/src/status.rs:324` decides the finished verdict on
  `kind != "completed"`.
- `src/bin/issue/triage.rs:64-70` colors the tracker cell by matching three of
  the six, with a catch-all for the rest.
- `src/bin/issue/dashboard/mod.rs:83-85` ranks chart bands by matching three.
- `src/bin/issue/dashboard/mod.rs:142` counts "open now" as neither
  `completed` nor `canceled`.

So the abstraction already exists. It is just undeclared, untyped, and sourced
from exactly one vendor.

## What the target project actually needs

Measured against `K-Nette/BountyPop_GODOT` on 2026-08-23, not assumed:

- 22 issues, 19 `OPEN`, 3 `CLOSED`.
- Every label is topical (`combat`, `network`, `vfx`, `agents`, `ci`). None
  encodes status.
- One milestone in use (`Steam Demo Fest`). Milestones group a release, not a
  workflow.
- No Projects v2 board in play, and the account's `gh` token lacks
  `read:project`, so `projectItems` returns a scope error.
- Titles are conventional-commit shaped: `docs(agents): document the branch
  naming convention`.
- Branch convention is `<dev>/[<context>/]<name>`, lowercase, enforced by both
  the `pre-push` hook and a CI job. Nothing requires an issue number in a branch.

So the GitHub state model is open/closed, and `stateReason` splits closed into
`COMPLETED` / `NOT_PLANNED` / `DUPLICATE`. That maps onto three of devkit's six
kinds without inventing anything.

`devkit_common::github` already provides everything the adapter needs to talk to
the API: `graphql()` (`github.rs:64`) posts directly to `api.github.com/graphql`
with a token from `GH_TOKEN`/`GITHUB_TOKEN` falling back to `gh auth token`
(`github.rs:51`), and it is already wrapped in a timing span. `repo_slug(cwd)`
(`github.rs:163`) derives `owner/repo` from the `origin` remote with no HTTP.
The adapter spawns no subprocess per query and does not depend on the local `gh`
version, which matters: the installed `gh` 2.46.0 has no `stateReason` in its
`--json` field set, while the GraphQL API returns it fine.

## Design

### The seam

`devkit_common::tracker`, with `tracker::{linear, github, none}` submodules.
`linear.rs` moves down one level rather than into a crate of its own. It sits
beside `github` and `slack`, which are the same kind of thing: a client for one
outside service.

A `devkit-tracker` crate was the first instinct, on the reasoning that `Config`
lives in `devkit-ports` and a tracker taking `&Config` would invert the
dependency graph. That reasoning had the wrong culprit — `Config` is the thing
in the wrong crate (next section). Either way the entry point takes primitives,
which keeps it constructible in a test without a whole `Config`:

```rust
pub fn resolve(kind: Option<Kind>, repo: Option<&str>, cwd: &Path) -> Box<dyn Tracker>;
```

### A `devkit-config` crate

`devkit-ports/src/config.rs` is 1618 lines with zero `crate::` imports. It
depends on `anyhow`, `schemars`, `serde`, `toml`, and `std`, and nothing else in
its own crate. It is a leaf module in the wrong place, most likely because
`portm` was the first binary and the config began as an app-and-port catalog
before it grew `[defaults]`, `[templates]`, `[docs]`, `[brief]`, and
`[harness]`.

Three consumers already work around the placement, because none of them can
depend on `devkit-ports`:

| Crate | Workaround |
|---|---|
| `devkit-locks` | declares its own `HarnessProbe` to read `[harness] enforce_writes` (`hook.rs:137`) |
| `devkit-docs` | reimplements the whole layer walk over `devkit.toml` (`manifest.rs:167-180`) |
| `devkit-issue` | cannot read config at all |

The `devkit-docs` one is not merely duplication. Its walk reads only
`devkit.toml` — no `devkit.local.toml`, no `[config] root = true` cutoff, no
`$DEVKIT_CONFIG` — while `config.rs:730` lists `docs` in `STANDALONE_SECTIONS`
and `docs/configuration.md` documents `devkit.local.toml` as the untracked twin
with the same shape and schema. **A `[docs]` section in a `devkit.local.toml` is
valid config that `docm` silently ignores.** See "Discovered defects" below.

So `config` becomes its own crate rather than moving into `devkit-common`:

```
devkit-config   ← anyhow, schemars, serde, toml. No internal deps.
    ↑ devkit-common, devkit-ports, devkit-locks, devkit-issue, devkit-docs
```

Parking 1618 lines in `devkit-common` would put them in the crate everything
already pulls, and would add `schemars` to that path for `devkit-locks` and
`devkit-issue`, neither of which needs it. A leaf crate keeps each crate's role
the way `AGENTS.md`'s table describes them.

One consequence to accept deliberately: `Kind` needs `JsonSchema`, because
`[tracker] kind` appears in the published schema and taplo's completion depends
on the enum constraint. So `Kind` lives in `devkit-config` and
`devkit-common` depends on it, giving back part of the "keep `schemars` off the
common path" benefit. The alternative is a duplicated three-variant enum plus a
conversion — worse code for an unmeasured compile-time saving.

The move itself is mechanical: 30 reference lines across 14 files, plus four
`devkit-ports` modules (`apps`, `task`, `load`, `strays`) changing
`crate::config` to `devkit_config::`. It lands as
the first commit of phase 2, and makes a `tracker::from_config` convenience
natural alongside the primitive `resolve`.

Scope stays narrow. `apps`, `doppler`, and `load` stay in `devkit-ports`; they
are the port and app catalog, which is that crate's job.

The state vocabulary stops being strings:

```rust
pub enum Kind { Triage, Backlog, Unstarted, Started, Completed, Canceled }
pub struct State { pub kind: Kind, pub name: String, pub color: String }
```

`Kind` serializes to the same lowercase strings it replaces, so the dashboard's
cached JSON stays readable across the upgrade and the four match sites above
become exhaustive instead of catch-all. Per `AGENTS.md`, no `_ =>` arms.

The trait, one method per existing function:

```rust
pub trait Tracker {
    fn kind(&self) -> Kind;                                   // which provider
    fn ready(&self) -> bool;                                  // configured + can auth
    fn issue_ref(&self, input: &str) -> IssueRef;             // id | #n | URL → id (+ slug)
    fn title(&self, id: &str) -> Result<Option<String>>;
    fn details(&self, id: &str) -> Result<Option<IssueDetails>>;
    fn states(&self, ids: &[String]) -> HashMap<String, State>;
    fn issue_pr(&self, id: &str) -> Result<Option<PrRef>>;
    fn candidates(&self, n: u64) -> Result<Vec<IssueRef>>;    // bare-number disambiguation
    fn issues_for_prs(&self, urls: &[String]) -> HashMap<String, Vec<String>>;
    fn assigned_history(&self, on_page: &mut dyn FnMut(usize)) -> Result<Vec<AssignedIssue>>;
    fn timeline_origin(&self) -> Result<Option<String>>;
    fn issue_url(&self, id: &str) -> Option<String>;
    fn check(&self) -> Result<String>;                        // doctor: identity line
}
```

`pr_number_from_url` does not join it. It parses a GitHub PR URL and has always
belonged in `devkit_common::github`; it moves there as a drive-by.

The trait, not an enum, because of tests. `status::gather_live`
(`devkit-issue/src/status.rs:341`) spawns threads that make real network calls,
so its tests can only reach `assemble` with hand-built rows. `&dyn Tracker` lets
a fake drive discovery, state attachment, and the finished verdict end to end,
which is the layer where GitHub behavior needs proving. A `command`-backed
provider, if it is ever built, is then a file rather than a redesign.

### The GitHub mapping

| GitHub | `Kind` | `State.name` | colour |
|---|---|---|---|
| `OPEN` | `Started` | `Open` | GitHub's `open` green, rendered yellow by `triage.rs` |
| `CLOSED` + `COMPLETED` | `Completed` | `Done` | purple |
| `CLOSED` + `NOT_PLANNED` | `Canceled` | `Not planned` | grey |
| `CLOSED` + `DUPLICATE` | `Canceled` | `Duplicate` | grey |

`OPEN` maps to `Started`, not `Unstarted`. GitHub gives no signal that separates
a backlog issue from one in progress. Deriving it from "has an assignee" was
considered and rejected: in the target repo every issue is assigned to the sole
developer, so the chart would collapse to one band while implying four. Two
honest bands beat four invented ones.

History for the dashboard comes from
`timelineItems(itemTypes: [CLOSED_EVENT, REOPENED_EVENT])`, which returns
per-event `createdAt` and, on `ClosedEvent`, its own `stateReason`. Verified
against issue 87. `timeline_origin` returns the earliest issue `createdAt`
rather than Linear's account-creation date.

`assigned_history` is scoped to the configured repo, one paginated query. Not
`gh search issues --assignee @me` across GitHub: that drags in every drive-by
assignment in every org, and per-issue timeline history across N repos is N
queries.

`issue_pr` maps to the issue's `timelineItems(itemTypes: [CONNECTED_EVENT,
CROSS_REFERENCED_EVENT])`, filtered to PRs in the same repo. This is the least
certain mapping in the design and should be prototyped against a real linked
pair before the phase-3 plan hardens.

`candidates(n)` returns empty for GitHub, by decision. A bare `87` is ambiguous
between issue #87 and PR #87 in one repository, where Linear's namespaces at
least differ. Rather than route that through the `decide_fuzzy` prompt
(`checkout.rs:190-205`), the GitHub tracker keeps the rule `checkout-pr` already
applies when no Linear key is set (`checkout.rs:182-188`): **a bare number is a
PR.** One rule, no prompt. The method stays on the trait because Linear needs it.

That leaves no shorthand for "the *issue* numbered 87" under GitHub, which is
acceptable because the issue URL is unambiguous. `classify` (`checkout.rs:31`)
currently hardcodes both URL shapes; it delegates id and URL recognition to
`Tracker::issue_ref` and keeps only the two generic rules it already has —
`#N` is a PR, and a `github.com/…/pull/N` URL is a PR. A
`github.com/…/issues/N` URL then resolves as an issue through the tracker.

### The `None` tracker

A real implementation, not a special case: every method returns empty, `ready()`
is false, `reason_not_finished` skips the state gate. That is exactly today's
no-key behavior, which lets the scattered `if has_key` branches in `status.rs`
and `end.rs` be deleted rather than generalized.

### Config

Purely additive. Nothing existing changes meaning.

```toml
[tracker]
kind = "github"          # "linear" | "github" | "none"; detected when absent

[github]
repo = "K-Nette/BountyPop_GODOT"   # optional, defaults to the origin remote
```

`[linear] resolve_pr_links` (`config.rs:28`) stays exactly where it is. Moving
it under `[tracker.linear]` would break every config that sets it and buy
nothing.

Resolution order when `kind` is absent: a resolvable `LINEAR_API_KEY`, then a
GitHub `origin` remote, then `None`.

Detection is a floor, not a convenience. Any developer with a globally exported
`LINEAR_API_KEY` gets `linear` for every project, so a GitHub project on such a
machine must set `kind` explicitly. The value of detection is that every config
that exists today keeps behaving identically without being edited. It will not
save the target project its one line. `devkit doctor` must therefore print which
tracker resolved *and why*, or the ambient behavior is undebuggable.

`[templates] parse_conventional_titles` (default `true`) gates the title parsing
below.

### The status report shape (breaking)

The Linear-shaped fields on `IssueWorktree` and `StatusReport`
(`status.rs:44-45`, `55-56`) are replaced. This changes the MCP `issue` handler's
JSON, which is accepted.

```rust
pub struct IssueWorktree { /* … */ pub state: Option<State> }
pub struct StatusReport  { /* … */ pub tracker: TrackerInfo }
pub struct TrackerInfo { pub kind: Kind, pub ready: bool, pub link_base: Option<String> }
```

`ready` carries what `has_linear_key` meant. `link_base` generalizes
`linear_workspace`, whose only job was building issue URLs. User-facing strings
follow: "no Linear key" becomes "no tracker", and `triage::linear_cell` becomes
`triage::state_cell`.

### Issue-id recovery

`worktree::issue_id_of` (`worktree.rs:37`) scans the branch then the directory
name for the first letters-dash-digits run (`find_id`, `worktree.rs:51`) and
uppercases the result. Both halves are Linear assumptions hiding in a generic
function, and the target repo's branch convention carries no issue number at all.

`IssueRecord` (`src/bin/issue/record.rs`) moves into `devkit_common::worktree`,
beside the function that needs it. It is already written by both entry points
that create a worktree — `checkout.rs:362` and `end.rs:296` — and already holds
`issue`. Lookup becomes:

1. `<worktree>/.devkit/issue.toml`, field `issue`, verbatim.
2. Failing that, the branch scan, then the directory scan, as today.
3. Failing that, `UNKNOWN`.

Normalization moves out of `issue_id_of` and into `Tracker::issue_ref`, so
Linear uppercases `eng-123`, GitHub strips a leading `#`, and `None` passes
through. The branch fallback is kept deliberately: it is what keeps every
pre-existing worktree and every hand-made `git worktree add` working.

### Slug and template context

`from_linear_title` (`slug.rs:22`) becomes `from_issue_title` and strips two
prefixes: the issue id, as today, and a leading conventional-commit prefix. The
parse is a hand-rolled scan — lowercase letters, optional `(scope)`, optional
`!`, then `: ` — not a regex.

`type` and `scope` join the render context for `branch`, `worktree_dir`, and
`issue_summary`, empty strings when the title is not conventional. The target
repo can then set:

```toml
[templates]
branch = "{{ prefix }}{{ scope }}/{{ slug }}"
```

and get `lev/agents/document-the-branch-naming-convention`, which is its
documented `<dev>/<context>/<name>` convention exactly.

This changes Linear branch names too. A Linear issue titled `fix(api): handle
null user` currently slugs to `fix-api-handle-null-user` and will slug to
`handle-null-user`. Existing worktrees are untouched; future branches read
differently. On by default because the slug is strictly better;
`parse_conventional_titles = false` restores the old behavior.

### Config path expansion (independent)

Applied to `worktree_root`, `baseline_path`, `doppler_yaml`, and
`branch_prefix`. In order:

1. Expand `${VAR}`. An unset variable is a hard error naming both the config key
   and the variable. `$$` escapes a literal `$`. Only the braced form is
   recognized, so there is one syntax to document and no ambiguity about where a
   bare `$NAME` ends.
2. Expand a leading `~/`, as `expand_tilde` (`config.rs:769`) does today.
3. For the three path keys, if the value is still relative, resolve it against
   the directory of the config layer that declared it.

Step 3 needs no new plumbing. `Provenance.origin` (`config.rs:502`) is already a
`HashMap<dotted-path, PathBuf>` populated per leaf by `deep_merge`
(`config.rs:519`), so the declaring layer is a lookup away.

It is a behavior change: a relative `worktree_root` currently resolves against
the process working directory, so `issue setup` run from two directories gives
two answers. That is a bug wearing a feature's clothes.

What it buys is the thing that motivated this section. A project's tracked
`devkit.toml` can say:

```toml
[defaults]
worktree_root = "../BountyPop_GODOT-worktrees"
baseline_path = "../BountyPop_GODOT-worktrees/_baseline"
baseline_ref  = "origin/main"
```

and be correct on every machine and for every developer, with only
`branch_prefix` left to a personal layer or `${USER}`.

## Non-goals

- **Write operations.** `devkit-issue` is a read-only triage facade and stays
  one. No assigning, no state transitions, no comments.
- **A `command` provider.** Configurable shell commands emitting JSON is a
  plausible eventual third provider and the wrong first move: it means publishing
  a plugin API of a dozen JSON schemas designed against zero real consumers.
  Once Linear and GitHub have both been implemented against this trait, the
  contract is proven and a command provider is an adapter. Revisit then.
- **GitHub Projects v2 and label-as-status.** Neither is in use in the target
  repo, and Projects needs a token scope the account does not have. The trait
  makes both additive later: they change one `states()` implementation and add
  config, nothing else.
- **Jira, Shortcut, Linear write-back.** Not asked for.

## Delivery

Three phases. Phase 3 is meaningless without phase 2; phase 1 is independent of
both and could land first or in parallel.

**Phase 1 — config path expansion.** `${VAR}`, `~`, and layer-relative
resolution on four `[defaults]` keys. Schema regen, docs. Self-contained.

**Phase 2 — the tracker seam, Linear and None only.** A pure refactor whose
proof is that `cargo test --workspace` stays green and every command behaves
identically. First commit extracts `config` into a new `devkit-config` crate.
Then `Kind` becomes an enum, the four match sites become exhaustive, the status
report is reshaped, the MCP field rename lands, `IssueRecord` moves, id recovery
becomes record-first, `classify` delegates to `issue_ref`, and the fake tracker
arrives with the tests it unblocks. Nothing new is user-visible except the
renamed status strings.

**Phase 3 — the GitHub tracker.** The adapter against a contract two
implementations already exercise, plus `[tracker]`/`[github]` config, the doctor
row, the `gh auth login` guidance on the `devkit auth` and doctor paths,
conventional-title parsing, schema regen, and docs. `issue_pr`'s cross-reference
mapping is the one piece to prototype before committing to the plan.

## Testing

TDD throughout; `cargo test --workspace` is the merge gate.

- **Fake tracker.** Test-only implementation in `devkit_common::tracker`. Drives
  `devkit-issue`'s `gather` end to end for the first time: discovery, state
  attachment, finished verdict, and the `None` degradation, all without a
  network.
- **GitHub parsing.** `linear.rs` already splits every operation into
  `*_query` / `parse_*` / networked wrapper. The GitHub adapter copies that
  split, and the parse functions run against recorded GraphQL fixtures captured
  from `K-Nette/BountyPop_GODOT` — including issue 87's full closed-with-timeline
  response, already in hand. Only the thin wrapper touches the network.
- **State mapping.** Table test over all four `(state, stateReason)` pairs plus
  a null `stateReason`, which older closed issues carry.
- **Conventional titles.** Table test: plain, `type:`, `type(scope):`,
  `type(scope)!:`, a colon inside the subject, and a title that is only a
  prefix.
- **Config expansion.** Set, unset (must error and name the key), `$$` escape,
  layer-relative resolution from two different working directories yielding the
  same path, and `~` still working.
- **Schema drift.** `schema/devkit-config.json` regenerates via
  `DEVKIT_UPDATE_SCHEMA=1 cargo test`; the committed-drift test fails until it
  does.

Windows CI matters here: per `AGENTS.md`, anything spawning processes must poll
rather than sleep. The tracker tests are network-free and unaffected, but the
`${VAR}` tests must not assume `$HOME` exists, since `expand_tilde` already
reads `HOME` only and silently no-ops where it is unset.

## Risks

| Risk | Mitigation |
|---|---|
| `issue_pr` cross-reference mapping is unproven | prototype against a real linked issue/PR pair before the phase-3 plan is written; fall back to "no linked PR" rather than guessing |
| Phase 2 is a wide refactor across five crates | it is behavior-preserving by construction; a green workspace test run plus unchanged CLI output is the gate |
| Detection surprises a user with a global Linear key | `devkit doctor` prints the resolved tracker and the reason; documented in `docs/configuration.md` |
| Layer-relative paths change existing relative-path configs | no known config uses a relative path for these keys; call it out in the release notes |
| The `config` extraction collides with concurrent work in this checkout | it touches 14 files by import line only; land it as its own commit, first, and rebase rather than merge |
| Bare-number ambiguity between issue #N and PR #N | resolved by decision: a bare number is a PR under the GitHub tracker |

`AGENTS.md`'s crate table gains a `devkit-config` row in phase 2,
`devkit-common` gains `tracker` and loses `linear`, and `devkit-ports` loses
`config` from its description.
`docs/configuration.md` gains `[tracker]`, `[github]`,
`[templates] parse_conventional_titles`, and the path-expansion rules.

## Discovered defects

Found while scoping this work. Neither is in scope; both should be filed.

1. **`docm` ignores `[docs]` in a `devkit.local.toml`.** `manifest.rs:167-180`
   walks upward reading only `devkit.toml`, while `config.rs:730` lists `docs`
   among the sections a standalone layer may carry and
   `docs/configuration.md:23` documents the local file as the untracked twin
   with the same shape and schema. The same walk also misses `[config] root =
   true` and `$DEVKIT_CONFIG`.

   The fix is not the deletion it looks like. `docm` merges `[[docs.libs]]`
   entry-by-entry by lib name, while `config::deep_merge` replaces arrays
   wholesale, so `devkit-docs` needs the *layer list*, not the merged table.
   That means making `discover` public and reworking `manifest::load` around it.
   Extracting `devkit-config` is the prerequisite, not the fix.

2. **`pr_number_from_url` lives in `linear.rs`.** It parses a GitHub PR URL and
   has no Linear content. Moves to `devkit_common::github` with this work.

## Resolved decisions

Settled during design; recorded so the plan does not relitigate them.

| Question | Decision |
|---|---|
| `stateReason` null on an old closed issue | `Completed`. The field defaulted to that behavior before it existed. |
| Bare-number ambiguity under GitHub | No disambiguation. A bare number is a PR; the issue URL addresses an issue. |
| A GitHub provider for `devkit auth` | No. `gh auth login` and `GH_TOKEN`/`GITHUB_TOKEN` already cover it. |
| Conventional-title stripping | On by default, `[templates] parse_conventional_titles = false` to disable. |
| Where `config` lives | Its own `devkit-config` leaf crate, not `devkit-common`. |

Because `devkit auth` gains nothing, the GitHub path must say so out loud or a
user will run `devkit auth` and find no GitHub option:

- `devkit auth` with no subcommand, and `devkit auth github` if typed, print
  that GitHub authentication is `gh auth login` or a `GH_TOKEN` /
  `GITHUB_TOKEN` export, and that devkit reads both (`github.rs:51`).
- `devkit doctor`'s tracker row, when the resolved tracker is GitHub and
  `github::token()` returns `None`, carries the same instruction as its hint,
  matching the existing `HINT_LINEAR` pattern (`doctor.rs:21`).
- Any tracker call failing on a missing token surfaces the `bearer()` message
  that already names both, rather than a bare 401.
