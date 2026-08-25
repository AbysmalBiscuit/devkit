# The GitHub tracker

**Date:** 2026-08-24
**Status:** ready to plan, after eight rounds of adversarial cross-model
review. See `2026-08-24-github-tracker-review-log.md`.
**Parent:** `2026-08-23-pluggable-issue-tracker-design.md`, phase 3. That spec's
phases 1 and 2 shipped. This one supersedes its "The GitHub mapping" section,
which a live probe disproved in two places.

Phase 2 left a `Tracker` trait with Linear and no-tracker behind it. This spec
fills the third slot: a GitHub Issues adapter, selected by `[tracker] kind =
"github"`, so a project that tracks work in GitHub gets the title-derived slug,
the summary file, the state column, the state gate, and the dashboard timeline
that Linear projects already get.

Delivering that takes more than the adapter. Phase 2 moved `status` and `end`
onto the trait and stopped there, so `setup`, `checkout-pr`'s bare-number arm,
`prs` and the dashboard still call Linear directly and five trait methods have
no caller at all. Wiring them is part of this phase, not an inheritance from the
last one.

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

**So the lookup moves off REST onto GraphQL.** Dropping the owner from the REST
query does work against the live API, but REST documents `head` only in the form
`user:ref-name`; an unqualified value is undocumented behavior that a probe can
observe and cannot make stable. GraphQL's `Repository.pullRequests` takes
`headRefName` as a documented argument, matches a fork's head branch without any
owner qualifier, and is the mechanism `gh pr list --head` itself uses. Verified
against `mathix420/alacritree`:
`pullRequests(headRefName: "fix/glyph-overhang-clipped")` returns PR 185, whose
`headRepositoryOwner` is `AbysmalBiscuit`, with `totalCount: 1`.

`totalCount` alongside the nodes is what makes ambiguity detectable rather than
inferred, so two forks proposing the same branch name are seen as two.

### The PR lookup answers with a type, not an `Option`

`pr_by_head` today returns `Result<Option<PrBrief>>` and every caller collapses
it. `branch_pr_number`, `existing_pr` and `fetch_pr_full` are each written as
`if let Some(found) = …and_then(|slug| pr_by_head(&slug, b).ok())`, which fails
in two directions at once. A lookup that succeeds and finds nothing yields
`Some(None)`, satisfies the `if let`, and returns "no PR" without consulting
`gh`. A lookup that errors yields `None`, falls through to `gh pr list --head
<branch> --limit 1`, and takes an arbitrary one of several matches. Refusing
inside `pr_by_head` would therefore change nothing: the refusal is swallowed by
`.ok()` and the guess happens anyway.

So both transports, HTTP and `gh`, return the same four-way answer, and it is a
tagged value rather than a pair of nullable fields:

| Answer | Meaning | Caller |
|---|---|---|
| `Unique(pr)` | exactly one PR has this head branch | use it |
| `NoMatch` | the transport answered; there is no such PR | trust it; do **not** fall back |
| `Ambiguous(Vec<pr>)` | several candidates share the branch name | see below |
| `Unavailable(reason)` | no token, or the request failed | fall back to the other transport |

Only `Unavailable` reaches the fallback. That alone closes the silent
short-circuit, independently of the ambiguity work.

Ambiguity divides by what the caller is about to do:

- **Acting paths refuse.** `review request` is about to create or comment on a
  PR and `review finish` is about to merge or close one; `checkout-pr` is about
  to build a worktree from the answer. Each surfaces the candidates and stops.
- **Read-only paths record it.** `issue status` reports the row as ambiguous
  with its candidates, and the finished verdict stays closed with that as its
  reason. A report that cannot identify the PR must not claim the work is done —
  `issue end` reads this verdict to decide whether a worktree may be deleted,
  and a stranger's merged PR is exactly the input that must not authorize a
  deletion.

**`IssueWorktree` cannot carry that as it stands.** The row holds `pr_state:
String`, one `pr_number` and one `pr_url`, so an ambiguous answer has nowhere to
put its candidates and degrades to a state string nothing else understands.
`triage.rs` renders `format!("{} #{}", row.pr_state,
row.pr_number.unwrap_or(0))`, so an `"AMBIGUOUS"` string with no number prints
as `AMBIGUOUS #0` — a PR number that does not exist, in a column a human reads
to decide whether to delete a worktree. The MCP `issue.status` action serializes
the same struct, so its consumers would receive that too, with no structured way
to see what the candidates were.

So the row carries the tagged status itself — `Unique(PrRef)`, `NoMatch`,
`Ambiguous(Vec<PrRef>)`, `Unavailable` — and `pr_state`, `pr_number` and
`pr_url` become values derived from it for display and for the existing
serialized shape. One source of truth, and every consumer that wants the detail
can reach it: the finished verdict reads the tag rather than comparing strings,
`triage.rs` renders `ambiguous (2)` from the tag instead of formatting a missing
number, and MCP gains the candidate list rather than a string it must parse.

**The capped listing has to go with it.** `best_pr` in `status.rs` is the third
place a guess is made, and simply feeding it the typed answer would not fix it,
because its input is wrong: `fetch_prs` pulls the repository's PRs with `gh pr
list --state all --limit 500` (or the HTTP equivalent) and `best_pr` selects on
`p.head_ref_name == head` from whatever came back. A window that truncates
produces a false `Unique` — or a false `NoMatch` — with no signal that it did,
which is precisely the shape of error the recorded-PR rule exists to prevent.
Ambiguity computed from a truncated set is not ambiguity detection.

So `status` resolves each distinct worktree branch through the same head lookup
the rest of the design uses, batched into one round trip by GraphQL alias the
way `linear::build_query` batches its state queries. The number of distinct
branches is the number of issue worktrees, which is small; the repository's
total PR count, which is what the 500 cap is fighting, stops mattering.

(`gh pr list --repo <upstream> --head <branch>` finds the fork's PR, while the
owner-qualified `--head owner:branch` does not. The `gh` fallback keeps the
spelling verified for it, and `--limit 1` is dropped so it can see a second
candidate.)

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
`Repos { issues, prs }` is resolved once per command from the config layer plus
the origin remote, and threaded to the sites that need it. The HTTP half takes
`repos.prs`; the `gh` half gains `--repo`, which covers `gh pr view`, `gh pr
list`, `gh pr checkout` and the `gh pr create` in `review/request.rs`.

**`--repo` is passed on every repository-scoped `gh` command, with no exemption
for a value that came from origin.** Omitting it looks like a harmless
optimization and is not one. `cmd::capture` inherits the environment, so `gh`
still reads `GH_REPO`. Exempting the origin-defaulted case does not help: the
HTTP half would use the slug devkit resolved from origin while the `gh` half
followed `GH_REPO` somewhere else, so one `review request` could read PR A over
HTTP and then edit, check out, or create against repository B. The two halves
must name the same repository or they are not one seam.

"Repository-scoped" is the whole rule, not a softening of it. `gh pr` and its
subcommands take `--repo`; `gh auth token`, `gh auth status` and `gh api
graphql` do not, and devkit uses all three. `gh api graphql` carries the
repository the only way it can, as query variables. The requirement is that no
invocation lets the ambient environment choose the repository, not that a
particular flag appears.

**The host is part of that, and `--repo owner/repo` does not pin it.** `GH_HOST`
selects an enterprise host, so an unscoped `gh auth token` can hand back an
enterprise credential that `github.rs` then sends to `api.github.com` — a token
disclosed to a host it was not issued for, which is worse than a wrong-repository
read. Authentication commands are not repository-scoped but they are host-scoped.
So `--repo` is spelled `github.com/owner/repo`, and `gh api` and `gh auth token`
carry `--hostname github.com`. GitHub Enterprise is out of scope for this phase;
naming the host explicitly is what keeps it out rather than silently half-in.

Each field still records whether it was configured, overridden on the command
line, or defaulted from origin. That provenance is no longer what decides
`--repo`; it decides which key an error is allowed to demand, below.

**The two keys resolve independently.** Requiring both to resolve makes a Linear
project with an explicit `pr_repo` and no GitHub origin configure an
`issues_repo` it will never use, which contradicts the reason `pr_repo` sits
outside `[tracker]` at all. So each key resolves on its own and is required only
where it is used: `issues_repo` by the GitHub tracker, `pr_repo` by the PR
paths. A missing one is an error at the operation that needs it, naming that
key.

**Defaulting from origin requires a `github.com` origin.** Host validation
belongs to every path that reads the remote, not to detection alone. A project
that declares `kind = "github"` skips detection entirely, and `repo_slug` does
not check the host, so a GitLab origin would yield `owner/repo`, default both
repositories to it, and query an unrelated `github.com/owner/repo` that may well
exist — a tracker reporting `ready` about someone else's issues. So the host
check guards the origin fallback itself. Explicitly configured repositories need
no origin and are unaffected.

`prs.rs` already holds a partial version of this in `resolve_repo(Option<&str>,
cwd)`, fed by `issue prs --repo`. The seam generalizes that rather than
inventing a parallel mechanism, and the flag keeps working as an explicit
override of `pr_repo` for one invocation.

`GithubTracker` is constructed with `repos.issues` rather than resolving a
repository per call, and `tracker::resolve` regains the `repo` parameter that
`8ccb43e` removed.

**Resolution happens before construction, and origin is consulted only for what
config did not supply.** A project that sets `issues_repo` and `pr_repo` has
named both repositories outright; asking the `origin` remote for them again is
pointless, and failing when there is no GitHub origin is wrong. So `Repos` is
resolved first — each key from config, falling back to the origin remote for a
key config left unset — and the resolved value is handed to the tracker. Origin
is required only when a key is missing, and the error names which key would fix
it.

This is also what lets `ready` rest on **the token alone**, rather than on
`repo_slug(cwd)`. The repository half is settled once, at construction:
`tracker::resolve` is the only place a `GithubTracker` is built, and it builds
one only from an issues repository that already resolved, so by the time
anything can call `ready` there is nothing left for it to check. That makes the
single construction site load-bearing — a second one that skipped the check
would leave `ready` claiming readiness with no repository to ask. A GitHub
project whose code lives elsewhere, or one worked on from a directory with no
GitHub remote, is ready when its config says which repository holds the issues.

**Detection must validate the host too.** `slug_from_remote_url` parses any
`https://host/owner/repo` shape without checking the host, so
`https://gitlab.com/o/r` yields `o/r` and `repo_slug` succeeds. `detect()` reads
a successful `repo_slug` as proof of a GitHub origin, so a GitLab or Bitbucket
project currently detects as GitHub. Detection gains the same `github.com` check
the origin fallback gains above; they are one rule applied wherever the remote
is read. `slug_from_remote_url` itself is left alone, since its other
callers already know they hold a GitHub URL.

### Per-method mapping

| Method | Source |
|---|---|
| `kind` | `TrackerKind::Github` |
| `ready` | `github::token()` resolves. The issues repository was already resolved to construct the tracker, so nothing checks it again |
| `issue_ref` | a bare number, or a `github.com/…/issues/N` URL whose repository is `issues_repo` — one outside it is refused. No `#` stripping: `classify` resolves `#N` to a PR before any tracker is asked, and no implementation of this method strips one |
| `title` / `details` | `repository.issue(number:)` fields |
| `states` | `state` + `stateReason`, batched by alias the way `linear::build_query` batches |
| `issue_pr` | `closedByPullRequestsReferences(first: 10, includeClosedPrs: true, orderByState: true)`, any repository; see below |
| `candidates` | empty; a bare number is a PR |
| `issues_for_prs` | each PR's `closingIssuesReferences`, as a bare number for an issue in the tracker's own issues repository and `owner/name#number` for one anywhere else. The repository comparison is case-insensitive, since GitHub echoes the owner and name as they are spelled on the repository rather than as configured |
| `assigned_history` | `issues(filterBy: {assignee: <viewer login>})`, paginated, each node carrying its `CLOSED_EVENT` / `REOPENED_EVENT` timeline |
| `timeline_origin` | earliest issue `createdAt` in the repository |
| `issue_url` | `https://github.com/{slug}/issues/{n}`; an `owner/name#number` id — the cross-repository form `issues_for_prs` emits — routes to the repository it names, a bare number to the tracker's own. Linear identifiers are `ENG-1`, so the two id vocabularies cannot collide |
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

The ranking is done by the adapter, not by the server, and the tuple is stated
in full because a partial one reads as a tie-breaker while behaving as a
refusal. Candidates are ordered by state — merged, then open, then closed — and
within the top state group by PR number, highest first. `orderByState` is
documented only as "return results ordered by state" with no direction given, so
nothing load-bearing rests on it.

Number ordering only means something inside one repository. Two PRs in different
repositories have unrelated numbering, so `#5` upstream is not "older" than
`#900` in a fork. **A tie is therefore candidates that share the top state and
span more than one repository**, and only that. Two merged PRs in the same
repository are ranked, not refused: the higher number is the later attempt.

What does matter is seeing every candidate. A ranked window is worthless if the
winner sits outside it, and a tie that looks unique only because the second
candidate was truncated is worse than a visible tie. So the query requests
`pageInfo { hasNextPage }` alongside `first: 10`, and the adapter refuses rather
than ranking a truncated set. Ten is chosen over routine pagination because the
connection is empty or single in every observed case; `hasNextPage` is what
makes that choice safe instead of merely convenient.

**Every nested connection gets the same treatment, not just this one.** A
GraphQL connection nested inside a paginated one does not paginate with its
parent, so walking the outer pages silently truncates each inner list at its
`first:`. Two in this design are nested: each issue's `timelineItems` inside
`assigned_history`'s paginated `issues`, and `closingIssuesReferences` inside
`issues_for_prs`. An issue closed and reopened more times than the window holds
would contribute a truncated transition history to the dashboard chart, and a PR
closing many issues would report only some of them — both wrong quietly, in a
place nothing else would contradict.

So every connection in every query requests `pageInfo { hasNextPage }`, and a
truncated one is either paginated or reported as incomplete. Which of the two is
per connection: `closingIssuesReferences` reports incomplete, since a partial
answer there feeds a link column that is better blank than wrong;
`timelineItems` paginates, since a chart missing transitions is not visibly
wrong at all.

Where a tie survives that rule — the top state group spanning repositories —
`checkout-pr` refuses and names the candidates rather than guessing, because it
is about to create a worktree from that answer. `states` takes the ranked first,
being a state column with no action behind it. The status report does not: it
carries the ambiguity into the row and leaves the finished verdict closed, the
same as for a head lookup, because `issue end` reads that verdict.

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
both repository and number. It is absent on records written before it existed
and on an `issue setup` worktree whose PR does not exist yet.

**Every path that learns the PR writes it, not only `checkout-pr`.** The
ordinary lifecycle is `issue setup` then `issue review request`: setup has no PR
to record, and `review request` is the command that finds or creates one. If
only `checkout-pr` wrote the field, that whole flow would leave the record empty
and fall back to the branch matching this section exists to replace — the
authoritative record would be authoritative for exactly the case that did not
need it. So `review request` persists the PR URL whenever it resolves one,
whether it found an existing PR or opened a new one.

**That write is not sufficient as the rebind, and `review request` needs an
explicit one.** Letting the write itself serve as the rebind assumed the command
could always discover the right PR again, and it cannot. The two rules collide:
if `review request` honors the record it can never escape a stale or wrong
binding, and if it ignores the record and searches `pr_repo` it can rebind to
the wrong PR — which is the branch matching this section exists to replace. The
superseded case closes the trap: an old PR and its replacement share a head
branch, the head lookup returns `Ambiguous`, and an acting path refuses. The
command that was supposed to fix the binding is the one that cannot run.

Ranking the candidates by state would paper over that one case and open a worse
one, since ranking across head owners is how a stranger's merged PR wins. So the
rule stays strict and gains a human-supplied escape:

- Precedence is **explicit locator, then recorded locator, then branch
  discovery.** `review finish` already takes a `--pr <number>` that wins over
  branch discovery by contract; leaving the record unconditionally on top would
  either disable that flag silently or leave an undocumented way around the new
  rule. Naming the order settles both.
- `--pr` means one thing everywhere: *use this PR for this run*. A URL names its
  own repository, a bare number means `pr_repo`, and it gains the URL form on
  `review request`. It does not itself write anything. Rebinding falls out of
  the rule already stated — `review request` records whatever PR it acted on —
  so `review request --pr <URL>` replaces a wrong binding, while `review finish
  --pr <n>` overrides for that run and leaves the record alone.
- Branch discovery runs only when neither locator is present.

**Every PR `review request` acts on has to belong to this worktree, and the
branch name does not prove it.** The check is not about how the PR was chosen.
An explicit `--pr` with a mistyped number names a real PR that resolves cleanly;
a recorded locator can have been written when the branch meant something else;
and a branch-discovered `Unique` is unique only in that one repository's PRs
share the name — another fork's same-named branch produces exactly the same
answer. All three then drive reviewer edits, Slack notifications and the
recorded binding the finished verdict later reads. So the comparison gates all
three, not the explicit one. Repository and number are not enough: with a wrong
PR bound, the record makes it authoritative. When that unrelated PR merges,
`issue end` sees a merged PR, a completed issue and a clean tree, and runs
`git branch -D` on a worktree whose work never landed.

A branch-name match does not close it either, because same-named branches across
forks are the case this design already acknowledges everywhere else. So the
check is on the commit: the selected PR's `headRefOid` must equal the worktree's
`HEAD`, and a mismatch is refused showing both.

**`review finish` is deliberately outside that rule.** It is the *reviewer's*
command, run in a worktree `checkout-pr` built, where `HEAD` goes stale the
moment the author pushes again — an equality gate there would refuse the
ordinary flow, and the command mutates neither the PR nor the record: its whole
effect is a Slack message to the author. So the exemption is a decision with a
reason, not an omission, and the code carries the same reason where the gate
would otherwise sit.

The verdict is not gated on the oid either. FINISHED stays merged PR plus clean
tree plus issue state; what protects it from a stranger's PR is the typed head
lookup, whose `Ambiguous` answer holds the verdict open rather than picking a
winner, together with the recorded binding that `review request` only writes
once the oid agreed.

`review request` pushes the branch before it looks up the PR, so on the ordinary
path the remote is current and the oids agree. Under `--no-push` they can
disagree, and that fails closed: declining to publish the branch is declining to
make it checkable, and binding a PR to unverifiable commits is the thing being
prevented.

`issue end` removes the worktree and its record together, which covers
clearing.

A recorded URL can still go stale in one direction the flow does not reach: a PR
deleted, transferred, or in a repository the token can no longer see. Status
treats a recorded PR that does not resolve as unknown rather than as an error —
the row reports it, and the finished verdict stays closed. It does not silently
fall back to branch matching, because a silent fallback is how a stranger's PR
gets attached in the first place.

**A recorded PR is always authoritative, not only when it is elsewhere.** The
first version of this consulted the record only for a PR outside `repos.prs`,
which left the ordinary case on branch matching. That is not safe: `best_pr`
selects with `p.head_ref_name == head` and nothing more, from a listing capped
at 500, so two forks proposing `fix/crash` both match. Attaching the wrong PR
would let `issue end` judge a worktree finished on a stranger's merge. So
`status` queries a recorded URL exactly, in whatever repository it names, and
falls back to branch discovery only for a worktree whose record carries no PR.
Those keep today's behavior exactly.

### A pasted URL keeps its repository

`checkout-pr` classifies its input in `classify`, and a GitHub PR URL becomes
`Ident::Pr(u64)` — the number survives, the repository is thrown away. With one
resolved repository that loss is invisible, because the only repository the
number could mean is the one being used. With `issues_repo` and `pr_repo`
configured separately it stops being invisible: pasting
`github.com/other/repo/pull/42` resolves `pr_repo#42`, a different pull request
that happens to share a number, and `checkout-pr` builds a worktree from it
without a word.

So a PR identifier carries `{ repo: Option<String>, number: u64 }`. `None` means
the input was a bare number or a `#42`, which still defaults to `pr_repo`; a URL
fills it in and that repository wins. The same locator is what
`issue_pr` returns and what the record stores, so one shape describes a PR
everywhere.

Issue URLs take the other resolution. `IssueRef { id, slug }` is shared with
Linear, and widening it for a field only GitHub populates would push GitHub's
repository question into Linear's type. The GitHub adapter's `issue_ref`
instead refuses a `github.com/…/issues/N` URL whose repository is not
`issues_repo`, naming both repositories in the error. The tracker is scoped to
one repository by construction, so an issue outside it is genuinely unanswerable
rather than merely inconvenient, and refusing says so. A bare number or `#42`
continues to mean `issues_repo`.

**`issue_ref` has to be able to refuse, and today it cannot.** Its signature is
`fn issue_ref(&self, input: &str) -> IssueRef` — no `Result`, so the only way to
report a foreign repository would be to encode it in the returned id and let a
caller guess. `checkout-pr` already works around the absence by inspecting the
result: it treats a `/` in the id as "the tracker failed to parse this". So the
method becomes `Result<IssueRef>`, the refusal is an ordinary error with both
repositories named, and `checkout-pr`'s slash heuristic goes away with it.

**Undeclared projects keep the permissive parse.** `setup` does not call the
trait at all; it calls `slug::parse_issue_ref`, which recognizes a `linear.app`
URL by string alone and needs no API key. Routing that through the tracker
unchanged would regress a real case: a project that declares no tracker, on a
machine with no `LINEAR_API_KEY`, pastes a Linear URL today and gets its id and
title slug out of it. `NoneTracker` would return the raw input and the slug
would vanish. `Resolved.declared` already distinguishes a tracker the project
named from one devkit fell back to, so the fallback path keeps parsing a Linear
URL the way it does now, and only a *declared* tracker owns the answer
completely.

### Authentication

devkit stores no GitHub credential of its own. `gh auth login`, `GH_TOKEN` and
`GITHUB_TOKEN` already cover it, and `github::token()` already reads all three.
So `devkit auth github` reports rather than stores.

**The identity comes from the token, not from `gh`.** `resolve_token` reads
`GH_TOKEN`, then `GITHUB_TOKEN`, and only then falls back to `gh auth token`. So
with either variable set, the active `gh` account is not the identity devkit
uses, and reporting it as such would mislead precisely the user who most needs
the answer. The command resolves the token the way `github::token()` does, asks
GitHub which account it belongs to, and reports that login and which source
supplied the token. A `--token` handed to it is refused rather than accepted and
discarded: a report has nowhere to put a credential, and silently dropping one
would leave the caller believing devkit now holds it.

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

### Most of the trait has no caller

`assigned_history` and `timeline_origin` are not the only dead methods, and the
gap is not a detail. Counting callers of each `Tracker` method from outside
`tracker/`:

| Method | Callers outside the tracker module |
|---|---|
| `states`, `issue_url` | 4 each |
| `check` | 2 |
| `title`, `issue_ref`, `issue_pr` | 1 each |
| `details`, `candidates`, `issues_for_prs`, `assigned_history`, `timeline_origin` | **0** |

Phase 2 moved `status` and `end` onto the seam. It did not move `setup`,
`checkout-pr`'s bare-number arm, `prs`, or the dashboard, and each of those
still calls `linear::` directly. So the trait is a shape the code mostly does
not go through, and implementing `GithubTracker` against it would deliver a
tracker that most commands never ask.

**`issue setup` is the sharpest case, because it is the entry point.**
`resolve_slug` calls `crate::slug::linear_key()?` — a hard error without a
Linear key — then `linear::issue_title`, then `slug::from_linear_title`, and
prints `slug from Linear:`. `fetch_details` does the same for
`linear::issue_details`, which is every fact the summary file holds. A GitHub
project running `issue setup` therefore gets no title, no derived slug, and no
summary file: the first two things this spec's opening paragraph promises. The
`title` and `details` trait methods exist for exactly this and `setup` calls
neither.

**`checkout-pr`'s bare-number arm is the case with a wrong answer rather than no
answer.** It reads the ambient `LINEAR_API_KEY` directly: absent, a number is
taken as a GitHub PR without further thought; present, it calls
`linear::issues_by_number` and can resolve the number to a Linear issue, or
refuse it as ambiguous, on a project whose configured tracker is GitHub or none.
The exported key of one project decides what a number means in another, and
declaring `kind = "github"` does not stop it, because the code never asks which
tracker was selected. `LinearTracker::candidates` already wraps
`issues_by_number`, so routing through the trait leaves Linear's behavior
identical where Linear is selected and correctly absent where it is not;
`GithubTracker::candidates` returns empty, which is what makes a bare number a
PR on a GitHub project — by the tracker's answer rather than by a missing
environment variable.

**PR triage has both remaining dead methods.** `prs.rs` reaches for
`linear::workspace_url_key()` to build its link base, which `issue_url` covers.
More consequentially, `devkit-issue::prs::gather` calls
`linear::issues_for_prs` directly, gated by `[linear] resolve_pr_links`, so a
GitHub PR row would never receive the issues it closes — the column simply stays
empty, and `issues_for_prs` stays dead in contradiction of this task's own gate.
So the resolved tracker is injected into PR gathering and the trait method is
called. `resolve_pr_links` keeps its meaning as *Linear's* opt-in, because the
Linear implementation is the expensive one it was added to gate; GitHub's is a
field on a query already being made. Both the CLI and MCP paths into `gather`
change together.

`summary.rs` and `setup.rs` also pass `linear::IssueDetails` around as a type;
that one is a naming problem rather than a behavioral one, and renaming it is
optional, not part of the gate.

So wiring the remaining commands to the trait is its own delivery task, sized
against that table rather than against a guess. Its gate is that every method
has a non-test caller, and that a project with no `LINEAR_API_KEY` in the
environment gets identical behavior from a Linear-configured devkit — which is
also the regression test proving the trait carries what the direct calls did.

### The dashboard is not wired to the trait yet

The dashboard is the fourth of the unwired commands, and the one whose two trait
methods exist for nothing else. `dashboard/data.rs` returns an empty list when
`LINEAR_API_KEY` does not resolve and otherwise calls
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

**Scoping the key puts configuration into a filename, so both ends need
guarding.** `cache::path_for` is
`cache_dir().join("dashboard").join(format!("{key}.json"))`, a raw
interpolation. A repository slug is one of the components the key is about to
gain, and `issues_repo` comes from `devkit.toml`, which travels with a checkout.
A value carrying path separators or `..` would let a dashboard write land
outside the cache directory — from cloning a repository and running a read-only
command.

Two guards, because each is right on its own. Configured repositories are
validated as `owner/repo` when they are resolved, since a slug that is not one
is a configuration error worth reporting wherever it came from. And every
cache-scope component is encoded before it reaches a filename, so `path_for`
cannot be made to escape regardless of what a future key includes.

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

Twelve tasks, up from nine, because the fourth review round found that the trait
most of this spec builds on is not what the commands actually call. Tasks 1
through 4 are groundwork on the existing Linear paths and change no GitHub
behavior; each is a regression gate in its own right, which is why they are
separate. Task 5 is the adapter. Tasks 6 through 8 build the lifecycle the
adapter needs. Task 9 is the switch that makes GitHub live, which is why it
comes after all of them rather than after the adapter.

1. **The repository resolution seam.** `[github] issues_repo` and `pr_repo`
   config **with the schema regenerated in this same task**, the `Repos`
   resolution — from config first, origin only for a key config left unset,
   each key resolving independently and required only where it is used, each
   retaining whether it was configured, overridden or defaulted, each validated
   as an `owner/repo` slug where it is resolved, and the origin fallback
   requiring a `github.com` host — every GitHub operation taking it instead of
   resolving its own, and `--repo github.com/owner/repo` on every `gh pr`
   command including `gh pr edit`, with no exemption for an origin-defaulted
   value, plus `--hostname github.com` on `gh api` and `gh auth token`. No
   change to what any lookup answers. **The gate is that a project setting
   neither key reaches the same repository it does today**, Linear projects
   included, since this task alone touches every PR path devkit has. That is
   "same repository", not "same argument vector": the one intended difference is
   that an ambient `GH_REPO` or `GH_HOST` no longer redirects anything, which is
   the point of the task and is asserted rather than waived.

   Schema regeneration cannot wait for the documentation task:
   `tests/config_schema.rs` compares the committed schema against the generated
   one and fails on any drift, so a task that adds config keys without it is red
   the moment it lands. Every task that changes a config type regenerates in
   place.
2. **The typed PR lookup.** `pr_by_head` moves to GraphQL
   `pullRequests(headRefName:)` with `totalCount`, both transports return
   `Unique` / `NoMatch` / `Ambiguous` / `Unavailable`, and the acting callers
   are rewritten to branch on it: `branch_pr_number`, `existing_pr` and
   `fetch_pr_full` stop collapsing the answer with `.ok()`, the `gh` fallback
   fires only on `Unavailable` and drops `--limit 1`, and `review request` and
   `review finish` refuse an ambiguous answer. The gate is that an unambiguous
   single-repository project — every project today — resolves the same PR it
   always did.
3. **Status's PR resolution and the tagged row.** `fetch_prs` stops pulling the
   repository's PRs with `--limit 500` and resolves each distinct worktree
   branch through the typed lookup, batched by GraphQL alias. `IssueWorktree`
   carries the tagged status, with `pr_state`, `pr_number` and `pr_url` derived
   from it so the serialized shape survives; the finished verdict reads the tag;
   `triage.rs` renders the ambiguous case from the tag rather than formatting
   `#0`; the MCP `issue.status` action gains the candidate list. **`issue info`
   is part of this task**: it calls `fetch_prs` on its own path and
   `apply_cached_pr` writes `pr_number`/`pr_state`/`pr_url` directly, so
   `info.rs` and `info_cache.rs` construct the tag instead, and a cached unique
   PR is discarded rather than replayed when the live lookup is no longer
   unique. Left out, this task either stops compiling or renders a PR beside a
   verdict reading a contradictory tag.
4. **Wire the remaining commands to the trait.** `Tracker::issue_ref` becomes
   `Result<IssueRef>` so it can refuse, and `checkout-pr`'s slash heuristic goes
   away with it. `setup` parses through the trait and takes its title and
   summary facts from `title` and `details` instead of `slug::parse_issue_ref`,
   `linear::issue_title` and `linear::issue_details`, stopping its hard Linear-key
   requirement — while an **undeclared** tracker keeps today's permissive
   `linear.app` URL parse, so a project with no tracker and no key still gets a
   slug from a pasted URL. `checkout-pr`'s bare-number arm resolves candidates
   through `candidates` instead of reading `LINEAR_API_KEY`. `prs.rs` takes its
   link base from `issue_url`, and `devkit-issue::prs::gather` takes the
   resolved tracker and calls `issues_for_prs` instead of `linear::` directly,
   on both the CLI and MCP paths, with `[linear] resolve_pr_links` kept as
   Linear's own opt-in gate. The gate is that every trait method except the two
   the dashboard uses has a non-test caller, and that a Linear-configured
   project behaves identically through the new path.
5. **The adapter.** `github.rs` with the query, parse and wrapper split, the
   `Tracker` implementation including the linked-PR ranking rule, and its
   fixture tests. No wiring yet.
6. **Identifier repositories.** A PR identifier carries `{ repo, number }`, so
   `classify` keeps the repository from a pasted PR URL and only a bare number
   or `#42` defaults to `pr_repo`; the GitHub adapter's `issue_ref` refuses an
   issue URL outside `issues_repo`, naming both.
7. **The recorded PR binding.** `IssueRecord` gains the optional PR locator;
   `checkout-pr` and `review request` both write it whenever they resolve a PR.
   Precedence becomes explicit locator, then record, then branch discovery, so
   `review finish --pr` keeps winning as it does today and stays a one-run
   override that writes nothing; `review request` gains the URL form, and its
   existing write rule is what makes it a rebind. On `review request` a PR's
   `headRefOid` must equal the worktree's `HEAD` or it is refused, however the
   PR was chosen — explicit, recorded or branch-discovered alike. **Where the
   comparison happens depends on whether the PR already exists.** Adding
   reviewers to an existing PR is gated before the call. A PR being created has
   no head to compare until it exists, so it is fetched back and validated
   immediately after the call and before anything downstream — before the record
   is written and before any notification — with the error saying the PR is open
   with nothing recorded and nobody notified. `review finish` is exempt for the
   reason given above, and the finished verdict rests on the merged PR, the
   clean tree and the issue state rather than on this comparison. A recorded
   PR that no longer resolves reports unknown rather than falling back.
8. **Wire the dashboard to the trait.** `dashboard/data.rs` resolves the
   configured tracker and calls `assigned_history` and `timeline_origin` instead
   of Linear directly. Linear's behavior through the new path is the regression
   gate. **Every dashboard cache key gets scoped**, not just the issue one:
   `cache::path_for` is `cache_dir()/dashboard/{key}.json` with no project
   component, so `issues`, `pr-timeline-mine` and `pr-timeline-all` are already
   shared by every project on the machine. The key gains the tracker, the
   repository and the viewer identity, and every component is encoded before it
   reaches a filename so a configured slug cannot escape the cache directory.
9. **Selection and detection.** `TrackerKind::Github` constructs `GithubTracker`
   instead of the stand-in, with `declared` and the reason line;
   `tracker::resolve` regains `repo` and is handed the resolved `repos.issues`;
   `ready` rests on the token alone, that construction site having already
   settled the repository, rather than on `repo_slug(cwd)`; detection validates
   the origin host is `github.com`. A `github` kind whose issues repository does
   not resolve falls back to no tracker, undeclared, with the failure kept in
   `reason` so the caller can say what stopped it. This
   lands after tasks 6 through 8 on purpose: it is the switch that makes GitHub
   live, and flipping it while the recorded-PR lifecycle or the dashboard is
   still half-wired would ship a tracker that reports confidently wrong
   verdicts.
10. **`devkit auth github`** and the doctor hint, with the identity taken from
    the resolved token rather than from the active `gh` account.
11. **Dogfood.** devkit's own `devkit.toml` declares `[tracker] kind =
    "github"`. Worth noting: this repository currently has no GitHub issues
    filed, so the declaration exercises the empty-tracker path and little else.
    It becomes real exercise once devkit work is filed as issues. Land it last,
    or defer it.
12. **Documentation.** `docs/configuration.md` gains the `[tracker] kind =
    "github"` row, the `[github]` table, and the auth instruction; `README.md`
    gains `devkit auth github`; `AGENTS.md`'s tracker paragraph drops "there is
    no GitHub implementation" and describes the real arm; `skills/using-devkit/`'s
    CLI reference gains the new subcommand and `--pr`. Schema regenerates via
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
  and an open together, two merged **in one repository** — ranked by number, not
  refused — two merged **across repositories**, which is refused because their
  numbers are not comparable, and none.
- **The recorded PR.** A record carrying a cross-repository PR URL drives status
  to the finished verdict; a record without one keeps today's behavior; an old
  record with no field at all still deserializes; `review request` writes the
  URL on a worktree whose record had none, and overwrites it when it resolves a
  different PR; and a recorded URL that no longer resolves reports unknown
  rather than falling back to branch matching.
- **Cache scoping.** Two projects on different trackers, and two viewers on one
  project, each get their own `issues` and `pr-timeline-*` entries rather than
  reading each other's.
- **Host validation.** Detection returns no tracker for a GitLab or Bitbucket
  origin, and GitHub for a `github.com` origin in each remote-URL shape
  `slug_from_remote_url` accepts.
- **Repository resolution.** Neither key set resolves both repositories to
  origin; each key set independently; both set; both set with no GitHub origin
  at all, which must succeed and must report `ready`; one key set with no origin
  to supply the other, which succeeds for every operation using the key that
  resolved and fails **only at an operation needing the missing one**, naming
  it; and `issue prs --repo` still overriding `pr_repo` for one invocation. The identical-behavior
  gate for an unconfigured project is asserted at this layer rather than left to
  manual checking.
- **The typed PR lookup.** A `headRefName` response with one node parses to
  `Unique`, with `totalCount: 0` to `NoMatch`, with two nodes to `Ambiguous`,
  and a transport error to `Unavailable`. Above that, a caller test asserts the
  branching that made this a type: `NoMatch` does not reach the `gh` fallback,
  `Unavailable` does, `review request` and `review finish` refuse on
  `Ambiguous`, and `status` reports `AMBIGUOUS` with the finished verdict
  closed. The fork case is covered by a fixture whose single node carries a
  `headRepositoryOwner` other than the searched repository's owner.
- **Connection completeness.** A `closedByPullRequestsReferences` response
  carrying `hasNextPage: true` is refused rather than ranked, so a winner
  outside the window can never be silently dropped. A truncated
  `closingIssuesReferences` reports incomplete rather than a partial link list,
  and a truncated `timelineItems` nested inside a paginated `issues` page is
  paginated through rather than cut — the nested case walking its own pages
  while the outer connection stays where it is.
- **Recorded PR precedence.** A record whose PR sits inside `pr_repo` is still
  queried by URL rather than matched by branch, proven by a fixture where a
  second PR shares the branch name and would otherwise win.
- **Identifier repository.** `classify` on `github.com/other/repo/pull/42`
  yields that repository rather than `pr_repo`; on `#42` and on a bare number it
  yields none, which resolves to `pr_repo`. `issue_ref` accepts an issue URL in
  `issues_repo`, refuses one outside it naming both repositories, and accepts a
  bare number.
- **Status's PR resolution.** A repository whose PR count exceeds any single
  window still resolves each worktree branch correctly, which the `--limit 500`
  listing could not guarantee; two PRs on one branch make that row ambiguous
  rather than picking a winner; and the alias batching issues one round trip for
  many branches.
- **The tagged row.** `triage.rs` renders the ambiguous case without emitting a
  PR number, so the `AMBIGUOUS #0` shape cannot recur; `pr_state`, `pr_number`
  and `pr_url` derive from the tag with the same values they carry today for
  `Unique` and `NoMatch`; the MCP `issue.status` payload round-trips the
  candidates; and the finished verdict reads the tag rather than a string.
- **The trait wiring.** Every `Tracker` method has a non-test caller. `issue
  setup` on a Linear project produces the identical slug and summary through
  `title` and `details`; on a GitHub project it produces them at all. A bare
  number on a project whose tracker is GitHub or none resolves as a PR **with
  `LINEAR_API_KEY` exported in the environment**, which is the regression the
  ambient read caused.
- **Repository scoping on `gh`.** Every `gh pr` invocation carries `--repo`,
  asserted on the argument vector, including when the value came from origin and
  when it equals origin, so an ambient `GH_REPO` cannot redirect it. `gh auth`
  and `gh api graphql` carry no `--repo`, and the graphql path names its
  repository in the query variables instead.
- **`--pr` and precedence.** `issue review request --pr <URL>` replaces an
  existing binding and sets one where none existed; a bare number binds within
  `pr_repo`; the supersede case — an old and a new PR sharing a head branch,
  which acting paths refuse as ambiguous — is recoverable through it, which is
  the case that made the flag necessary. `review finish --pr <n>` still wins
  over both the record and branch discovery and leaves the record unchanged,
  which is today's contract. On `review request` a PR whose `headRefOid` is not
  the worktree's `HEAD` is refused whether it arrived by `--pr`, from the
  record, or from a unique branch lookup — before the call when the PR already
  exists, and immediately after it, before the record is written and before any
  notification, when it was just created — while a squash- or rebase-merged PR
  still compares equal because `headRefOid` is the branch head the PR carried,
  not the commit that landed on the base; under `--no-push`, a branch ahead of
  its remote fails closed. `review finish` runs no such comparison; the
  exemption and its reason are recorded where the gate would otherwise sit.
- **`issue_ref` refusing.** A GitHub issue URL outside `issues_repo` returns an
  error naming both repositories rather than a mangled id, and `checkout-pr`
  classifies it as unrecognized without consulting a `/` in the result. A
  project with **no declared tracker and no `LINEAR_API_KEY`** still gets id and
  slug from a pasted `linear.app` URL, which is the regression the trait routing
  would otherwise cause.
- **PR triage through the trait.** A GitHub PR row carries the issues it closes,
  from `issues_for_prs`; a Linear project's rows are unchanged and still gated
  by `[linear] resolve_pr_links`; and the MCP path resolves the same way as the
  CLI.
- **Independent keys.** A Linear project setting only `pr_repo`, with no GitHub
  origin, resolves and works without being made to configure an `issues_repo` it
  never uses; a GitHub operation needing the key that is missing errors naming
  that key.
- **Repository and host on `gh`.** Every `gh pr` invocation carries `--repo
  github.com/owner/repo`, asserted on the argument vector, including when the
  value came from origin — so neither `GH_REPO` nor `GH_HOST` can make the `gh`
  half act on a different repository or host from the one the HTTP half read.
  `gh api` and `gh auth token` carry `--hostname github.com` and no `--repo`.
- **Origin host.** A GitLab or Bitbucket origin fails the origin fallback rather
  than defaulting a repository, on a *declared* `github` tracker as well as
  during detection; explicitly configured repositories work with no origin at
  all.
- **`issue info`.** Its live path and its cached path both produce the tagged
  status, and a cached unique PR is discarded rather than replayed when the live
  lookup is ambiguous or unavailable.
- **Cache path safety.** An `issues_repo` carrying path separators or `..` is
  rejected when the repositories are resolved, and — independently — a
  cache-scope component containing them still produces a path inside the
  dashboard cache directory.
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
| An issue a PR closes in another repository | Carried as `owner/name#number`, a bare number meaning the tracker's own issues repository; the comparison that decides which form to emit is case-insensitive, and `issue_url` follows a qualified id to the repository it names. Linear ids are `ENG-1`, so the two vocabularies cannot be confused for one another. |
| Repository configuration | `[github] issues_repo` and `pr_repo`, separately configurable, both defaulting to origin. |
| Where `[github]` sits | Top level, not under `[tracker]`. `pr_repo` serves Linear projects on a fork too. |
| The PR head owner | Neither configured nor derived. `pr_by_head` searches GraphQL `headRefName` by branch name alone. |
| REST or GraphQL for the head lookup | GraphQL. REST documents `head` only as `user:ref-name`, so the unqualified form a probe observed is undocumented behavior. |
| What a PR lookup returns | `Unique` / `NoMatch` / `Ambiguous` / `Unavailable` from both transports. Only `Unavailable` falls back; acting paths refuse `Ambiguous`, read-only paths report it and keep the verdict closed. |
| Who writes the recorded PR | `checkout-pr` and `review request` both, whenever either resolves one. |
| How a binding is corrected | `issue review request --pr <URL\|number>`. The implicit write is not enough: an old and a new PR share a head branch, so the lookup is `Ambiguous` and the acting path that would fix the binding refuses. |
| Whether the record binds acting paths too | Yes. A record authoritative only for `status` leaves `review request` and `review finish` on branch discovery, which is what the record replaces. |
| Ranking ambiguous head candidates by state | No. Ranking across head owners is how a stranger's merged PR wins. The rule stays strict and `--pr` is the escape. |
| How status finds each worktree's PR | Per-branch typed lookups batched by GraphQL alias, not a `--limit 500` repository listing. Ambiguity computed from a truncated window is not ambiguity detection. |
| How the row carries an ambiguous PR | A tagged status on `IssueWorktree`, with `pr_state`/`pr_number`/`pr_url` derived from it. A `"AMBIGUOUS"` string renders as `AMBIGUOUS #0` and gives MCP nothing structured. |
| `--repo` when the configured value equals origin | Passed anyway. `gh` reads an ambient `GH_REPO`, so omitting it lets an environment variable override a setting the project made. `Repos` keeps configured/overridden/defaulted provenance to tell the cases apart. |
| The bare-number arm's issue candidates | Through `Tracker::candidates`, never the ambient `LINEAR_API_KEY`. |
| `issue_ref`'s signature | `Result<IssueRef>`. It cannot refuse a foreign repository otherwise, and `checkout-pr`'s slash heuristic exists because it cannot. |
| Parsing for an undeclared tracker | Keeps today's permissive `linear.app` parse. Routing it through `NoneTracker` would lose the slug for a project with no tracker and no key. |
| `[linear] resolve_pr_links` | Stays, as Linear's own opt-in gate. GitHub's linked issues are a field on a query already being made. |
| `--pr`'s meaning | One meaning everywhere: use this PR for this run. Rebinding falls out of `review request` recording what it acted on, so `review finish --pr` keeps its one-run contract. |
| Locator precedence | Explicit `--pr`, then the record, then branch discovery. |
| Validating a PR on an acting path | Its `headRefOid` must equal the worktree's `HEAD` — explicit, recorded or branch-discovered alike. A branch-name match does not prove the PR carries these commits, and same-named branches across forks are the case this design assumes everywhere else. |
| Which command that gates | `review request`, which edits the PR and writes the binding. `review finish` is exempt: it is the reviewer's command, run where `HEAD` goes stale by design as the author pushes, and it mutates neither the PR nor the record. The finished verdict is not gated on the oid either — it rests on the merged PR, the clean tree and the issue state, with an `Ambiguous` head lookup holding it open. |
| When that check runs | Before the call for an existing PR; immediately after, and before the record or any notification, for one just created. A PR that does not exist yet has no head to compare. |
| `--no-push` under that rule | Fails closed. Declining to publish the branch is declining to make it checkable. |
| `--repo` on `gh` | On every `gh pr` command, with no origin-defaulted exemption, spelled `github.com/owner/repo`. `gh api` and `gh auth token` take `--hostname github.com` instead. `GH_REPO` must not split the `gh` half from the HTTP half, and `GH_HOST` must not send a token to a host it was not issued for. |
| Whether both repository keys must resolve | No. Each resolves independently and is required only where used, so a Linear project with a fork workflow sets `pr_repo` alone. |
| Where the `github.com` host check applies | Every origin fallback, not detection alone. A declared `github` tracker skips detection, and `repo_slug` is host-blind. |
| Configured repositories in a cache filename | Validated as `owner/repo` where resolved, and every cache-scope component encoded before it reaches a path. |
| The commands phase 2 left on direct `linear::` calls | Wired in this phase. `setup`, `checkout-pr`'s fuzzy arm and `prs` were not moved onto the seam, and five trait methods had no caller at all. |
| A recorded PR that no longer resolves | Reported as unknown with the verdict closed. Never a silent fall back to branch matching. |
| A pasted PR URL's repository | Kept. A PR identifier is `{ repo, number }`; only a bare number or `#42` defaults to `pr_repo`. |
| An issue URL outside `issues_repo` | Refused, naming both. `IssueRef` stays as it is rather than carrying a repository Linear never fills. |
| A recorded PR URL | Always authoritative, in any repository. Branch discovery serves only records without one. |
| Truncated linked-PR results | Refused via `hasNextPage`, never ranked. |
| Every other connection | Same rule. `pageInfo` on all of them; `closingIssuesReferences` reports incomplete, `timelineItems` paginates. A connection nested in a paginated one does not paginate with its parent. |
| The linked-PR ranking tuple | State, then number within the top state group. A tie is a top state group spanning repositories, where numbers are not comparable — two merged PRs in one repository are ranked, not refused. |
| Which PRs the OID check gates | All of them on `review request`: explicit, recorded, and branch-discovered. How the PR was chosen does not change what it can do. |
| `--repo`'s actual scope | Repository-scoped `gh pr` commands. `gh auth` and `gh api graphql` do not accept it; graphql names its repository in variables. The rule is that the environment never chooses the repository. |
| Schema regeneration | In the task that changes the config type, not deferred to the documentation task. |
| When GitHub goes live | After the recorded-PR and dashboard tasks, so the switch never exposes a half-wired tracker. |
| `tracker::resolve`'s signature | Regains `repo`, handed `repos.issues`. |
| Host validation in detection | Added. A non-`github.com` origin no longer detects as GitHub. |
| What `ready` means | A resolved token, not `repo_slug(cwd)`. The issues repository is checked once at the single construction site, so `ready` has nothing left to check; a second construction site that skipped it would have `ready` claim readiness with no repository to ask. A project that names its repositories needs no GitHub origin. |
| The dashboard | Wired to the trait in this phase, not assumed. |
| A `devkit auth github` credential store | No. It reports the resolved token's identity and lists `gh` accounts as diagnostics. |
| Conventional-title parsing | Out of scope. Its own spec. |
| Fixture content | Synthesized in the shape of the originals, never copied. |
| devkit's own tracker | Declares `github`, landed last, with the empty-repository caveat stated. |
