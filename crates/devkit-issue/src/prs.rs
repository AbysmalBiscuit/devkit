use anyhow::Result;
use devkit_common::cmd::gh_json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// GraphQL response shapes ---------------------------------------------------------

#[derive(serde::Deserialize)]
struct GqlResp {
    data: GqlData,
}

#[derive(serde::Deserialize)]
struct GqlData {
    viewer: Viewer,
    mine: SearchNodes,
    #[serde(rename = "reviewRequested")]
    review_requested: SearchNodes,
    #[serde(rename = "reviewedBy")]
    reviewed_by: SearchNodes,
}

#[derive(serde::Deserialize)]
struct Viewer {
    login: String,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct SearchNodes {
    nodes: Vec<PrNode>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct ActorLogin {
    login: String,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct ReviewNode {
    author: ActorLogin,
    state: String,
    #[serde(rename = "submittedAt")]
    submitted_at: String,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct ReviewConn {
    nodes: Vec<ReviewNode>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct ReqNode {
    #[serde(rename = "requestedReviewer")]
    requested_reviewer: Option<ActorLogin>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct ReqConn {
    nodes: Vec<ReqNode>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct Rollup {
    state: String,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct CommitInner {
    #[serde(rename = "statusCheckRollup")]
    status_check_rollup: Option<Rollup>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct CommitNode {
    commit: CommitInner,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct CommitsConn {
    nodes: Vec<CommitNode>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct PrNode {
    number: u64,
    url: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "isDraft")]
    is_draft: bool,
    #[serde(rename = "reviewDecision")]
    review_decision: Option<String>,
    mergeable: String,
    author: ActorLogin,
    commits: CommitsConn,
    reviews: ReviewConn,
    #[serde(rename = "reviewRequests")]
    review_requests: ReqConn,
}

impl PrNode {
    /// The status-check rollup state of the last commit, if any.
    fn rollup_state(&self) -> Option<&str> {
        self.commits
            .nodes
            .first()
            .and_then(|c| c.commit.status_check_rollup.as_ref())
            .map(|r| r.state.as_str())
    }
}

#[derive(Deserialize)]
struct RepoInfo {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

// pure logic --------------------------------------------------------------------

const FAIL: [&str; 4] = ["FAILURE", "ERROR", "TIMED_OUT", "CANCELLED"];
#[allow(dead_code)]
const RUNNING: [&str; 3] = ["IN_PROGRESS", "QUEUED", "PENDING"];

/// The issue id a PR addresses, taken from its branch (head) ref and uppercased.
fn issue_of(head: &str) -> String {
    devkit_common::worktree::find_id(head)
        .map(|s| s.to_uppercase())
        .unwrap_or_else(|| "-".to_string())
}

fn checks_text(rollup: Option<&str>) -> &'static str {
    match rollup {
        None => "-",
        Some("SUCCESS") => "ok",
        Some(s) if FAIL.contains(&s) => "fail",
        Some(_) => "run",
    }
}

fn review_text(pr: &PrNode) -> &'static str {
    if changes_requested(pr) {
        return "changes";
    }
    match pr.review_decision.as_deref() {
        Some("APPROVED") => "approved",
        Some("REVIEW_REQUIRED") => "awaiting",
        _ => {
            if pr.reviews.nodes.is_empty() {
                "awaiting"
            } else {
                "commented"
            }
        }
    }
}

/// Logins whose current effective review is `CHANGES_REQUESTED`. A reviewer's
/// effective state is their most recent `APPROVED` / `CHANGES_REQUESTED` /
/// `DISMISSED` review; `COMMENTED` and `PENDING` reviews leave a standing
/// request untouched. Bots are included — any actor that requests changes counts.
fn change_requesters(pr: &PrNode) -> Vec<&str> {
    let mut latest: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
    for r in &pr.reviews.nodes {
        if !matches!(
            r.state.as_str(),
            "APPROVED" | "CHANGES_REQUESTED" | "DISMISSED"
        ) {
            continue;
        }
        let slot = latest.entry(r.author.login.as_str()).or_insert(("", ""));
        if r.submitted_at.as_str() >= slot.0 {
            *slot = (r.submitted_at.as_str(), r.state.as_str());
        }
    }
    latest
        .into_iter()
        .filter(|(_, (_, state))| *state == "CHANGES_REQUESTED")
        .map(|(login, _)| login)
        .collect()
}

/// True when the PR carries a standing change request. Driven by the per-author
/// effective review state so any actor (human or bot, required or not) counts;
/// falls back to GitHub's `reviewDecision` in case the review list was truncated.
fn changes_requested(pr: &PrNode) -> bool {
    !change_requesters(pr).is_empty() || pr.review_decision.as_deref() == Some("CHANGES_REQUESTED")
}

/// True when a reviewer who requested changes is back in the pending
/// review-request list. GitHub drops a reviewer from `reviewRequests` once they
/// submit a review, so their reappearance means re-review was requested of them.
fn re_review_requested(pr: &PrNode) -> bool {
    let requesters = change_requesters(pr);
    pr.review_requests
        .nodes
        .iter()
        .filter_map(|r| r.requested_reviewer.as_ref())
        .any(|rr| requesters.contains(&rr.login.as_str()))
}

fn mine_action(pr: &PrNode) -> String {
    if pr.is_draft {
        return "draft".into();
    }
    let conflict = pr.mergeable == "CONFLICTING";
    if changes_requested(pr) {
        let base = if re_review_requested(pr) {
            "await re-review"
        } else {
            "address changes"
        };
        return format!("{base}{}", if conflict { " + rebase" } else { "" });
    }
    match pr.review_decision.as_deref() {
        Some("APPROVED") => {
            if conflict {
                "rebase -> merge".into()
            } else if checks_text(pr.rollup_state()) == "fail" {
                "fix CI -> merge".into()
            } else {
                "MERGE".into()
            }
        }
        _ => format!("awaiting review{}", if conflict { "; rebase" } else { "" }),
    }
}

/// My effective review verdict on a PR. A `COMMENTED` reply never supersedes a
/// standing `APPROVED`/`CHANGES_REQUESTED`: the latest *decision* review
/// (`APPROVED`/`CHANGES_REQUESTED`/`DISMISSED`) wins, mirroring GitHub's own
/// review-decision semantics and the `change_requesters` rule on the mine path.
/// Only when there is no standing decision does a `COMMENTED` review count.
fn my_vote(pr: &PrNode, me: &str) -> &'static str {
    let decision = pr
        .reviews
        .nodes
        .iter()
        .filter(|r| r.author.login == me)
        .filter(|r| matches!(r.state.as_str(), "APPROVED" | "CHANGES_REQUESTED" | "DISMISSED"))
        .max_by(|a, b| a.submitted_at.cmp(&b.submitted_at))
        .map(|r| r.state.as_str());
    match decision {
        Some("APPROVED") => "APPROVED",
        Some("CHANGES_REQUESTED") => "CHANGES_REQUESTED",
        // No standing decision (none, or a dismissed review): a comment still
        // prompts the reviewer to decide.
        _ if pr
            .reviews
            .nodes
            .iter()
            .any(|r| r.author.login == me && r.state == "COMMENTED") =>
        {
            "COMMENTED"
        }
        _ => "",
    }
}

/// (my_vote, action) for a PR where I'm a reviewer.
fn reviewer_state(pr: &PrNode, me: &str) -> (String, String) {
    let vote = my_vote(pr, me);
    let requested = pr
        .review_requests
        .nodes
        .iter()
        .filter_map(|r| r.requested_reviewer.as_ref())
        .any(|rr| rr.login == me);
    let vote_label = match vote {
        "APPROVED" => "approved",
        "CHANGES_REQUESTED" => "changes",
        "COMMENTED" => "commented",
        _ => "-",
    }
    .to_string();
    let action = if requested {
        "REVIEW NEEDED"
    } else {
        match vote {
            "APPROVED" => "done (approved)",
            "CHANGES_REQUESTED" => "awaiting author fixes",
            "COMMENTED" => "commented; decide",
            _ => "REVIEW NEEDED",
        }
    }
    .to_string();
    (vote_label, action)
}

// GraphQL fetch -----------------------------------------------------------------

const PR_FIELDS: &str = "number url headRefName isDraft reviewDecision mergeable \
author { login } \
commits(last: 1) { nodes { commit { statusCheckRollup { state } } } } \
reviews(last: 100) { nodes { author { login } state submittedAt } } \
reviewRequests(first: 100) { nodes { requestedReviewer { ... on User { login } } } }";

fn build_query(repo: &str) -> String {
    let scope = format!("repo:{repo} ");
    let frag = format!("nodes {{ ... on PullRequest {{ {PR_FIELDS} }} }}");
    format!(
        "query {{ viewer {{ login }} \
mine: search(query: \"{scope}is:pr is:open author:@me\", type: ISSUE, first: 100) {{ {frag} }} \
reviewRequested: search(query: \"{scope}is:pr is:open review-requested:@me\", type: ISSUE, first: 100) {{ {frag} }} \
reviewedBy: search(query: \"{scope}is:pr is:open reviewed-by:@me\", type: ISSUE, first: 100) {{ {frag} }} }}"
    )
}

/// Turn one GraphQL response into the report. Pure → unit-tested.
fn classify(data: GqlData, want_mine: bool, want_reviews: bool) -> PrsReport {
    let me = data.viewer.login;

    let mine_views: Vec<MinePrView> = if want_mine {
        data.mine
            .nodes
            .iter()
            .filter(|pr| pr.number != 0)
            .map(|pr| MinePrView {
                number: pr.number,
                url: pr.url.clone(),
                issue_id: issue_of(&pr.head_ref_name),
                review_state: review_text(pr).to_string(),
                check_state: checks_text(pr.rollup_state()).to_string(),
                action: mine_action(pr),
            })
            .collect()
    } else {
        Vec::new()
    };

    let review_views: Vec<ReviewPrView> = if want_reviews {
        let mut seen: BTreeMap<u64, PrNode> = BTreeMap::new();
        for pr in data
            .review_requested
            .nodes
            .into_iter()
            .chain(data.reviewed_by.nodes)
            .filter(|pr| pr.number != 0 && pr.author.login != me)
        {
            seen.entry(pr.number).or_insert(pr);
        }
        seen.into_values()
            .map(|pr| {
                let (my_vote, action) = reviewer_state(&pr, &me);
                ReviewPrView {
                    number: pr.number,
                    url: pr.url.clone(),
                    author: pr.author.login.clone(),
                    my_vote,
                    action,
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    PrsReport {
        mine: mine_views,
        reviews: review_views,
    }
}

// views + gather ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct MinePrView {
    pub number: u64,
    pub url: String,
    pub issue_id: String,
    pub review_state: String,
    pub check_state: String,
    pub action: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewPrView {
    pub number: u64,
    pub url: String,
    pub author: String,
    pub my_vote: String,
    pub action: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrsReport {
    pub mine: Vec<MinePrView>,
    pub reviews: Vec<ReviewPrView>,
}

/// Resolve `owner/name`. Returns `repo` as-is when given, else asks `gh`.
pub fn resolve_repo(repo: Option<&str>, cwd: &str) -> Result<String> {
    if let Some(r) = repo {
        return Ok(r.to_string());
    }
    let info: RepoInfo = gh_json(&["repo", "view", "--json", "nameWithOwner"], cwd)?;
    Ok(info.name_with_owner)
}

/// Fetch and classify the caller's PRs in a single GraphQL round-trip. Neither
/// flag set ⇒ both groups. Stateless: no diff cache is read or written.
pub fn gather(root: &str, mine: bool, reviews: bool, repo: Option<&str>) -> Result<PrsReport> {
    let want_mine = mine || !reviews;
    let want_reviews = reviews || !mine;
    let repo = match repo {
        Some(r) => r.to_string(),
        None => resolve_repo(None, root)?,
    };
    let query = build_query(&repo);
    let arg = format!("query={query}");
    let resp: GqlResp = gh_json(&["api", "graphql", "-f", &arg], root)?;
    Ok(classify(resp.data, want_mine, want_reviews))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(json: serde_json::Value) -> PrNode {
        serde_json::from_value(json).unwrap()
    }

    // A representative `gh api graphql` response parses into the views with the
    // same classification the old per-`gh pr list` path produced.
    #[test]
    fn parses_graphql_and_classifies() {
        let raw = r#"{
          "data": {
            "viewer": { "login": "me" },
            "mine": { "nodes": [
              { "number": 10, "url": "u10", "headRefName": "lev/eng-1-foo",
                "isDraft": false, "reviewDecision": "APPROVED", "mergeable": "MERGEABLE",
                "author": {"login": "me"},
                "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "SUCCESS"}}}]},
                "reviews": {"nodes": [{"author": {"login": "alice"}, "state": "APPROVED", "submittedAt": "2026-06-20T10:00:00Z"}]},
                "reviewRequests": {"nodes": []} }
            ]},
            "reviewRequested": { "nodes": [
              { "number": 20, "url": "u20", "headRefName": "x",
                "isDraft": false, "reviewDecision": "REVIEW_REQUIRED", "mergeable": "MERGEABLE",
                "author": {"login": "bob"},
                "commits": {"nodes": []},
                "reviews": {"nodes": []},
                "reviewRequests": {"nodes": [{"requestedReviewer": {"login": "me"}}]} }
            ]},
            "reviewedBy": { "nodes": [] }
          }
        }"#;
        let resp: GqlResp = serde_json::from_str(raw).unwrap();
        let report = classify(resp.data, true, true);
        assert_eq!(report.mine.len(), 1);
        assert_eq!(report.mine[0].number, 10);
        assert_eq!(report.mine[0].issue_id, "ENG-1");
        assert_eq!(report.mine[0].review_state, "approved");
        assert_eq!(report.mine[0].check_state, "ok");
        assert_eq!(report.mine[0].action, "MERGE");
        assert_eq!(report.reviews.len(), 1);
        assert_eq!(report.reviews[0].number, 20);
        assert_eq!(report.reviews[0].my_vote, "-");
        assert_eq!(report.reviews[0].action, "REVIEW NEEDED");
    }

    fn mine_node(
        decision: Option<&str>,
        mergeable: &str,
        draft: bool,
        rollup: Option<&str>,
    ) -> PrNode {
        let commits = match rollup {
            Some(s) => {
                serde_json::json!({"nodes": [{"commit": {"statusCheckRollup": {"state": s}}}]})
            }
            None => serde_json::json!({"nodes": []}),
        };
        node(serde_json::json!({
            "number": 1, "url": "u", "headRefName": "h",
            "isDraft": draft, "reviewDecision": decision, "mergeable": mergeable,
            "author": {"login": "x"}, "commits": commits,
            "reviews": {"nodes": []}, "reviewRequests": {"nodes": []}
        }))
    }

    #[test]
    fn checks_fail_run_ok_empty() {
        assert_eq!(checks_text(None), "-");
        assert_eq!(checks_text(Some("SUCCESS")), "ok");
        assert_eq!(checks_text(Some("FAILURE")), "fail");
        assert_eq!(checks_text(Some("ERROR")), "fail");
        assert_eq!(checks_text(Some("PENDING")), "run");
        assert_eq!(checks_text(Some("EXPECTED")), "run");
    }

    #[test]
    fn approved_green_merges() {
        assert_eq!(
            mine_action(&mine_node(
                Some("APPROVED"),
                "MERGEABLE",
                false,
                Some("SUCCESS")
            )),
            "MERGE"
        );
    }
    #[test]
    fn approved_with_failing_ci() {
        assert_eq!(
            mine_action(&mine_node(
                Some("APPROVED"),
                "MERGEABLE",
                false,
                Some("FAILURE")
            )),
            "fix CI -> merge"
        );
    }
    #[test]
    fn changes_requested_action() {
        assert_eq!(
            mine_action(&mine_node(
                Some("CHANGES_REQUESTED"),
                "MERGEABLE",
                false,
                None
            )),
            "address changes"
        );
    }
    #[test]
    fn draft_action() {
        assert_eq!(
            mine_action(&mine_node(None, "MERGEABLE", true, None)),
            "draft"
        );
    }

    /// A node addressing a change request: the human's `CHANGES_REQUESTED`
    /// followed by my own `COMMENTED` replies (e.g. answering a bot's inline
    /// threads), with `requested` controlling whether the human is re-requested.
    fn change_request_node(requested: bool) -> PrNode {
        let reviews = serde_json::json!({"nodes": [
            {"author": {"login": "human"}, "state": "CHANGES_REQUESTED", "submittedAt": "2026-06-23T11:00:00Z"},
            {"author": {"login": "me"}, "state": "COMMENTED", "submittedAt": "2026-06-23T13:00:00Z"}
        ]});
        let requests = if requested {
            serde_json::json!({"nodes": [{"requestedReviewer": {"login": "human"}}]})
        } else {
            serde_json::json!({"nodes": []})
        };
        node(serde_json::json!({
            "number": 1, "url": "u", "headRefName": "h", "isDraft": false,
            "reviewDecision": "CHANGES_REQUESTED", "mergeable": "MERGEABLE",
            "author": {"login": "me"}, "commits": {"nodes": []},
            "reviews": reviews, "reviewRequests": requests
        }))
    }

    // Replying to a comment thread (my COMMENTED review newer than the human's
    // CHANGES_REQUESTED) is not a re-review: with the human absent from the
    // pending request list the action stays "address changes".
    #[test]
    fn reply_without_re_request_stays_address_changes() {
        assert_eq!(mine_action(&change_request_node(false)), "address changes");
    }

    // Once the change-requester is re-requested they are back in reviewRequests,
    // so the action flips to "await re-review".
    #[test]
    fn re_requested_awaits_re_review() {
        assert_eq!(mine_action(&change_request_node(true)), "await re-review");
    }

    // A change request from a non-required reviewer (or bot) that GitHub does not
    // surface in `reviewDecision` still shows as "changes" / "address changes".
    #[test]
    fn non_required_change_request_counts() {
        let pr = node(serde_json::json!({
            "number": 1, "url": "u", "headRefName": "h", "isDraft": false,
            "reviewDecision": null, "mergeable": "MERGEABLE", "author": {"login": "me"},
            "commits": {"nodes": []},
            "reviews": {"nodes": [
                {"author": {"login": "greptile-apps"}, "state": "CHANGES_REQUESTED", "submittedAt": "2026-06-18T17:00:00Z"}
            ]},
            "reviewRequests": {"nodes": []}
        }));
        assert_eq!(review_text(&pr), "changes");
        assert_eq!(mine_action(&pr), "address changes");
    }

    // A later APPROVED clears a standing change request; a later COMMENTED does not.
    #[test]
    fn approval_clears_changes_comment_does_not() {
        let with = |last: &str, ts: &str| {
            node(serde_json::json!({
                "number": 1, "url": "u", "headRefName": "h", "isDraft": false,
                "reviewDecision": null, "mergeable": "MERGEABLE", "author": {"login": "me"},
                "commits": {"nodes": []},
                "reviews": {"nodes": [
                    {"author": {"login": "human"}, "state": "CHANGES_REQUESTED", "submittedAt": "2026-06-23T11:00:00Z"},
                    {"author": {"login": "human"}, "state": last, "submittedAt": ts}
                ]},
                "reviewRequests": {"nodes": []}
            }))
        };
        assert!(!changes_requested(&with(
            "APPROVED",
            "2026-06-23T12:00:00Z"
        )));
        assert!(changes_requested(&with(
            "COMMENTED",
            "2026-06-23T12:00:00Z"
        )));
    }
    #[test]
    fn review_text_variants() {
        assert_eq!(
            review_text(&mine_node(Some("APPROVED"), "x", false, None)),
            "approved"
        );
        assert_eq!(
            review_text(&mine_node(Some("CHANGES_REQUESTED"), "x", false, None)),
            "changes"
        );
        assert_eq!(review_text(&mine_node(None, "x", false, None)), "awaiting");
    }
    #[test]
    fn reviewer_state_requested_needs_review() {
        let pr = node(serde_json::json!({
            "number": 1, "url": "u", "headRefName": "h", "isDraft": false,
            "reviewDecision": null, "mergeable": "MERGEABLE", "author": {"login": "other"},
            "commits": {"nodes": []}, "reviews": {"nodes": []},
            "reviewRequests": {"nodes": [{"requestedReviewer": {"login": "me"}}]}
        }));
        let (vote, action) = reviewer_state(&pr, "me");
        assert_eq!(vote, "-");
        assert_eq!(action, "REVIEW NEEDED");
    }
    // A later COMMENTED reply (e.g. answering a thread) does not clear a standing
    // CHANGES_REQUESTED: the effective vote stays "changes" / "awaiting author fixes".
    #[test]
    fn reviewer_state_comment_does_not_supersede_changes() {
        let pr = node(serde_json::json!({
            "number": 1, "url": "u", "headRefName": "h", "isDraft": false,
            "reviewDecision": "CHANGES_REQUESTED", "mergeable": "MERGEABLE",
            "author": {"login": "other"}, "commits": {"nodes": []},
            "reviews": {"nodes": [
                {"author": {"login": "me"}, "state": "CHANGES_REQUESTED", "submittedAt": "2026-06-17T08:27:20Z"},
                {"author": {"login": "me"}, "state": "COMMENTED", "submittedAt": "2026-06-17T10:05:44Z"}
            ]},
            "reviewRequests": {"nodes": []}
        }));
        let (vote, action) = reviewer_state(&pr, "me");
        assert_eq!(vote, "changes");
        assert_eq!(action, "awaiting author fixes");
    }

    // With only COMMENTED reviews and no standing decision, the vote remains
    // "commented" so the reviewer is prompted to decide.
    #[test]
    fn reviewer_state_only_comments_decides() {
        let pr = node(serde_json::json!({
            "number": 1, "url": "u", "headRefName": "h", "isDraft": false,
            "reviewDecision": null, "mergeable": "MERGEABLE",
            "author": {"login": "other"}, "commits": {"nodes": []},
            "reviews": {"nodes": [
                {"author": {"login": "me"}, "state": "COMMENTED", "submittedAt": "2026-06-17T10:05:44Z"}
            ]},
            "reviewRequests": {"nodes": []}
        }));
        let (vote, action) = reviewer_state(&pr, "me");
        assert_eq!(vote, "commented");
        assert_eq!(action, "commented; decide");
    }

    // A later APPROVED supersedes an earlier CHANGES_REQUESTED (real decision change).
    #[test]
    fn reviewer_state_approval_supersedes_changes() {
        let pr = node(serde_json::json!({
            "number": 1, "url": "u", "headRefName": "h", "isDraft": false,
            "reviewDecision": "APPROVED", "mergeable": "MERGEABLE",
            "author": {"login": "other"}, "commits": {"nodes": []},
            "reviews": {"nodes": [
                {"author": {"login": "me"}, "state": "CHANGES_REQUESTED", "submittedAt": "2026-06-17T08:27:20Z"},
                {"author": {"login": "me"}, "state": "APPROVED", "submittedAt": "2026-06-17T10:05:44Z"}
            ]},
            "reviewRequests": {"nodes": []}
        }));
        let (vote, action) = reviewer_state(&pr, "me");
        assert_eq!(vote, "approved");
        assert_eq!(action, "done (approved)");
    }

    #[test]
    fn reviewer_state_approved_done() {
        let pr = node(serde_json::json!({
            "number": 1, "url": "u", "headRefName": "h", "isDraft": false,
            "reviewDecision": null, "mergeable": "MERGEABLE", "author": {"login": "other"},
            "commits": {"nodes": []},
            "reviews": {"nodes": [{"author": {"login": "me"}, "state": "APPROVED", "submittedAt": "2026-01-01T00:00:00Z"}]},
            "reviewRequests": {"nodes": []}
        }));
        let (vote, action) = reviewer_state(&pr, "me");
        assert_eq!(vote, "approved");
        assert_eq!(action, "done (approved)");
    }
    #[test]
    fn issue_of_finds_swe() {
        assert_eq!(issue_of("lev/swe-123-fix"), "SWE-123");
        assert_eq!(issue_of("main"), "-");
    }
    #[test]
    fn issue_of_finds_non_swe_prefix() {
        assert_eq!(issue_of("lev/eng-1234-fix"), "ENG-1234");
        assert_eq!(issue_of("feature/abc-9-thing"), "ABC-9");
    }
}
