# `issue prs` No-Review-Requested Labels Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** In `issue prs`, stop labelling PRs with no review in flight as `awaiting review`; show `not requested` / `MERGE (unreviewed)` instead, gated on CI and conflicts.

**Architecture:** A new `review_in_flight` predicate in `crates/devkit-issue/src/prs.rs` (true when `reviewDecision == REVIEW_REQUIRED` or `reviewRequests` is non-empty) splits the existing fall-through in both `review_text` (REVIEW column) and `mine_action` (ACTION column). The not-in-flight action arm mirrors the approved arm's CI/conflict gating. Rendering needs no paint changes — the new labels match existing `paint_action` prefixes — only the legend text gains the new label.

**Tech Stack:** Rust (edition 2024), serde_json test fixtures, `cargo test --workspace`.

**Spec:** `docs/superpowers/specs/2026-07-10-prs-no-review-requested-design.md`

## Global Constraints

- Exact new labels (copy verbatim): REVIEW column `not requested`; ACTION column `MERGE (unreviewed)`, `fix CI -> merge (unreviewed)`, `rebase -> merge (unreviewed)`.
- Submitted `COMMENTED` reviews do NOT count as review-in-flight (bots such as Greptile comment on every PR).
- A pending `reviewRequests` entry wins over a stray comment: REVIEW column reads `awaiting`, not `commented`.
- `REVIEW_REQUIRED` and requested-reviewer PRs keep today's `awaiting` / `awaiting review` labels exactly.
- Plain `MERGE` stays reserved for approved PRs.
- Out of scope: `reviewer_state` (reviews table), `issue status`, the MCP `issue.prs` action, human-vs-bot comment distinction.
- Merge gate before every commit: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all`.
- Commits follow Conventional Commits; commit messages end with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

---

### Task 1: `review_in_flight` predicate + new labels in `review_text` / `mine_action`

**Files:**
- Modify: `crates/devkit-issue/src/prs.rs:338-355` (`review_text`)
- Modify: `crates/devkit-issue/src/prs.rs:421-445` (`mine_action`)
- Test: `crates/devkit-issue/src/prs.rs` (`#[cfg(test)] mod tests`, after `re_requested_awaits_re_review` at ~line 961)

**Interfaces:**
- Consumes: existing `PrNode`, `changes_requested(pr)`, `approved(pr)`, `check_verdict(pr, ignored)`, `Checks::Fail`, test helpers `node(json)` (~line 672) and `mine_node(decision, mergeable, draft, rollup)` (~line 733).
- Produces: `fn review_in_flight(pr: &PrNode) -> bool` (private, same file); `review_text` may now return `"not requested"`; `mine_action` may now return `"MERGE (unreviewed)"`, `"fix CI -> merge (unreviewed)"`, `"rebase -> merge (unreviewed)"`. Task 2 relies on these exact strings.

- [ ] **Step 1: Write the failing tests**

Add after the `re_requested_awaits_re_review` test (~line 961) in the `tests` module of `crates/devkit-issue/src/prs.rs`:

```rust
    /// Null-decision node (repo requires no reviews): `requested` controls the
    /// pending reviewRequests list, `reviews` the submitted review list.
    fn no_decision_node(
        mergeable: &str,
        rollup: &str,
        requested: bool,
        reviews: serde_json::Value,
    ) -> PrNode {
        let requests = if requested {
            serde_json::json!({"nodes": [{"requestedReviewer": {"login": "human"}}]})
        } else {
            serde_json::json!({"nodes": []})
        };
        node(serde_json::json!({
            "number": 1, "url": "u", "headRefName": "h", "isDraft": false,
            "reviewDecision": null, "mergeable": mergeable,
            "author": {"login": "me"},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": rollup}}}]},
            "reviews": reviews, "reviewRequests": requests
        }))
    }

    // No review required, requested, or submitted: nothing is in flight, so the
    // PR is not "awaiting" anything — it is mergeable now, flagged unreviewed.
    #[test]
    fn unrequested_pr_reads_merge_unreviewed() {
        let pr = no_decision_node("MERGEABLE", "SUCCESS", false, serde_json::json!({"nodes": []}));
        assert_eq!(review_text(&pr), "not requested");
        assert_eq!(mine_action(&pr, &[]), "MERGE (unreviewed)");
    }

    // The unreviewed arm carries the same CI/conflict gating as the approved
    // arm, so "merge" is never claimed while something blocks it.
    #[test]
    fn unrequested_pr_with_failing_ci() {
        let pr = no_decision_node("MERGEABLE", "FAILURE", false, serde_json::json!({"nodes": []}));
        assert_eq!(mine_action(&pr, &[]), "fix CI -> merge (unreviewed)");
    }
    #[test]
    fn unrequested_pr_with_conflict() {
        let pr = no_decision_node("CONFLICTING", "SUCCESS", false, serde_json::json!({"nodes": []}));
        assert_eq!(mine_action(&pr, &[]), "rebase -> merge (unreviewed)");
    }

    // A pending review request means someone is expected to review: the wait
    // is real even though the repo requires no review.
    #[test]
    fn pending_request_still_awaits_review() {
        let pr = no_decision_node("MERGEABLE", "SUCCESS", true, serde_json::json!({"nodes": []}));
        assert_eq!(review_text(&pr), "awaiting");
        assert_eq!(mine_action(&pr, &[]), "awaiting review");
    }

    // Branch protection requiring a review blocks the merge, so the PR stays
    // "awaiting review" even with no named reviewer.
    #[test]
    fn review_required_still_awaits_review() {
        let pr = mine_node(Some("REVIEW_REQUIRED"), "MERGEABLE", false, Some("SUCCESS"));
        assert_eq!(review_text(&pr), "awaiting");
        assert_eq!(mine_action(&pr, &[]), "awaiting review");
    }

    // A bot's COMMENTED review is not a review in flight: the REVIEW column
    // still reports the comment, but the action stays mergeable.
    #[test]
    fn bot_comment_does_not_mask_unrequested() {
        let reviews = serde_json::json!({"nodes": [
            {"author": {"login": "greptile-apps"}, "state": "COMMENTED", "submittedAt": "2026-07-01T10:00:00Z"}
        ]});
        let pr = no_decision_node("MERGEABLE", "SUCCESS", false, reviews);
        assert_eq!(review_text(&pr), "commented");
        assert_eq!(mine_action(&pr, &[]), "MERGE (unreviewed)");
    }

    // With a reviewer pending AND a stray bot comment, the in-flight request
    // wins: "awaiting", not "commented".
    #[test]
    fn pending_request_wins_over_comment() {
        let reviews = serde_json::json!({"nodes": [
            {"author": {"login": "greptile-apps"}, "state": "COMMENTED", "submittedAt": "2026-07-01T10:00:00Z"}
        ]});
        let pr = no_decision_node("MERGEABLE", "SUCCESS", true, reviews);
        assert_eq!(review_text(&pr), "awaiting");
        assert_eq!(mine_action(&pr, &[]), "awaiting review");
    }
```

Also update the flipping assertion in `review_text_variants` (~line 1035): a null-decision node with no requests and no reviews now reads `not requested`:

```rust
        assert_eq!(
            review_text(&mine_node(None, "x", false, None)),
            "not requested"
        );
```

- [ ] **Step 2: Run the tests to verify they fail for the right reason**

Run: `cargo test -p devkit-issue unrequested -- --nocapture` and `cargo test -p devkit-issue review_text_variants`

Expected: FAIL — `no_decision_node`/new tests assert `"not requested"` / `"MERGE (unreviewed)"` but get `"awaiting"` / `"awaiting review"`. (`pending_request_wins_over_comment` fails on `review_text` returning `"commented"`.) A compile error about a missing helper means the helper block wasn't added — that is the wrong RED; fix before proceeding.

- [ ] **Step 3: Implement the predicate and the new arms**

In `crates/devkit-issue/src/prs.rs`, add above `review_text` (~line 338):

```rust
/// True when someone is actually expected to review: branch protection
/// requires a review (`REVIEW_REQUIRED`), or a reviewer sits in the pending
/// request list (including CODEOWNERS auto-requests). Submitted `COMMENTED`
/// reviews don't count — bots comment on every PR, and a comment obliges
/// nobody. Standing decisions are handled by `approved`/`changes_requested`
/// before callers consult this predicate.
fn review_in_flight(pr: &PrNode) -> bool {
    pr.review_decision.as_deref() == Some("REVIEW_REQUIRED")
        || !pr.review_requests.nodes.is_empty()
}
```

Replace the body of `review_text` (lines 338-355) — the in-flight check comes before the comment check so a pending request wins over a stray comment:

```rust
fn review_text(pr: &PrNode) -> &'static str {
    if changes_requested(pr) {
        return "changes";
    }
    if approved(pr) {
        return "approved";
    }
    if review_in_flight(pr) {
        return "awaiting";
    }
    if pr.reviews.nodes.is_empty() {
        "not requested"
    } else {
        "commented"
    }
}
```

Replace the final `else` arm of `mine_action` (line 442-444, after the `approved` block):

```rust
    } else if review_in_flight(pr) {
        format!("awaiting review{}", if conflict { "; rebase" } else { "" })
    } else if conflict {
        "rebase -> merge (unreviewed)".into()
    } else if matches!(check_verdict(pr, ignored), Checks::Fail(_)) {
        "fix CI -> merge (unreviewed)".into()
    } else {
        "MERGE (unreviewed)".into()
    }
```

(The three unreviewed labels deliberately reuse the `MERGE` / `rebase -> merge` / `fix` prefixes that `paint_action` in `src/bin/issue/prs.rs` already colours green/green/red — no rendering change is needed.)

- [ ] **Step 4: Run the full gate**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`

Expected: all tests PASS (the 7 new ones plus the updated `review_text_variants`; no other test asserts the old fall-through), clippy clean, fmt makes no complaints. If any *other* existing test fails, stop and re-read it — the spec says only `review_text_variants` flips.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-issue/src/prs.rs
git commit -m "feat(issue): distinguish unrequested review from awaiting in prs

A PR with no required review, no pending review request, and no standing
decision showed \"awaiting review\" even though nobody was expected to
review and nothing blocked the merge. A new review_in_flight predicate
(REVIEW_REQUIRED or pending reviewRequests; COMMENTED reviews excluded so
bot comments don't mask the state) splits the fall-through: such PRs now
read \"not requested\" / \"MERGE (unreviewed)\", gated on CI and conflicts
like the approved arm. A pending request also wins over a stray comment
in the REVIEW column.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Legend names the new green label

**Files:**
- Modify: `src/bin/issue/prs.rs:166-178` (`legend_lines`)

**Interfaces:**
- Consumes: the exact action string `MERGE (unreviewed)` produced by Task 1's `mine_action`.
- Produces: nothing consumed downstream — display copy only.

- [ ] **Step 1: Update the legend**

In `legend_lines` (`src/bin/issue/prs.rs:166-178`), add the new label to the green group and drop the now-impossible bare-awaiting implication from the passive group note (the passive group keeps `awaiting review · draft`; both still exist):

```rust
        format!(
            "{} {} (REVIEW NEEDED · address changes · fix CI) · {} (MERGE · MERGE (unreviewed) · done) · {} (awaiting author fixes) · {}",
            ui::dim("ACTION colour:"),
            ui::red("needs you"),
            ui::green("ready to land"),
            ui::yellow("waiting on author"),
            ui::dim("passive (awaiting review · draft)"),
        ),
```

(Only the second `format!` positional segment changes: `{} (MERGE · done)` → `{} (MERGE · MERGE (unreviewed) · done)`.)

- [ ] **Step 2: Run the full gate**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`

Expected: PASS / clean — `legend_lines` has no test asserting its copy; this is a compile-and-gate check.

- [ ] **Step 3: Eyeball the real output**

Run: `cargo run --bin issue -- prs --repo adaptyvbio/monorepo --no-cache | head -40` (from any directory inside a repo; `--repo` pins the target).

Expected: the user's unrequested PRs (e.g. #3766, #3751) show REVIEW `not requested` and ACTION `MERGE (unreviewed)` (green); #3512/#3065 (pending reviewers) still show `awaiting review`; the legend's green group names `MERGE (unreviewed)`.

- [ ] **Step 4: Commit**

```bash
git add src/bin/issue/prs.rs
git commit -m "feat(issue): add MERGE (unreviewed) to prs action legend

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```
