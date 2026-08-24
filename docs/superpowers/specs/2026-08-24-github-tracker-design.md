# The GitHub tracker

**Date:** 2026-08-24
**Status:** ready to plan, after one round of adversarial cross-model
review. See `2026-08-24-github-tracker-review-log.md`.
**Parent:** `2026-08-23-pluggable-issue-tracker-design.md`, phase 3. That spec's
phases 1 and 2 shipped. This one supersedes its "The GitHub mapping" section,
which a live probe disproved in two places.

Phase 2 left a `Tracker` trait with Linear and no-tracker behind it. This spec
fills the third slot: a GitHub Issues adapter, selected by `[tracker] kind =
"github"`, so a project that tracks work in GitHub gets the title-derived slug,
the summary file, the state column, the state gate, and the dashboard timeline
that Linear projects already get.

## What changed since the parent spec

The parent spec called `issue_pr` "the least certain mapping in the design" and
asked for a probe before planning. The probe ran on 2026-08-24 against
`K-Nette/BountyPop_GODOT` (28 issues, 73 PRs) and `AbysmalBiscuit/alacritree`
(12 issues, 4 PRs). It disproved the mapping.

**`ConnectedEvent` never fires.** Zero occurrences across every issue in both
repositories. It records a manual link made through the Development sidebar,
which nobody uses. Half the parent spec's query was dead.

**`willCloseTarget` goes false once the issue closes.** Issues 78, 87, 79 and 80
each have a merged PR that closed them, and `CrossReferencedEvent` reports
`willCloseTarget: false` for all four. Filtering on it loses the PR for exactly
the closed issues the finished verdict reads. `CrossReferencedEvent` also
carries issue-to-issue references, so its `source` needs a typename filter.

**`closedByPullRequestsReferences` answers directly.** With
`includeClosedPrs: true` it returned the correct PR for all ten linked issues,
open and closed alike, with the repository attached and no unrelated sources.
One field replaces the timeline walk.

**A linked PR is routinely in another repository.** In
`AbysmalBiscuit/alacritree`, issues 6 and 7 are closed by PRs 185 and 184 in
`mathix420/alacritree`. That is the ordinary fork workflow, and the parent
spec's "filter to PRs in the same repo" rule would report both issues as having
no PR.

**`gh` handles the cross-repository checkout, given the repository.** In a clone
whose only remote is the fork, `gh pr checkout 185` fails with "Could not
resolve to a PullRequest with the number of 185". With `--repo
mathix420/alacritree` it succeeds, resolving the PR from upstream and fetching
its head branch from the fork, where the branch actually lives. No upstream
remote is needed.

Confirmed as the parent spec described: the dashboard's
`timelineItems(itemTypes: [CLOSED_EVENT, REOPENED_EVENT])` history returns
per-event `createdAt` and `stateReason` (verified on issue 87), and
`AbysmalBiscuit/BountyPop_GODOT` is an empty fork, so `K-Nette/BountyPop_GODOT`
is the live repository.

## Design

### The adapter

`crates/devkit-common/src/tracker/github.rs`, mirroring `linear.rs`'s split of
every operation into `*_query`, `parse_*`, and a networked wrapper. Only the
wrappers touch the network, so each parse function tests against a recorded
response. It builds on `devkit_common::github`, which already provides
`graphql()`, `repo_slug(cwd)`, and `token()` with its `GH_TOKEN` /
`GITHUB_TOKEN` / `gh auth token` chain, and is already wrapped in a timing span.

### Which repository the adapter reads

`repo_slug(cwd)` is the default and is right for the observed repositories:
alacritree keeps its issues on the fork, which is `origin`. But origin-only
resolution cannot serve every layout. In the ordinary fork workflow the issues
live upstream while `origin` is the fork, and a project may deliberately track
issues in a repository separate from its code. Both cases would query the wrong
repository and find no issue.

So `[github] repo` is retained as an optional override, defaulting to the origin
remote. That answers the objection it was dropped for: the repository *is*
derived from the worktree, and the override exists only for the layouts
derivation cannot reach. `tracker::resolve` regains the `repo` parameter that
`8ccb43e` removed, and `GithubTracker` is constructed with a resolved slug
rather than resolving one per call.

**Detection must validate the host.** `slug_from_remote_url` parses any
`https://host/owner/repo` shape without checking the host, so
`https://gitlab.com/o/r` yields `o/r` and `repo_slug` succeeds. `detect()` reads
a successful `repo_slug` as proof of a GitHub origin, so a GitLab or Bitbucket
project currently detects as GitHub. Detection gains a host check against
`github.com`. `slug_from_remote_url` itself is left alone, since its other
callers already know they hold a GitHub URL.

### Per-method mapping

| Method | Source |
|---|---|
| `kind` | `TrackerKind::Github` |
| `ready` | `github::token()` resolves and `repo_slug` succeeds |
| `issue_ref` | strips a leading `#`; recognizes a `github.com/…/issues/N` URL |
| `title` / `details` | `repository.issue(number:)` fields |
| `states` | `state` + `stateReason`, batched by alias the way `linear::build_query` batches |
| `issue_pr` | `closedByPullRequestsReferences(first: 10, includeClosedPrs: true, orderByState: true)`, any repository; see below |
| `candidates` | empty; a bare number is a PR |
| `issues_for_prs` | each PR's `closingIssuesReferences` |
| `assigned_history` | `issues(filterBy: {assignee: <viewer login>})`, paginated, each node carrying its `CLOSED_EVENT` / `REOPENED_EVENT` timeline |
| `timeline_origin` | earliest issue `createdAt` in the repository |
| `issue_url` | `https://github.com/{slug}/issues/{n}` |
| `check` | viewer login, for `devkit doctor`'s identity line |

### State mapping

| GitHub | `StateKind` | `State.name` |
|---|---|---|
| `OPEN` | `Started` | `Open` |
| `CLOSED` + `COMPLETED` | `Completed` | `Done` |
| `CLOSED` + `NOT_PLANNED` | `Canceled` | `Not planned` |
| `CLOSED` + `DUPLICATE` | `Canceled` | `Duplicate` |
| `CLOSED` + null | `Completed` | `Done` |

Unchanged from the parent spec, including its reasoning that `OPEN` maps to
`Started` rather than `Unstarted`: GitHub gives no signal separating a backlog
issue from one in progress, and deriving one from assignee presence would invent
bands the data cannot support.

Neither probed repository holds a `NOT_PLANNED` or `DUPLICATE` issue, so those
two rows rest on the table test alone. That is acceptable, since `stateReason`
is a closed enum the API documents, but it should be stated rather than implied.

### The assignee filter needs a login, not a placeholder

`filterBy` takes a concrete `assignee` login and has no `@me` equivalent.
Filtering on the repository owner is wrong: in `K-Nette/BountyPop_GODOT` every
assigned issue belongs to `AbysmalBiscuit`, so `assignee: "K-Nette"` returns
nothing while `assignee: "AbysmalBiscuit"` returns 27 of the 28. So
`assigned_history` resolves `viewer { login }` first, then filters on it. The
timeline nests inside the same query, so it stays one paginated round trip per
page as the parent spec intended.

`issues_for_prs` is likewise verified: `closingIssuesReferences` on a PR returns
the issues it closes, and an empty list for a PR that closes none.

### Choosing among several linked PRs

An issue may have more than one linked PR: a superseded attempt, or a reopened
issue closed twice. `first: 1` would return an arbitrary one, and the field
documents no default ordering. Neither probed repository currently has such an
issue, so this is an unobserved risk rather than a live bug, but the failure
mode is silent and wrong rather than loud.

The field takes an `orderByState` argument. The query asks for `first: 10,
orderByState: true` and the adapter picks by an explicit rule: a merged PR wins,
then an open one, then the highest number. Ten is chosen over full pagination
because the connection is empty or single in every observed case, and paginating
it would be more than the job needs.

Where the choice is genuinely ambiguous, meaning several merged or several open
candidates remain, `checkout-pr` refuses and names them rather than guessing,
because it is about to create a worktree from that answer. `states` and the
status report are read-only and take the ranked first.

### Cross-repository checkout

`PrRef` already carries `url`, so `issue_pr` returning a PR in another
repository needs no type change. `checkout-pr` parses `owner/repo` out of that
URL and passes `--repo` to both `gh` calls it makes, `gh pr view` in
`fetch_pr_meta` and `gh pr checkout` in the worktree, whenever the parsed slug
differs from `repo_slug` of the monorepo. `github.rs` already holds
`pr_number_from_url` and `slug_from_remote_url` for the parser to sit beside.

The same-repository case passes no `--repo` and behaves exactly as today.

**The PR must be persisted, or the lifecycle stalls.** `IssueRecord` carries no
PR reference, and `status::fetch_prs_http` lists PRs for `repo_slug(cwd)` only.
A worktree checked out from a PR in another repository would therefore find no
matching PR, report `pr_state` as `NO_PR`, and the finished verdict would say
"no PR" forever no matter what happened upstream. Returning a cross-repository
PR without recording it is worse than filtering it out.

So `IssueRecord` gains an optional PR reference: the full URL, which already
identifies both the repository and the number. It is written by `checkout-pr`,
absent on records written before it existed and on `issue setup` worktrees that
have no PR yet. `status` reads it and, when the recorded PR is outside the
origin repository, queries that repository for that one PR instead of relying on
the origin PR listing. Worktrees with no recorded PR keep today's behavior
exactly.

### Authentication

devkit stores no GitHub credential of its own. `gh auth login`, `GH_TOKEN` and
`GITHUB_TOKEN` already cover it, and `github::token()` already reads all three.
So `devkit auth github` reports rather than stores.

**The identity comes from the token, not from `gh`.** `resolve_token` reads
`GH_TOKEN`, then `GITHUB_TOKEN`, and only then falls back to `gh auth token`. So
with either variable set, the active `gh` account is not the identity devkit
uses, and reporting it as such would mislead precisely the user who most needs
the answer. The command resolves the token the way `github::token()` does,
queries `viewer { login }` with it, and reports that login and which source
supplied the token.

Beneath that it lists the `gh` accounts from `gh auth status --json hosts`,
which returns per account a `login`, `host`, `active` flag, `scopes` string and
`state`. That list is secondary diagnostics: it explains what `gh auth token`
would return and lets the user spot a login they forgot, but it is not the
identity line.

With no token resolvable at all, or when `--json` is unsupported by an older
`gh`, it prints the `gh auth login` instruction and the two environment
variables. Bare `devkit auth` lists `github` among its providers with the same
one-line summary.

`devkit doctor`'s tracker row, when the resolved tracker is GitHub and
`github::token()` returns `None`, carries that instruction as its hint, matching
the existing `HINT_LINEAR` pattern.

### Selection

`TrackerKind::Github` currently resolves to the no-tracker stand-in with
`declared: false`. It gains its real arm: a `GithubTracker`, `declared` from
whether the config named it, and a reason line distinguishing a declared kind
from detection.

Detection order is unchanged: a resolvable `LINEAR_API_KEY`, then a GitHub
`origin` remote, then no tracker. The parent spec's warning still holds. A
globally exported `LINEAR_API_KEY` resolves to Linear for every project, so a
GitHub project on such a machine must name `kind` explicitly, and `devkit
doctor` prints which tracker resolved and why.

### The dashboard is not wired to the trait yet

Phase 2 moved Linear behind the trait for `status` and `end`, but not for the
dashboard. `assigned_history` and `timeline_origin` have no caller anywhere
outside the tracker module: they are dead trait methods. `dashboard/data.rs`
returns an empty list when `LINEAR_API_KEY` does not resolve and otherwise calls
`linear::assigned_issue_history_with_progress` directly.

So implementing those two methods on `GithubTracker` delivers nothing on its
own. The dashboard would show an empty timeline for a GitHub project and never
say why. Wiring it is part of this phase, not an assumed inheritance: the
dashboard resolves the configured tracker the way `status` does and calls the
trait, so Linear keeps its current behavior through the same path.

Its cache needs scoping in the same change. `dashboard/data.rs` caches under the
literal key `"issues"`, which is global, so two projects already share one cache
entry. Two projects on different trackers would serve each other's timelines.
The key gains the tracker kind and the repository or workspace identity.

## Non-goals

- **Write operations.** `devkit-issue` is a read-only triage facade. No
  assigning, no state transitions, no comments.
- **Conventional-title parsing.** The parent spec bundled `from_issue_title`,
  `[templates] parse_conventional_titles`, and `type` / `scope` in the render
  context into this phase. They are independent of GitHub and they change Linear
  branch names too, so they get their own spec. Nothing here depends on them.
- **A `devkit auth github` credential store.** Reporting only.
- **GitHub Projects v2 and label-as-status.** Neither is in use in the observed
  repositories, and Projects needs a token scope the account lacks. Both remain
  additive later: they change one `states()` implementation and add config.
- **A `command` provider.** Unchanged from the parent spec. Revisit once two
  real implementations have exercised the trait.

## Delivery

Eight tasks. Task 1 is the bulk. Tasks 2 through 4 each close a gap the review
found and none of them is optional: without 4 the dashboard silently shows
nothing, and without 3 the finished verdict never closes on a cross-repository
PR.

1. **The adapter.** `github.rs` with the query, parse and wrapper split, the
   `Tracker` implementation including the linked-PR ranking rule, and its
   fixture tests. No wiring yet.
2. **Selection and detection.** `TrackerKind::Github` constructs `GithubTracker`
   instead of the stand-in, with `declared` and the reason line; `[github] repo`
   config with the origin default; `tracker::resolve` regains `repo`; detection
   validates the origin host is `github.com`.
3. **The PR reference in the record.** `IssueRecord` gains the optional PR URL,
   `checkout-pr` writes it and passes `--repo` to `gh` when the PR is outside
   origin, and `status` consults it for a PR the origin listing cannot see.
4. **Wire the dashboard to the trait.** `dashboard/data.rs` resolves the
   configured tracker and calls `assigned_history` and `timeline_origin` instead
   of Linear directly, and the cache key gains the tracker and repository
   identity. Linear's behavior through the new path is the regression gate.
5. **`devkit auth github`** and the doctor hint, with the identity taken from
   the resolved token rather than from the active `gh` account.
6. **Dogfood.** devkit's own `devkit.toml` declares `[tracker] kind = "github"`.
   Worth noting: this repository currently has no GitHub issues filed, so the
   declaration exercises the empty-tracker path and little else. It becomes real
   exercise once devkit work is filed as issues. Land it last, or defer it.
7. **Documentation.** `docs/configuration.md` gains the `[tracker] kind =
   "github"` row, `[github] repo`, and the auth instruction; `README.md` gains
   `devkit auth github`; `AGENTS.md`'s tracker paragraph drops "there is no
   GitHub implementation" and describes the real arm; `skills/using-devkit/`'s
   CLI reference gains the new subcommand. Schema regenerates via
   `DEVKIT_UPDATE_SCHEMA=1 cargo test`.

## Testing

TDD throughout; `cargo test --workspace` is the merge gate.

- **Fixtures.** Recorded GraphQL responses captured from the two probed
  repositories, covering a linked open issue, a linked closed issue, a
  cross-repository link, an issue with no PR, and an issue whose only
  cross-reference is another issue. **Issue titles, bodies and branch names in
  the fixtures are synthesized in the same shape as the originals, not copied
  verbatim.** The structure and field coverage come from the real responses; the
  content does not.
- **State mapping.** Table test over every `(state, stateReason)` pair including
  a null reason. `NOT_PLANNED` and `DUPLICATE` are synthetic.
- **`issue_pr`.** Same-repository and cross-repository fixtures assert the
  returned `PrRef.url`, and a no-link fixture asserts `None`.
- **Cross-repository checkout.** A unit test on the slug parser and the decision
  to pass `--repo`, since the `gh` invocation itself is not testable offline.
- **`devkit auth github`.** Parse tests over a `gh auth status --json hosts`
  response with several accounts, an empty response, and a malformed one.
- **Linked-PR ranking.** Table test over one merged PR, one open PR, a merged
  and an open together, two merged, and none, asserting the chosen PR and that
  the two-merged case is reported as ambiguous rather than silently ranked.
- **The recorded PR.** A record carrying a cross-repository PR URL drives status
  to the finished verdict; a record without one keeps today's behavior; an old
  record with no field at all still deserializes.
- **Host validation.** Detection returns no tracker for a GitLab or Bitbucket
  origin, and GitHub for a `github.com` origin in each remote-URL shape
  `slug_from_remote_url` accepts.
- **The dashboard through the trait.** The fake tracker drives the timeline, so
  the wiring is covered without a network call, and Linear's path is asserted
  unchanged.
- **The fake tracker** from phase 2 keeps driving `devkit-issue`'s `gather` end
  to end. Nothing in this phase requires the network to test.

Windows CI applies as always: nothing here spawns processes in tests, and the
adapter is network-free under test.

## Risks

| Risk | Mitigation |
|---|---|
| `NOT_PLANNED` and `DUPLICATE` are untested against live data | the API documents `stateReason` as a closed enum; the table test covers both, and a wrong mapping degrades to a state label rather than a crash |
| An older `gh` lacks `auth status --json` | detected and degraded to the plain `gh auth login` instruction |
| `assigned_history` is repository-scoped, so work assigned in other repositories is invisible in the dashboard | deliberate, carried over from the parent spec: a cross-repository search drags in every drive-by assignment and costs one timeline query per repository |
| A globally exported `LINEAR_API_KEY` silently wins over a GitHub project | `kind` must be named explicitly; `devkit doctor` prints the resolved tracker and the reason |
| Rate limits on a repository with many issues | one paginated query per operation, batched by alias the way Linear's is; no per-issue round trips |

## Resolved decisions

| Question | Decision |
|---|---|
| How `issue_pr` finds the PR | `closedByPullRequestsReferences`, not the timeline. Probe-driven. |
| Several linked PRs | `orderByState` plus an explicit merged / open / highest rule; `checkout-pr` refuses a genuine tie. |
| A linked PR in another repository | Returned, not filtered, and persisted in `IssueRecord` so status can see it. |
| `[github] repo` config | Retained as an optional override with the origin remote as default. Reversed after review. |
| `tracker::resolve`'s signature | Regains `repo`. |
| Host validation in detection | Added. A non-`github.com` origin no longer detects as GitHub. |
| The dashboard | Wired to the trait in this phase, not assumed. |
| A `devkit auth github` credential store | No. It reports the resolved token's identity and lists `gh` accounts as diagnostics. |
| Conventional-title parsing | Out of scope. Its own spec. |
| Fixture content | Synthesized in the shape of the originals, never copied. |
| devkit's own tracker | Declares `github`, landed last, with the empty-repository caveat stated. |
