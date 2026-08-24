# GitHub tracker spec: review log

Adversarial cross-model review of
`2026-08-24-github-tracker-design.md`. Critic: OpenAI Codex
(`gpt-5.6-sol`, reasoning effort `xhigh`), read-only, one round, scoped to
major issues only. Claude is the final arbiter on every point.

## Round 1: Codex

Verdict: REVISE. Five material gaps.

1. **`first: 1` picks an arbitrary linked PR.** No ordering or ambiguity policy,
   so an abandoned PR can win over its replacement or over the merged one.
   Fix: paginate the linked PRs and have `checkout-pr` select explicitly or fail
   when several candidates remain.

2. **Cross-repository checkout breaks the lifecycle after checkout.**
   `IssueRecord` stores no PR URL or repository, while `status` lists PRs from
   `origin` only, so a foreign PR reads as `NO_PR` and `issue end` never reaches
   finished. Fix: persist the authoritative PR URL in `IssueRecord` and have
   status query that exact repository and PR.

3. **Origin-only repository resolution is wrong for standard fork clones and
   central issue repositories.** `slug_from_remote_url` also does not validate
   the host, so a GitLab origin triggers GitHub detection. Fix: restore
   `[github] repo`, default it only from a validated GitHub origin, and pass the
   resolved repository into `GithubTracker`.

4. **The promised GitHub dashboard is not wired.** Dashboard history still
   requires `LINEAR_API_KEY` and calls Linear directly, and caches every project
   under the global `"issues"` key. Fix: add a delivery task that resolves the
   configured tracker, calls `assigned_history` and `timeline_origin`, and
   scopes cache keys by tracker identity and repository.

5. **`devkit auth github` can report the wrong identity.** The spec says the
   active `gh` account is the identity devkit uses, but token resolution prefers
   `GH_TOKEN` and `GITHUB_TOKEN` over `gh auth token`. Fix: resolve the real
   token source first, query `viewer.login` with that token, and list `gh`
   accounts as secondary diagnostics.

### Claude's response

Every finding was checked against the source before being accepted. All five
hold. Two of them (2 and 4) are the kind this review exists to catch.

**Accepted as written: 2, 4, 5.**

- **2** is confirmed at `crates/devkit-issue/src/status.rs`: `fetch_prs_http`
  resolves `github::repo_slug(cwd)` and lists that repository's PRs, so a PR in
  another repository is invisible and `pr_state` stays `NO_PR`, which the
  finished verdict reports as "no PR" forever. Returning a cross-repository PR
  without persisting it was a hole. `IssueRecord` gains the PR URL, and status
  consults it.
- **4** is confirmed and is the largest gap: `assigned_history` and
  `timeline_origin` have no callers anywhere outside the tracker module. They
  are dead trait methods. `dashboard/data.rs` returns early without a
  `LINEAR_API_KEY` and calls `linear::assigned_issue_history_with_progress`
  directly, so the dashboard would ignore a GitHub tracker entirely. The spec
  promised a timeline it could not deliver. Wiring the dashboard to the trait
  becomes its own task, and the global `"issues"` cache key gets scoped, since
  two projects on different trackers would otherwise clobber each other.
- **5** is confirmed at `github.rs`: `resolve_token` reads `GH_TOKEN` then
  `GITHUB_TOKEN` before falling back to `gh auth token`, so with either variable
  set the active `gh` account is not the identity devkit uses. The command now
  reports the resolved token's own `viewer.login` as the identity and lists `gh`
  accounts beneath it.

**Accepted with a narrower fix: 1.** The risk is real and the ordering is
genuinely undefined, but neither probed repository has an issue with more than
one linked PR, and the field takes an `orderByState` argument, which the review
did not account for. Full pagination for a connection that is almost always
empty or single is more than the job needs. The query becomes `first: 10,
orderByState: true`, preferring merged, then open, then the highest number, and
`checkout-pr` refuses rather than guessing when several remain viable.

**Accepted in two halves: 3.** The host bug is real and pre-existing:
`slug_from_remote_url` parses `https://gitlab.com/o/r` into `o/r` with no host
check, and `detect()` treats a successful `repo_slug` as proof of GitHub. That
gets a host validation in detection. The `[github] repo` half reverses a
decision the spec made deliberately, so it is carried as a recommendation for
the author rather than settled here. The counter-case is sound though: the
observed fork keeps its issues on the fork, but the ordinary fork workflow keeps
them upstream, and origin-only resolution cannot serve both.

No second round was run. This revision is unreviewed.

## Round 2: Codex

Verdict: REVISE. Five more, all against the parts round 1 did not see.

1. **`head_owner` cannot be project-global.** `repo_slug` reads the origin fetch
   URL, but git supports a separate push URL, `remote.pushDefault` and per-branch
   push remotes, and a checked-out contributor PR has a different head owner
   again. Worse, the callers treat a successful-but-empty HTTP lookup as
   authoritative instead of falling through to `gh`. Fix: resolve the head
   repository per branch, or use an unqualified lookup with ambiguity refusal.

2. **A recorded PR is authoritative even inside `pr_repo`.** The spec consults
   the recorded URL only when the PR is outside `pr_repo`; otherwise status
   still matches on `head_ref_name` alone within a capped listing. Two forks
   sharing a branch name let status attach the wrong PR, and `issue end` could
   then remove a worktree early. Fix: always query a recorded PR URL exactly,
   and use branch discovery only for records without one.

3. **`first: 10` leaves the ranking incomplete.** GitHub documents
   `orderByState` only as "return results ordered by state", with no direction,
   and the spec ignores `hasNextPage`. An eleventh candidate can change the
   winner or turn a unique answer into a tie. Fix: request `pageInfo` and refuse
   on `hasNextPage`, or paginate before ranking.

4. **The call-site inventory has the wrong shape.** `gh pr edit` in
   `review/request.rs` infers its repository from the working directory and is
   absent from the spec's list. The dashboard's `pr-timeline-*` caches are also
   global, so different repositories or viewers can be served each other's data.
   Fix: audit every GitHub operation and every cache, not the `repo_slug`
   callers.

5. **The delivery order cannot land green.** Task 1 adds config fields while
   schema regeneration waits until task 8, and the schema drift test fails
   immediately. Task 3 also activates the adapter before the recorded-PR
   lifecycle and the dashboard wiring exist. Fix: regenerate the schema in task
   1 and move selection after those tasks.

### Claude's response

All five verified against the source and accepted. Three were confirmed by
reading the code the review cited, and one turned out worse than reported.

- **1** holds, and the mechanism is worth stating exactly: `branch_pr_number`
  and its siblings are written as `if let Some(found) = …and_then(|slug|
  pr_by_head(&slug, b).ok())`, so an HTTP call that succeeds and finds nothing
  yields `Some(None)`, matches the `if let`, and returns `Ok(None)` without ever
  trying `gh`. A wrong head owner therefore reports "no PR" authoritatively and
  silently. The fix goes further than the review proposed: `head_owner` is
  removed from the design entirely. An unqualified `head={branch}` lookup was
  already measured to work against `mathix420/alacritree`, so the owner never
  needs deriving, and the ambiguity it introduces is handled by refusing rather
  than ranking.
- **2** holds. `best_pr` filters on `p.head_ref_name == head` and nothing else,
  so branch-name collision across forks is real. Always consulting a recorded PR
  URL is both simpler than the conditional rule and safer, and it subsumes half
  of finding 1: a contributor's PR is identified by its recorded URL rather than
  by guessing whose fork the branch is on.
- **3** holds. The ranking was always client-side, so `orderByState` was never
  load-bearing; completeness was the real gap. The query requests `pageInfo` and
  the adapter refuses on `hasNextPage` rather than ranking a truncated set.
- **4** holds, and the cache half is worse than reported. `cache::path_for` is
  `cache_dir()/dashboard/{key}.json` with no project component at all, so
  `issues`, `pr-timeline-mine` and `pr-timeline-all` are already shared across
  every project on the machine. The task is rewritten as an audit of every
  GitHub operation and every cache key, since "the nine `repo_slug` callers" was
  an inventory of the wrong thing and `gh pr edit` proves it.
- **5** holds on both halves. `tests/config_schema.rs` compares the committed
  schema against the generated one and fails on any drift, so a task that adds
  config keys without regenerating cannot be green on its own, which the
  project's per-task merge gate requires. Selection moves after the record and
  dashboard tasks so the adapter is never live while half-wired.

Two rounds have now run and neither returned APPROVED. Every finding across both
was accepted; none was rejected. This revision is unreviewed.

## Round 3: Codex

`VERDICT: REVISE`. Tasks 2 through 4 have no remaining ordering blocker, but
task 1 is not behaviorally safe and task 5 still has a readiness bug.

1. **The unqualified REST lookup relies on undocumented behavior.** GitHub's
   REST contract requires `head=user:ref-name`; `gh pr list --head` works
   because `gh` uses GraphQL `pullRequests(headRefName:)`, not that REST
   behavior. A live probe does not make it stable. *Fix:* implement direct
   lookup with GraphQL `pullRequests(headRefName:)`, matching the documented
   mechanism `gh` uses.
2. **Ambiguity refusal is not end to end.** `review request` and `review finish`
   discard `pr_by_head` errors with `.ok()` and fall back to `gh pr list --limit
   1` (`request.rs:107`, `finish.rs:43`); `status` independently ranks
   same-named branches in `best_pr` (`status.rs:29`). Acting paths can still
   choose arbitrarily, and `issue end` can treat a stranger's merged PR as
   grounds for deletion. *Fix:* return a typed unique/none/ambiguous/unavailable
   result from both transports; acting paths propagate ambiguity, status records
   a per-row ambiguous/unknown PR and keeps the finished gate closed.
3. **Only `checkout-pr` writes the authoritative URL.** The normal `issue setup`
   → `issue review request` flow reads the record but never updates it when it
   finds or creates the exact PR (`request.rs:207`); replacement PRs likewise
   leave an immutable stale binding. *Fix:* make the binding updateable, persist
   it whenever review request finds or creates a PR, and provide a rebind/clear
   path for superseded PRs.
4. **Repository-qualified URLs collapse to bare numbers.** `checkout-pr`
   discards the repository from a PR URL (`checkout.rs:36`), and `IssueRef` has
   nowhere to retain the repository from an issue URL. With separate configured
   repositories, pasting `other/repo/pull/42` or `other/repo/issues/42` resolves
   `pr_repo#42` or `issues_repo#42`. *Fix:* preserve `{repo, number}` for PR
   URLs, and either preserve issue repository identity or reject issue URLs that
   do not match `issues_repo`; only bare numbers default to the configured
   repositories.
5. **`ready()` still depends on `repo_slug(cwd)`** even though `GithubTracker`
   is constructed with `repos.issues`. A project with both keys configured and
   no GitHub origin has everything required but remains unready, leaving every
   state gate closed. *Fix:* resolve defaults before constructing the tracker,
   require origin only for missing config values, and define readiness as a
   token plus the validated configured issue repository.

### Claude's response

All five verified against source before acceptance, and all five accepted.
Findings 2 through 5 were confirmed by reading the cited lines; finding 1 was
confirmed by probing the proposed replacement rather than the claim.

- **Finding 1 accepted, and the fix probed.** REST documents `head` only as
  `user:ref-name`, so the unqualified form the earlier probe observed is
  undocumented. `pullRequests(headRefName: "fix/glyph-overhang-clipped")`
  against `mathix420/alacritree` returns PR 185 with `headRepositoryOwner:
  AbysmalBiscuit` and `totalCount: 1` — documented argument, cross-fork match,
  and ambiguity detectable from `totalCount` rather than inferred from a node
  count that a page limit could have truncated. Strictly better than what it
  replaces.
- **Finding 2 accepted, and it is the sharpest of the five.** Refusing inside
  `pr_by_head` would have changed nothing: `.ok()` swallows the refusal and the
  `gh` fallback guesses anyway. The same `.ok()` also makes `Some(None)` return
  "no PR" without consulting `gh`. Both failures come from an `Option` that
  cannot say which of four things happened, so the fix is a four-way type
  returned by both transports, with only `Unavailable` reaching the fallback.
  `best_pr` moves onto it too. The spec gained a section for this.
- **Finding 3 accepted; the rebind half narrowed.** `request.rs:207` and
  `finish.rs:124` only `record::read` — nothing outside `checkout.rs`,
  `setup.rs` and `end.rs` writes. So the ordinary `setup` → `review request`
  flow would have left the record empty and fallen back to exactly the branch
  matching the record exists to replace. `review request` now persists the URL
  whenever it resolves a PR. A separate rebind or clear command was **not**
  added: that write is itself the rebind, since the next `review request` on a
  replacement PR overwrites, and `issue end` removes the record with the
  worktree. What the flow does not reach is a recorded PR that stops resolving,
  so status reports that as unknown with the verdict closed rather than falling
  back.
- **Finding 4 accepted; the two halves resolved differently.** A PR identifier
  becomes `{ repo: Option<String>, number }`, so a pasted URL keeps its
  repository and only a bare number defaults to `pr_repo`. Issue URLs take the
  other option Codex offered: `IssueRef` is shared with Linear, and widening it
  for a field only GitHub fills would push GitHub's repository question into
  Linear's type, so `issue_ref` refuses an issue URL outside `issues_repo`
  instead. The tracker is scoped to one repository by construction, so an issue
  outside it is unanswerable rather than merely inconvenient.
- **Finding 5 accepted.** Line 165 of the spec contradicted line 148 outright.
  `Repos` now resolves before the tracker is constructed, taking each key from
  config and consulting origin only for a key config left unset, and `ready` is
  a resolved token plus a resolved `repos.issues`.

Delivery grew from eight tasks to nine. The typed lookup was split out of task 1
rather than bundled into it: task 1 moves resolution and changes nothing about
what any lookup answers, task 2 changes the contract every PR path reads. Codex
called task 1 not behaviorally safe, and a task that did both at once would have
had no clean gate when it went wrong.

Three rounds have run and none returned APPROVED. Fifteen findings across the
three; every one accepted, two of them narrowed in scope with the reason
recorded above. This revision is unreviewed.

## Round 4: Codex

`VERDICT: REVISE`.

1. **`checkout-pr`'s fuzzy-number path still bypasses the selected tracker.** It
   checks the ambient `LINEAR_API_KEY` and calls Linear directly
   (`checkout.rs:192`), so an explicitly GitHub or none project can resolve or
   reject a number based on unrelated Linear issues. No delivery task removes
   this path. *Fix:* resolve issue-side candidates through
   `Tracker::candidates`.
2. **Status ambiguity detection remains incomplete.** GraphQL `totalCount`
   protects `pr_by_head`, but `best_pr` is still fed by a repository-wide list
   capped at 500 (`status.rs:249`); it can report `Unique` or `NoMatch` while
   another matching PR lies beyond the window, potentially authorizing `issue
   end` from the wrong merged PR. *Fix:* resolve each distinct worktree branch
   through the complete typed head lookup, batched with GraphQL aliases.
3. **`AMBIGUOUS` does not fit the serialized status contract.**
   `IssueWorktree` has one `pr_number` and one `pr_url` (`status.rs:37`), so
   there is nowhere to carry the promised candidates; the renderer would display
   `AMBIGUOUS #0` (`triage.rs:5`), and MCP consumers receive no structured
   candidate identities. *Fix:* a tagged PR-status representation, with the
   legacy display fields and the finished verdict derived from it.
4. **"Recorded PR is authoritative" conflicts with implicit rebinding.** If
   `review request` honors the record it cannot escape a stale binding; if it
   ignores the record it can rebind to the wrong repository. A superseded
   same-branch PR is especially unrecoverable, because old plus new produces
   `Ambiguous`, which acting paths refuse. *Fix:* make the recorded locator
   authoritative for acting paths and add an explicit `--pr <URL|number>`
   rebind; branch discovery only when nothing is recorded.
5. **Task 1 cannot make configured repositories authoritative while preserving
   its claimed fallback behavior.** `--repo` is passed only when `repos.prs`
   differs from origin; when an explicitly configured `pr_repo` equals origin,
   `gh` still honors `GH_REPO` and can act on another repository. `Repos {
   issues, prs }` loses the provenance needed to tell that from an unset
   default. *Fix:* retain configured/defaulted provenance and always pass
   `--repo` for configured or overridden repositories.

### Claude's response

All five verified and accepted. Finding 1 turned out to be the visible corner of
something considerably larger, found by following it rather than by taking it as
written.

- **Finding 1 accepted, and it expanded.** `candidates` has exactly one
  reference in the workspace and it is an assertion in `tracker/mod.rs`'s own
  tests. Counting callers of every trait method from outside `tracker/`:
  `details`, `candidates`, `issues_for_prs`, `assigned_history` and
  `timeline_origin` have **zero**; `title`, `issue_ref` and `issue_pr` have one
  each. Phase 2 moved `status` and `end` onto the seam and nothing else.

  The sharpest consequence is `issue setup`, which this spec's opening paragraph
  promises will give a GitHub project a title-derived slug and a summary file.
  `resolve_slug` calls `slug::linear_key()?` — a hard error without a Linear key
  — then `linear::issue_title`, then `slug::from_linear_title`, and prints `slug
  from Linear:`; `fetch_details` does the same for `linear::issue_details`. The
  entry point never asks the tracker anything. So the spec's headline promise
  was false as written, and "wire the remaining commands to the trait" became
  its own delivery task sized against that table rather than against a guess.
- **Finding 2 accepted.** Feeding `best_pr` the typed answer would not have
  fixed it, because its *input* is the problem: `fetch_prs` pulls the
  repository's PRs with `--limit 500` and `best_pr` filters what came back. A
  truncated window produces a false `Unique` with no signal. Ambiguity computed
  from a truncated set is not ambiguity detection. Status now resolves each
  distinct worktree branch through the typed lookup, batched by GraphQL alias
  the way `linear::build_query` already batches — the branch count is the
  worktree count, so the repository's total PR count stops mattering.
- **Finding 3 accepted, and the `AMBIGUOUS #0` prediction is exact.**
  `triage.rs`'s `pr_label` is `format!("{} #{}", row.pr_state,
  row.pr_number.unwrap_or(0))`, so a state string with no number prints a PR
  number that does not exist, in the column a human reads before deleting a
  worktree. The row now carries the tag itself and `pr_state`, `pr_number` and
  `pr_url` derive from it, which keeps the serialized shape and gives MCP the
  candidates.
- **Finding 4 accepted, reversing last round's narrowing.** I declined the
  rebind command in round 3 on the grounds that the implicit write covers it.
  This round produced the case that breaks that: an old PR and its replacement
  share a head branch, the lookup returns `Ambiguous`, acting paths refuse, and
  the command that was supposed to fix the binding is the one that cannot run.
  `issue review request --pr <URL|number>` is added, and the record binds acting
  paths as well as `status`. I considered ranking the candidates by state
  instead and rejected it: ranking across head owners is precisely how a
  stranger's merged PR wins, which round 3's finding 2 existed to prevent.
- **Finding 5 accepted.** `cmd::capture` inherits the environment, so `gh` reads
  `GH_REPO`. A project that deliberately set `pr_repo` to its origin's slug
  would have had that overridden by an ambient variable. `Repos` keeps
  configured/overridden/defaulted provenance per field, and `--repo` is passed
  for anything configured or overridden even when the value equals origin.

Delivery grew from nine tasks to twelve, most of the growth being the trait
wiring and the split Codex asked about: identifier repositories and the recorded
PR binding are now separate tasks, and status's PR resolution is separate from
the typed lookup beneath it.

Four rounds, twenty findings, none rejected. One round-3 decision reversed on
round-4 evidence. This revision is unreviewed.

## Round 5: Codex

`VERDICT: REVISE`. The tagged status and task ordering are sound; the trait
wiring and the PR-binding contract still have gaps.

1. **Task 4 has no valid input-parsing transition for `issue setup`.** It uses
   the Linear-only `slug::parse_issue_ref` (`setup.rs:292`), so a GitHub issue
   URL or `#42` never reaches `GithubTracker::issue_ref`; replacing it with
   detected-tracker parsing would break legacy undeclared projects where a
   Linear URL supplies its slug without a key. And the trait method cannot
   implement the promised repository-mismatch refusal, because it cannot return
   an error (`tracker/mod.rs:129`). *Fix:* make `issue_ref` return
   `Result<IssueRef>`, call it from setup and checkout, and preserve Linear-URL
   parsing when the tracker was not explicitly declared.
2. **Task 4 still leaves `issues_for_prs` dead.** The task replaces only the
   workspace-URL lookup, while `devkit-issue::prs::gather` calls
   `linear::issues_for_prs` directly behind `[linear] resolve_pr_links`
   (`prs.rs:903`); GitHub PR rows never receive their closing issue ids, which
   contradicts the task's own "every method has a caller" gate. *Fix:* inject
   the resolved tracker into PR gathering and keep `resolve_pr_links` as
   Linear's opt-in, in both the CLI and MCP paths.
3. **`review request --pr` can authoritatively bind an unrelated valid PR.** The
   spec checks repository and number but never that the PR belongs to this
   worktree; once that PR merges, a completed issue and a clean tree satisfy
   `issue end` and the worktree is deleted. *Fix:* require the selected PR's
   `headRefOid` to equal the current pushed `HEAD` before acting or writing.
4. **Record authority leaves `review finish --pr` precedence undefined.** That
   flag wins over branch discovery by contract today (`finish.rs:81`); making
   the record unconditionally authoritative either silently disables it or
   leaves an undocumented override. *Fix:* define precedence as explicit
   locator, then record, then branch lookup, with `review finish --pr` a
   one-run, non-rebinding override.
5. **Repository values become cache-key components without validation.**
   `cache::path_for` interpolates the key straight into a filename
   (`cache.rs:26`); a repository-controlled `devkit.toml` with traversal
   components can make dashboard writes escape the cache directory. *Fix:*
   validate configured repositories as `owner/repo` slugs and encode every
   cache-scope component before constructing a filename.

### Claude's response

All five verified and accepted. Two were contradictions inside the spec itself
rather than gaps between spec and code.

- **Finding 1 accepted.** `fn issue_ref(&self, input: &str) -> IssueRef` has no
  `Result`, so the refusal this spec promised in "A pasted URL keeps its
  repository" could not be expressed by the method that was supposed to make it.
  `checkout-pr` already works around the absence by treating a `/` in the
  returned id as a parse failure; that heuristic goes away with the signature
  change. The undeclared-project half is a real regression I would have shipped:
  `slug::parse_issue_ref` recognizes a `linear.app` URL by string alone and
  needs no key, so routing it through `NoneTracker` would drop the slug for a
  project with no tracker configured. `Resolved.declared` already exists to
  separate the project's own answer from devkit's fallback, so the fallback
  keeps the permissive parse.
- **Finding 2 accepted.** `prs::gather` calls `linear::issues_for_prs` directly,
  which task 4 did not mention while its gate claimed every method would gain a
  caller. `resolve_pr_links` keeps its meaning as Linear's opt-in — it was added
  to gate an expensive call, and GitHub's equivalent is a field on a query
  already being made.
- **Finding 3 accepted; the check substituted.** The hazard is exact: `--pr`
  with a mistyped number names a real PR that resolves cleanly, the record makes
  it authoritative, and its merge lets `issue end` delete a worktree whose work
  never landed. I did not take the `headRefOid` comparison, because it refuses
  whenever the local branch is ahead of what was pushed — the normal state right
  up until `review request` pushes — and breaks after any force-push. The
  selected PR's `headRefName` must equal the worktree's branch instead: it is
  what actually ties a PR to a worktree, it survives both cases, and it still
  admits the supersede case the flag exists for.
- **Finding 4 accepted.** `resolve_pr` is `pr_flag.or(branch_pr)`, so `review
  finish --pr` wins today and my record rule would have silently changed what
  the flag does. Precedence is now explicit locator, then record, then branch
  discovery. This also let `--pr` collapse to one meaning everywhere — *use this
  PR for this run* — with rebinding falling out of the already-stated rule that
  `review request` records what it acted on, rather than being a second meaning
  bolted onto the same flag.
- **Finding 5 accepted, both halves.** `path_for` is a raw `format!("{key}.json")`
  interpolation, and `issues_repo` travels with a checkout, so scoping the key
  by repository would let a cloned repository steer a dashboard write out of the
  cache directory. Validation and encoding are each right on their own:
  a slug that is not `owner/repo` is a configuration error worth reporting where
  it is resolved, and encoding makes `path_for` safe against whatever a future
  key includes.

Delivery stays at twelve tasks; tasks 1, 4, 7 and 8 grew.

Five rounds, twenty-five findings, none rejected outright — one round-3 decision
reversed on round-4 evidence, and one round-5 mechanism substituted with the
reason recorded. This revision is unreviewed.
