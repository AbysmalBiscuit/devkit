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
