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

### Which repository, for issues and for PRs

devkit resolves one repository today, `repo_slug(cwd)` from the `origin` remote,
and uses it for both issues and pull requests. That holds only when origin owns
both. It does not in a fork: origin is the fork, PRs are opened against
upstream, and the issues may sit on either side. It also does not when a project
tracks issues in a repository separate from its code.

The two therefore become separately configurable, both defaulting to the origin
remote so a project that sets neither behaves exactly as it does today:

```toml
[github]
issues_repo = "org/planning"           # where issues live
pr_repo     = "mathix420/alacritree"   # where PRs are opened
```

This table sits at `[github]`, not `[tracker.github]`. `pr_repo` is not tracker
work: a project on Linear with a fork workflow needs it just as much, and
scoping it under the tracker would deny it to them.

**There is no head-owner setting, and none is derived.** `github::pr_by_head`
builds its query as `head={slug_owner}:{branch}`, taking the head owner from the
repository being searched. Cross-repository that is wrong and measurably so:
against `mathix420/alacritree`, `head=mathix420:fix/glyph-overhang-clipped`
returns nothing while `head=AbysmalBiscuit:...` returns PR 185.

Deriving the owner from `origin` was the first answer and it is not safe. Git
allows a push URL distinct from the fetch URL, `remote.pushDefault`, and
per-branch push remotes, so origin need not be where a branch was pushed. A
worktree created by `checkout-pr` from a contributor's PR has a different head
owner again.

So the owner is dropped from the query rather than computed. An unqualified
`head={branch}` matches PR 185 against `mathix420/alacritree`, so `pr_by_head`
searches `pr_repo` by branch name alone. Where that returns more than one
candidate, which two forks using the same branch name can produce, it refuses
and names them instead of ranking.

**A silent failure mode has to close with it.** `branch_pr_number` and its
siblings are written as `if let Some(found) = …and_then(|slug| pr_by_head(&slug,
b).ok())`, so a lookup that succeeds and finds nothing produces `Some(None)`,
satisfies the `if let`, and returns `Ok(None)` without ever reaching the `gh`
fallback. Today that is harmless because the head owner is always right; with
cross-repository lookups in play, a missed match becomes an authoritative and
silent "no PR". The unqualified query is what makes the short-circuit safe
again, and the reason is recorded here because the code reads as if the fallback
still covers it.

(`gh pr list --repo <upstream> --head <branch>` also finds the fork's PR, while
the owner-qualified `--head owner:branch` does not. The REST and `gh` paths
disagree, so each keeps the spelling verified for it.)

### The repository resolution seam

Every GitHub operation resolves its repository independently today. Nine sites
call `repo_slug(cwd)` inline, in `prs.rs`, `status.rs`, `checkout.rs`,
`review/request.rs`, `review/finish.rs` and `dashboard/data.rs`, and they share
one shape: attempt direct HTTP with an explicit slug, then fall back to a `gh`
invocation that infers the repository from the working directory.

That list is not the whole surface, and enumerating `repo_slug` callers is the
wrong way to find it. `gh pr edit --add-reviewer` in `review/request.rs` never
calls `repo_slug` at all and still depends on the working directory to pick a
repository. **The task is an audit of every GitHub operation**, whether it
reaches the API over HTTP or through `gh`, not a sweep of one function's
callers.

Both halves need the resolved value, so the resolution moves into one place. A
`Repos { issues, prs }` is resolved once per command from the config layer
plus the origin remote, and threaded to the sites that need it. The HTTP
half takes `repos.prs`; the `gh` half gains `--repo` whenever `repos.prs`
differs from origin, which covers `gh pr view`, `gh pr list`, `gh pr checkout`
and the `gh pr create` in `review/request.rs`.

`prs.rs` already holds a partial version of this in `resolve_repo(Option<&str>,
cwd)`, fed by `issue prs --repo`. The seam generalizes that rather than
inventing a parallel mechanism, and the flag keeps working as an explicit
override of `pr_repo` for one invocation.

`GithubTracker` is constructed with `repos.issues` rather than resolving a
repository per call, and `tracker::resolve` regains the `repo` parameter that
`8ccb43e` removed.

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

The ranking is done by the adapter, not by the server: a merged PR wins, then an
open one, then the highest number. `orderByState` is documented only as "return
results ordered by state" with no direction given, so nothing load-bearing rests
on it.

What does matter is seeing every candidate. A ranked window is worthless if the
winner sits outside it, and a tie that looks unique only because the second
candidate was truncated is worse than a visible tie. So the query requests
`pageInfo { hasNextPage }` alongside `first: 10`, and the adapter refuses rather
than ranking a truncated set. Ten is chosen over routine pagination because the
connection is empty or single in every observed case; `hasNextPage` is what
makes that choice safe instead of merely convenient.

Where the choice is genuinely ambiguous, meaning several merged or several open
candidates remain, `checkout-pr` refuses and names them rather than guessing,
because it is about to create a worktree from that answer. `states` and the
status report are read-only and take the ranked first.

### A PR outside `pr_repo`

With the seam in place, a PR upstream is no longer foreign: it is simply where
`pr_repo` points, and `status`, `review` and `checkout-pr` all read it. The
verified `gh pr checkout <n> --repo <pr_repo>` behavior is what makes this work
without an upstream remote, resolving the PR from `pr_repo` and fetching its
head from wherever the branch lives.

A PR outside `pr_repo` remains possible, because `issue_pr` returns whatever
repository closed the issue and that need not be the configured one. Left
unrecorded it would stall the lifecycle: `status` lists PRs for `repos.prs`, so
a PR elsewhere reports `pr_state` as `NO_PR` and the finished verdict says "no
PR" no matter what happens to it.

So `IssueRecord` gains an optional PR reference, the full URL, which identifies
both repository and number. `checkout-pr` writes it. It is absent on records
written before it existed and on `issue setup` worktrees that have no PR yet.

**A recorded PR is always authoritative, not only when it is elsewhere.** The
first version of this consulted the record only for a PR outside `repos.prs`,
which left the ordinary case on branch matching. That is not safe: `best_pr`
selects with `p.head_ref_name == head` and nothing more, from a listing capped
at 500, so two forks proposing `fix/crash` both match. Attaching the wrong PR
would let `issue end` judge a worktree finished on a stranger's merge. So
`status` queries a recorded URL exactly, in whatever repository it names, and
falls back to branch discovery only for a worktree whose record carries no PR.
Those keep today's behavior exactly.

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

Eight tasks. Task 1 lands first because everything else reads the repositories
it resolves, and it is the one task that touches Linear paths, so it carries its
own regression gate. Task 2 is the bulk. Tasks 3 and 4 are not optional and not
deferrable: without 3 the finished verdict never closes on a PR outside
`pr_repo`, and without 4 the dashboard silently shows nothing. Task 5 is the
switch that makes GitHub live, which is why it follows them rather than the
adapter.

1. **The repository resolution seam.** `[github] issues_repo` and `pr_repo`
   config **with the schema regenerated in this same task**, the `Repos {
   issues, prs }` resolution, every GitHub operation taking it instead of
   resolving its own, `--repo` on the `gh` paths including `gh pr edit`, and
   `pr_by_head` dropping the head-owner qualifier for an unqualified branch
   lookup that refuses ambiguity. **The gate is that a project setting neither
   key behaves identically**, Linear projects included, since this task alone
   touches every PR path devkit has.

   Schema regeneration cannot wait for the documentation task:
   `tests/config_schema.rs` compares the committed schema against the generated
   one and fails on any drift, so a task that adds config keys without it is red
   the moment it lands. Every task that changes a config type regenerates in
   place.
2. **The adapter.** `github.rs` with the query, parse and wrapper split, the
   `Tracker` implementation including the linked-PR ranking rule, and its
   fixture tests. No wiring yet.
3. **The PR reference in the record.** `IssueRecord` gains the optional PR URL,
   `checkout-pr` writes it, and `status` queries a recorded PR exactly rather
   than matching on branch name.
4. **Wire the dashboard to the trait.** `dashboard/data.rs` resolves the
   configured tracker and calls `assigned_history` and `timeline_origin` instead
   of Linear directly. Linear's behavior through the new path is the regression
   gate. **Every dashboard cache key gets scoped**, not just the issue one:
   `cache::path_for` is `cache_dir()/dashboard/{key}.json` with no project
   component, so `issues`, `pr-timeline-mine` and `pr-timeline-all` are already
   shared by every project on the machine. The key gains the tracker, the
   repository and the viewer identity.
5. **Selection and detection.** `TrackerKind::Github` constructs `GithubTracker`
   instead of the stand-in, with `declared` and the reason line;
   `tracker::resolve` regains `repo` and is handed `repos.issues`; detection
   validates the origin host is `github.com`. This lands after tasks 3 and 4 on
   purpose: it is the switch that makes GitHub live, and flipping it while the
   recorded-PR lifecycle or the dashboard is still half-wired would ship a
   tracker that reports confidently wrong verdicts.
6. **`devkit auth github`** and the doctor hint, with the identity taken from
   the resolved token rather than from the active `gh` account.
7. **Dogfood.** devkit's own `devkit.toml` declares `[tracker] kind = "github"`.
   Worth noting: this repository currently has no GitHub issues filed, so the
   declaration exercises the empty-tracker path and little else. It becomes real
   exercise once devkit work is filed as issues. Land it last, or defer it.
8. **Documentation.** `docs/configuration.md` gains the `[tracker] kind =
   "github"` row, the `[github]` table, and the auth instruction; `README.md` gains
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
- **Cache scoping.** Two projects on different trackers, and two viewers on one
  project, each get their own `issues` and `pr-timeline-*` entries rather than
  reading each other's.
- **Host validation.** Detection returns no tracker for a GitLab or Bitbucket
  origin, and GitHub for a `github.com` origin in each remote-URL shape
  `slug_from_remote_url` accepts.
- **Repository resolution.** Neither key set resolves both repositories and the
  head owner to origin; each key set independently; both set; and `issue prs
  --repo` still overriding `pr_repo` for one invocation. The identical-behavior
  gate for an unconfigured project is asserted at this layer rather than left to
  manual checking.
- **`pr_by_head` without an owner qualifier.** A fork PR is found by branch name
  alone against the upstream repository, is missed when the owner is taken from
  the searched repository, and two candidates sharing a branch name are refused
  rather than ranked.
- **Linked-PR completeness.** A response carrying `hasNextPage: true` is refused
  rather than ranked, so a winner outside the window can never be silently
  dropped.
- **Recorded PR precedence.** A record whose PR sits inside `pr_repo` is still
  queried by URL rather than matched by branch, proven by a fixture where a
  second PR shares the branch name and would otherwise win.
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
| Repository configuration | `[github] issues_repo` and `pr_repo`, separately configurable, both defaulting to origin. |
| Where `[github]` sits | Top level, not under `[tracker]`. `pr_repo` serves Linear projects on a fork too. |
| The PR head owner | Neither configured nor derived. `pr_by_head` searches by branch name alone and refuses ambiguity. |
| A recorded PR URL | Always authoritative, in any repository. Branch discovery serves only records without one. |
| Truncated linked-PR results | Refused via `hasNextPage`, never ranked. |
| Schema regeneration | In the task that changes the config type, not deferred to the documentation task. |
| When GitHub goes live | After the recorded-PR and dashboard tasks, so the switch never exposes a half-wired tracker. |
| `tracker::resolve`'s signature | Regains `repo`, handed `repos.issues`. |
| Host validation in detection | Added. A non-`github.com` origin no longer detects as GitHub. |
| The dashboard | Wired to the trait in this phase, not assumed. |
| A `devkit auth github` credential store | No. It reports the resolved token's identity and lists `gh` accounts as diagnostics. |
| Conventional-title parsing | Out of scope. Its own spec. |
| Fixture content | Synthesized in the shape of the originals, never copied. |
| devkit's own tracker | Declares `github`, landed last, with the empty-repository caveat stated. |
