use anyhow::Result;
use devkit_common::cmd::gh_json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// gh JSON shapes ----------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct Check {
    conclusion: Option<String>,
    status: Option<String>,
    state: Option<String>,
}

#[derive(Deserialize)]
struct Author {
    login: String,
}

#[derive(Deserialize)]
struct Review {
    author: Author,
    #[serde(default)]
    state: String,
    #[serde(rename = "submittedAt", default)]
    submitted_at: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ReviewRequest {
    login: String,
}

#[derive(Deserialize)]
struct MinePr {
    number: u64,
    url: String,
    #[serde(rename = "headRefName", default)]
    head_ref_name: String,
    #[serde(rename = "isDraft", default)]
    is_draft: bool,
    #[serde(rename = "reviewDecision", default)]
    review_decision: Option<String>,
    #[serde(default)]
    mergeable: String,
    #[serde(rename = "statusCheckRollup", default)]
    status_check_rollup: Vec<Check>,
    #[serde(default)]
    reviews: Vec<Review>,
}

#[derive(Deserialize)]
struct ReviewPr {
    number: u64,
    url: String,
    author: Author,
    #[serde(rename = "latestReviews", default)]
    latest_reviews: Vec<Review>,
    #[serde(rename = "reviewRequests", default)]
    review_requests: Vec<ReviewRequest>,
}

#[derive(Deserialize)]
struct Me {
    login: String,
}

#[derive(Deserialize)]
struct RepoInfo {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

// pure logic --------------------------------------------------------------------

const BOTS: [&str; 3] = ["greptile-apps", "linear-code", "coderabbitai"];
const FAIL: [&str; 4] = ["FAILURE", "ERROR", "TIMED_OUT", "CANCELLED"];
const RUNNING: [&str; 3] = ["IN_PROGRESS", "QUEUED", "PENDING"];

fn is_bot(login: &str) -> bool {
    BOTS.contains(&login) || login.ends_with("[bot]")
}

/// The issue id a PR addresses, taken from its branch (head) ref and uppercased.
fn issue_of(head: &str) -> String {
    devkit_common::worktree::find_id(head)
        .map(|s| s.to_uppercase())
        .unwrap_or_else(|| "-".to_string())
}

fn checks_of(rollup: &[Check]) -> &'static str {
    if rollup.is_empty() {
        return "-";
    }
    let fail = rollup.iter().any(|c| {
        c.conclusion.as_deref().is_some_and(|x| FAIL.contains(&x))
            || matches!(c.state.as_deref(), Some("FAILURE") | Some("ERROR"))
    });
    if fail {
        return "fail";
    }
    let run = rollup.iter().any(|c| {
        c.status.as_deref().is_some_and(|x| RUNNING.contains(&x))
            || c.state.as_deref() == Some("PENDING")
    });
    if run {
        return "run";
    }
    "ok"
}

fn review_text(pr: &MinePr) -> &'static str {
    match pr.review_decision.as_deref() {
        Some("CHANGES_REQUESTED") => "changes",
        Some("APPROVED") => "approved",
        Some("REVIEW_REQUIRED") => "awaiting",
        _ => {
            if pr.reviews.is_empty() {
                "awaiting"
            } else {
                "commented"
            }
        }
    }
}

/// True when my latest review is newer than the latest non-bot reviewer's.
fn has_replied(pr: &MinePr, me: &str) -> bool {
    let mine = pr
        .reviews
        .iter()
        .filter(|r| r.author.login == me)
        .map(|r| r.submitted_at.as_str())
        .max()
        .unwrap_or("");
    let theirs = pr
        .reviews
        .iter()
        .filter(|r| r.author.login != me && !is_bot(&r.author.login))
        .map(|r| r.submitted_at.as_str())
        .max()
        .unwrap_or("");
    !mine.is_empty() && mine > theirs
}

fn mine_action(pr: &MinePr, me: &str) -> String {
    if pr.is_draft {
        return "draft".into();
    }
    let conflict = pr.mergeable == "CONFLICTING";
    match pr.review_decision.as_deref() {
        Some("CHANGES_REQUESTED") => {
            let base = if has_replied(pr, me) {
                "replied; await re-review"
            } else {
                "address changes"
            };
            format!("{base}{}", if conflict { " + rebase" } else { "" })
        }
        Some("APPROVED") => {
            if conflict {
                "rebase -> merge".into()
            } else if checks_of(&pr.status_check_rollup) == "fail" {
                "fix CI -> merge".into()
            } else {
                "MERGE".into()
            }
        }
        _ => format!("awaiting review{}", if conflict { "; rebase" } else { "" }),
    }
}

/// (my_vote, action) for a PR where I'm a reviewer.
fn reviewer_state(pr: &ReviewPr, me: &str) -> (String, String) {
    let vote = pr
        .latest_reviews
        .iter()
        .filter(|r| r.author.login == me)
        .map(|r| r.state.clone())
        .next_back()
        .unwrap_or_default();
    let requested = pr.review_requests.iter().any(|req| req.login == me);
    let vote_label = match vote.as_str() {
        "APPROVED" => "approved",
        "CHANGES_REQUESTED" => "changes",
        "COMMENTED" => "commented",
        _ => "-",
    }
    .to_string();
    let action = if requested {
        "REVIEW NEEDED"
    } else {
        match vote.as_str() {
            "APPROVED" => "done (approved)",
            "CHANGES_REQUESTED" => "awaiting author fixes",
            "COMMENTED" => "commented; decide",
            _ => "REVIEW NEEDED",
        }
    }
    .to_string();
    (vote_label, action)
}

// gh fetches --------------------------------------------------------------------

/// Resolve `owner/name`. Returns `repo` as-is when given, else asks `gh`.
pub fn resolve_repo(repo: Option<&str>, cwd: &str) -> Result<String> {
    if let Some(r) = repo {
        return Ok(r.to_string());
    }
    let info: RepoInfo = gh_json(&["repo", "view", "--json", "nameWithOwner"], cwd)?;
    Ok(info.name_with_owner)
}

fn fetch_mine(repo: Option<&str>, cwd: &str) -> Result<Vec<MinePr>> {
    let mut args: Vec<String> = vec!["pr".into(), "list".into()];
    if let Some(r) = repo {
        args.push("--repo".into());
        args.push(r.into());
    }
    for a in [
        "--author",
        "@me",
        "--state",
        "open",
        "--limit",
        "100",
        "--json",
        "number,url,headRefName,isDraft,reviewDecision,mergeable,statusCheckRollup,reviews",
    ] {
        args.push(a.into());
    }
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    gh_json(&refs, cwd)
}

fn fetch_reviews(repo: Option<&str>, me: &str, cwd: &str) -> Result<Vec<ReviewPr>> {
    let fields = "number,url,headRefName,author,latestReviews,reviewRequests";
    let mut seen: BTreeMap<u64, ReviewPr> = BTreeMap::new();
    for search in ["review-requested:@me", "reviewed-by:@me"] {
        let mut args: Vec<String> = vec!["pr".into(), "list".into()];
        if let Some(r) = repo {
            args.push("--repo".into());
            args.push(r.into());
        }
        for a in [
            "--state", "open", "--limit", "100", "--search", search, "--json", fields,
        ] {
            args.push(a.into());
        }
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let batch: Vec<ReviewPr> = gh_json(&refs, cwd)?;
        for pr in batch {
            seen.entry(pr.number).or_insert(pr);
        }
    }
    Ok(seen
        .into_values()
        .filter(|pr| pr.author.login != me)
        .collect())
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

/// Fetch and classify the caller's PRs. Neither flag set ⇒ both groups.
/// Stateless: no diff cache is read or written.
pub fn gather(root: &str, mine: bool, reviews: bool, repo: Option<&str>) -> Result<PrsReport> {
    let want_mine = mine || !reviews;
    let want_reviews = reviews || !mine;

    // Independent gh round-trips run concurrently; the reviews fetch needs the
    // current user, so it shares a thread with the user lookup.
    let (me, mine_prs, review_rows) = std::thread::scope(|s| {
        let user_reviews = s.spawn(|| -> Result<(String, Vec<ReviewPr>)> {
            let me: Me = gh_json(&["api", "user"], root)?;
            let me = me.login;
            let rows = if want_reviews {
                fetch_reviews(repo, &me, root)?
            } else {
                vec![]
            };
            Ok((me, rows))
        });
        let mine_thread = s.spawn(|| -> Result<Vec<MinePr>> {
            if want_mine {
                fetch_mine(repo, root)
            } else {
                Ok(vec![])
            }
        });
        let (me, review_rows) = user_reviews.join().expect("user/reviews thread panicked")?;
        let mine_prs = mine_thread.join().expect("mine thread panicked")?;
        Ok::<_, anyhow::Error>((me, mine_prs, review_rows))
    })?;

    let mine_views: Vec<MinePrView> = mine_prs
        .iter()
        .map(|pr| MinePrView {
            number: pr.number,
            url: pr.url.clone(),
            issue_id: issue_of(&pr.head_ref_name),
            review_state: review_text(pr).to_string(),
            check_state: checks_of(&pr.status_check_rollup).to_string(),
            action: mine_action(pr, &me),
        })
        .collect();

    let mut sorted: Vec<&ReviewPr> = review_rows.iter().collect();
    sorted.sort_by_key(|p| p.number);
    let review_views: Vec<ReviewPrView> = sorted
        .iter()
        .map(|pr| {
            let (my_vote, action) = reviewer_state(pr, &me);
            ReviewPrView {
                number: pr.number,
                url: pr.url.clone(),
                author: pr.author.login.clone(),
                my_vote,
                action,
            }
        })
        .collect();

    Ok(PrsReport {
        mine: mine_views,
        reviews: review_views,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mine(decision: Option<&str>, mergeable: &str, draft: bool, checks: Vec<Check>) -> MinePr {
        MinePr {
            number: 1,
            url: "u".into(),
            head_ref_name: "h".into(),
            is_draft: draft,
            review_decision: decision.map(String::from),
            mergeable: mergeable.into(),
            status_check_rollup: checks,
            reviews: vec![],
        }
    }
    fn check(conclusion: Option<&str>, status: Option<&str>) -> Check {
        Check {
            conclusion: conclusion.map(String::from),
            status: status.map(String::from),
            state: None,
        }
    }
    #[test]
    fn checks_fail_run_ok_empty() {
        assert_eq!(checks_of(&[]), "-");
        assert_eq!(
            checks_of(&[check(Some("SUCCESS"), Some("COMPLETED"))]),
            "ok"
        );
        assert_eq!(
            checks_of(&[check(Some("FAILURE"), Some("COMPLETED"))]),
            "fail"
        );
        assert_eq!(checks_of(&[check(None, Some("IN_PROGRESS"))]), "run");
    }
    #[test]
    fn approved_green_merges() {
        assert_eq!(
            mine_action(
                &mine(
                    Some("APPROVED"),
                    "MERGEABLE",
                    false,
                    vec![check(Some("SUCCESS"), None)]
                ),
                "me"
            ),
            "MERGE"
        );
    }
    #[test]
    fn approved_with_failing_ci() {
        assert_eq!(
            mine_action(
                &mine(
                    Some("APPROVED"),
                    "MERGEABLE",
                    false,
                    vec![check(Some("FAILURE"), None)]
                ),
                "me"
            ),
            "fix CI -> merge"
        );
    }
    #[test]
    fn changes_requested_action() {
        assert_eq!(
            mine_action(
                &mine(Some("CHANGES_REQUESTED"), "MERGEABLE", false, vec![]),
                "me"
            ),
            "address changes"
        );
    }
    #[test]
    fn draft_action() {
        assert_eq!(
            mine_action(&mine(None, "MERGEABLE", true, vec![]), "me"),
            "draft"
        );
    }
    #[test]
    fn review_text_variants() {
        assert_eq!(
            review_text(&mine(Some("APPROVED"), "x", false, vec![])),
            "approved"
        );
        assert_eq!(
            review_text(&mine(Some("CHANGES_REQUESTED"), "x", false, vec![])),
            "changes"
        );
        assert_eq!(review_text(&mine(None, "x", false, vec![])), "awaiting");
    }
    #[test]
    fn reviewer_state_requested_needs_review() {
        let pr = ReviewPr {
            number: 1,
            url: "u".into(),
            author: Author {
                login: "other".into(),
            },
            latest_reviews: vec![],
            review_requests: vec![ReviewRequest { login: "me".into() }],
        };
        let (vote, action) = reviewer_state(&pr, "me");
        assert_eq!(vote, "-");
        assert_eq!(action, "REVIEW NEEDED");
    }
    #[test]
    fn reviewer_state_approved_done() {
        let pr = ReviewPr {
            number: 1,
            url: "u".into(),
            author: Author {
                login: "other".into(),
            },
            latest_reviews: vec![Review {
                author: Author { login: "me".into() },
                state: "APPROVED".into(),
                submitted_at: "".into(),
            }],
            review_requests: vec![],
        };
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
