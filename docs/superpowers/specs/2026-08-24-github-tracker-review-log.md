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
