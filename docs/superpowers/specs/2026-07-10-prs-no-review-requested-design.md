# `issue prs`: distinguish "no review requested" from "awaiting review"

## Problem

In a repo without a branch-protection review requirement, GitHub's
`reviewDecision` is null. `mine_action` (`crates/devkit-issue/src/prs.rs`)
labels every open non-draft PR that is neither approved nor change-requested
as `awaiting review`, and `review_text` shows `awaiting` — even when no
reviewer was ever requested and nothing blocks the merge. The label claims a
wait that isn't happening: nobody has been asked, and GitHub would let the PR
merge right now. Verified against the adaptyv monorepo: 8 of 10 open PRs have
`reviewDecision: ""` and zero `reviewRequests`, and all render as
`awaiting review`.

## Design

### The review-in-flight predicate

A review is *in flight* when someone is actually expected to review:

```rust
fn review_in_flight(pr: &PrNode) -> bool {
    pr.review_decision.as_deref() == Some("REVIEW_REQUIRED")
        || !pr.review_requests.nodes.is_empty()
}
```

- `REVIEW_REQUIRED` means branch protection blocks the merge, so waiting is
  real even with no named reviewer.
- A non-empty `reviewRequests` list means a reviewer was asked (including
  CODEOWNERS auto-requests).
- Submitted `COMMENTED` reviews do **not** count as in flight. Bots such as
  Greptile leave a `COMMENTED` review on every PR; if comments counted, the
  new state would never fire in the monorepo. A standing decision review
  (`APPROVED`/`CHANGES_REQUESTED`) is already handled by the earlier
  `approved`/`changes_requested` arms and never reaches this predicate's
  callers' fall-through.

### `mine_action`

After the existing draft / changes-requested / approved arms, the
fall-through splits on the predicate:

- review in flight → today's behavior: `awaiting review`
  (`awaiting review; rebase` on conflict).
- not in flight → the PR is yours to move; mirror the approved arm's
  CI/conflict gating so "you can merge" is never claimed falsely:
  - merge conflict → `rebase -> merge (unreviewed)`
  - CI failing (non-ignored) → `fix CI -> merge (unreviewed)`
  - otherwise → `MERGE (unreviewed)`

The `(unreviewed)` qualifier keeps plain `MERGE` reserved for approved PRs:
the repo may not *require* review while the team still *wants* one, so an
unreviewed merge stays visibly distinct.

### `review_text` (review column)

When there is no standing decision:

- `REVIEW_REQUIRED` or pending `reviewRequests` → `awaiting`
- submitted reviews exist (comments) → `commented` (as today)
- none of the above → `not requested` (new; replaces the misleading
  `awaiting`)

`review_text` gains access to `reviewRequests`, which it currently ignores.
The first rule deliberately changes one existing case: today a bot comment
masks a pending review request as `commented`; an in-flight request now wins
and reports `awaiting`.

### Rendering (`src/bin/issue/prs.rs`)

`paint_action` needs no new rules: `MERGE (unreviewed)` matches the green
`MERGE` prefix, `rebase -> merge (unreviewed)` matches the green
`rebase -> merge` prefix, and `fix CI -> merge (unreviewed)` matches the red
`fix` prefix. Update the legend line that groups actions by color to name
`MERGE (unreviewed)` in the green group.

### Cache

`MinePrView` is persisted in the pr-status snapshot cache as plain strings —
no schema change. After the upgrade, `diff_cell` shows a one-time
`awaiting review -> MERGE (unreviewed)` diff per PR; harmless.

## Out of scope

- `reviewer_state` / the reviews table (PRs where I'm the reviewer) — its
  `REVIEW NEEDED` / `done` labels are driven by explicit requests already.
- `issue status` — it has no review-state label.
- The MCP `issue.prs` action — it serializes the same strings and needs no
  change.
- Distinguishing a *human* comment-without-vote from a bot comment. Both
  yield review `commented` + action `MERGE (unreviewed)`. If that proves
  noisy, a follow-up can add an `address comments` state keyed on non-bot
  authors.

## Tests (TDD, in `crates/devkit-issue/src/prs.rs`)

New, written first and watched fail:

- `mine_action`, null decision, no requests, no reviews, CI green →
  `MERGE (unreviewed)`
- same with failing CI → `fix CI -> merge (unreviewed)`
- same with `CONFLICTING` → `rebase -> merge (unreviewed)`
- null decision + a pending `reviewRequests` entry → `awaiting review`
- `REVIEW_REQUIRED`, no requests → `awaiting review`
- bot `COMMENTED` review, no requests → `MERGE (unreviewed)`
- `review_text`: the four-way mapping above, including the new
  `not requested` and the request-wins-over-comment precedence

Existing tests that flip deliberately (the behavior change is the point):

- `review_text(&mine_node(None, ...))` expecting `awaiting` →
  `not requested`
- any `mine_action` test on a null-decision, request-less node expecting
  `awaiting review` → the new label

The `mine_node` helper builds nodes with empty `reviewRequests`; add a
variant (or parameter) that populates them.

## Resolved decisions

1. Action wording: `MERGE (unreviewed)` (with `fix CI ->` / `rebase ->`
   variants).
2. Color: green — the PR is mergeable now.
3. Review column: `not requested`.
