use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use devkit_common::{
    forge::{CheckState, Forge, OpenPr, OpenPrPage, Repo, ReviewDecision, ReviewState, Section},
    tracker::{Tracker, TrackerKind},
};
use serde::{Deserialize, Serialize};

/// The three open-PR searches the report is built from, plus whose they are.
struct Sections {
    viewer: String,
    mine: Vec<OpenPr>,
    review_requested: Vec<OpenPr>,
    reviewed_by: Vec<OpenPr>,
}

// pure logic
// --------------------------------------------------------------------

/// The issue ids a PR addresses, uppercased: zero or one from text. Taken
/// from the branch (head) ref, the convention for our own PRs, falling back
/// to the PR title, where other people's PRs carry the id (e.g.
/// `feat: ... [SWE-123]`). Linear-linked ids are merged in later by `gather`.
fn issue_ids_of(head: &str, title: &str) -> Vec<String> {
    devkit_common::worktree::find_id(head)
        .or_else(|| devkit_common::worktree::find_id(title))
        .map(|s| s.to_uppercase())
        .into_iter()
        .collect()
}

/// Merge Linear-linked ids into the text-derived ids: union, text id first,
/// deduped case-insensitively (text ids are uppercased; Linear identifiers
/// are canonical uppercase).
fn merge_linked(ids: &mut Vec<String>, linked: &[String]) {
    for id in linked {
        if !ids.iter().any(|have| have.eq_ignore_ascii_case(id)) {
            ids.push(id.clone());
        }
    }
}

/// Union the Linear-linked issue ids (url -> ids) into every view row.
fn apply_linked(report: &mut PrsReport, linked: &HashMap<String, Vec<String>>) {
    for pr in &mut report.mine {
        if let Some(ids) = linked.get(&pr.url) {
            merge_linked(&mut pr.issue_ids, ids);
        }
    }
    for pr in &mut report.reviews {
        if let Some(ids) = linked.get(&pr.url) {
            merge_linked(&mut pr.issue_ids, ids);
        }
    }
}

/// Case-insensitive glob match supporting `*` (any run) and `?` (any one char).
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut resume) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            resume = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            resume += 1;
            ti = resume;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn name_ignored(name: &str, ignored: &[String]) -> bool {
    ignored.iter().any(|pat| glob_match(pat, name))
}

/// The CHECK verdict for a PR, with `ignored` check-name globs discounted. When
/// the PR carries per-check runs, the verdict is recomputed from the
/// non-ignored checks so a single known-broken check (e.g. a deploy left red by
/// an unfinished PR) no longer fails the column; ignored failures are counted
/// so they can still be surfaced. Falls back to the forge's overall state when
/// no runs are present.
enum Checks {
    /// No checks at all.
    None,
    /// All non-ignored checks green; `masked` is the count of ignored checks
    /// that were themselves failing (so the green can be flagged as masking
    /// them).
    Ok { masked: usize },
    /// A non-ignored check is still running.
    Run,
    /// Non-ignored checks that failed, by name (empty when only the overall
    /// state was available).
    Fail(Vec<String>),
}

fn check_verdict(pr: &OpenPr, ignored: &[String]) -> Checks {
    let Some(checks) = &pr.checks else {
        return Checks::None;
    };
    if checks.runs.is_empty() {
        return match checks.overall {
            CheckState::Passed => Checks::Ok { masked: 0 },
            CheckState::Running => Checks::Run,
            CheckState::Failed => Checks::Fail(Vec::new()),
        };
    }
    // Collapse re-run attempts. A forge can return every attempt of a check,
    // so a stale cancelled or failed run lingers beside the latest green one
    // and would otherwise fail the column. Keep only the most recent attempt
    // per name (by `started_at`); checks with no timestamp are already unique
    // per name, so they pass through untouched.
    let mut latest: Vec<&devkit_common::forge::CheckRun> = Vec::new();
    for c in &checks.runs {
        match latest.iter_mut().find(|e| e.name == c.name) {
            Some(prev) if c.started_at > prev.started_at => *prev = c,
            Some(_) => {}
            None => latest.push(c),
        }
    }

    let mut failing = Vec::new();
    let mut running = false;
    let mut masked = 0;
    for c in latest {
        let ignored = name_ignored(&c.name, ignored);
        match c.state {
            CheckState::Failed if ignored => masked += 1,
            CheckState::Failed => failing.push(c.name.clone()),
            CheckState::Running if !ignored => running = true,
            CheckState::Running | CheckState::Passed => {}
        }
    }
    if !failing.is_empty() {
        Checks::Fail(failing)
    } else if running {
        Checks::Run
    } else {
        Checks::Ok { masked }
    }
}

/// Render a [`Checks`] verdict into the CHECK cell string.
fn checks_cell(c: &Checks) -> String {
    match c {
        Checks::None => "-".to_string(),
        Checks::Run => "run".to_string(),
        Checks::Ok { masked: 0 } => "ok".to_string(),
        Checks::Ok { masked } => format!("ok ({masked} ignored)"),
        Checks::Fail(names) if names.is_empty() => "fail".to_string(),
        Checks::Fail(names) => format!("fail: {}", names.join(", ")),
    }
}

/// True when someone is actually expected to review: the forge requires a
/// review before merge, or a reviewer sits in the pending request list
/// (including CODEOWNERS auto-requests). Submitted comment reviews don't
/// count, since bots comment on every PR and a comment obliges nobody.
/// Standing decisions are handled by `approved`/`changes_requested` before
/// callers consult this predicate.
fn review_in_flight(pr: &OpenPr) -> bool {
    pr.review_decision == Some(ReviewDecision::ReviewRequired) || !pr.review_requests.is_empty()
}

fn review_text(pr: &OpenPr) -> &'static str {
    if changes_requested(pr) {
        return "changes";
    }
    if approved(pr) {
        return "approved";
    }
    if review_in_flight(pr) {
        return "awaiting";
    }
    if pr.reviews.is_empty() {
        "not requested"
    } else {
        "commented"
    }
}

/// Whether a review is a decision that stands until the same reviewer makes
/// another: approval, a change request, or a dismissal.
fn is_decision(state: ReviewState) -> bool {
    matches!(
        state,
        ReviewState::Approved | ReviewState::ChangesRequested | ReviewState::Dismissed
    )
}

/// Per-author effective review state: each reviewer's most recent decision
/// wins, and comment or pending reviews leave a standing decision untouched.
/// Bots are included: any actor that votes counts. Mirrors GitHub's own
/// review-decision semantics.
fn effective_reviews(pr: &OpenPr) -> BTreeMap<&str, ReviewState> {
    let mut latest: BTreeMap<&str, (&str, ReviewState)> = BTreeMap::new();
    for r in pr.reviews.iter().filter(|r| is_decision(r.state)) {
        let slot = latest
            .entry(r.author.as_str())
            .or_insert(("", ReviewState::Pending));
        if r.submitted_at.as_str() >= slot.0 {
            *slot = (r.submitted_at.as_str(), r.state);
        }
    }
    latest
        .into_iter()
        .map(|(login, (_, state))| (login, state))
        .collect()
}

/// Logins whose current effective review is a change request.
fn change_requesters(pr: &OpenPr) -> Vec<&str> {
    effective_reviews(pr)
        .into_iter()
        .filter(|(_, state)| *state == ReviewState::ChangesRequested)
        .map(|(login, _)| login)
        .collect()
}

/// True when the PR carries a standing change request. Driven by the per-author
/// effective review state so any actor (human or bot, required or not) counts;
/// falls back to the forge's overall decision in case the review list was
/// truncated.
fn changes_requested(pr: &OpenPr) -> bool {
    !change_requesters(pr).is_empty()
        || pr.review_decision == Some(ReviewDecision::ChangesRequested)
}

/// True when the PR carries a standing approval and no standing change request.
/// The overall decision is empty when the repo requires no review, so an
/// approval on such a PR shows only in the per-author review state, derived
/// symmetrically to [`changes_requested`] so a non-required approval still
/// reads as approved.
fn approved(pr: &OpenPr) -> bool {
    if changes_requested(pr) {
        return false;
    }
    pr.review_decision == Some(ReviewDecision::Approved)
        || effective_reviews(pr)
            .values()
            .any(|s| *s == ReviewState::Approved)
}

/// True when a reviewer who requested changes is back in the pending
/// review-request list. A forge drops a reviewer from the pending list once
/// they submit a review, so their reappearance means re-review was requested
/// of them.
fn re_review_requested(pr: &OpenPr) -> bool {
    let requesters = change_requesters(pr);
    pr.review_requests
        .iter()
        .any(|login| requesters.contains(&login.as_str()))
}

fn mine_action(pr: &OpenPr, ignored: &[String]) -> String {
    if pr.is_draft {
        return "draft".into();
    }
    let conflict = pr.conflicting;
    if changes_requested(pr) {
        let base = if re_review_requested(pr) {
            "await re-review"
        } else {
            "address changes"
        };
        return format!("{base}{}", if conflict { " + rebase" } else { "" });
    }
    if approved(pr) {
        if conflict {
            "rebase -> merge".into()
        } else if matches!(check_verdict(pr, ignored), Checks::Fail(_)) {
            "fix CI -> merge".into()
        } else {
            "MERGE".into()
        }
    } else if review_in_flight(pr) {
        format!("awaiting review{}", if conflict { "; rebase" } else { "" })
    } else if conflict {
        "rebase -> merge (unreviewed)".into()
    } else if matches!(check_verdict(pr, ignored), Checks::Fail(_)) {
        "fix CI -> merge (unreviewed)".into()
    } else {
        "MERGE (unreviewed)".into()
    }
}

/// My effective review verdict on a PR. A comment never supersedes a standing
/// approval or change request: the latest *decision* review wins, mirroring
/// the `change_requesters` rule on the mine path. Only when there is no
/// standing decision does a comment count.
fn my_vote(pr: &OpenPr, me: &str) -> &'static str {
    let decision = pr
        .reviews
        .iter()
        .filter(|r| r.author == me && is_decision(r.state))
        .max_by(|a, b| a.submitted_at.cmp(&b.submitted_at))
        .map(|r| r.state);
    match decision {
        Some(ReviewState::Approved) => "APPROVED",
        Some(ReviewState::ChangesRequested) => "CHANGES_REQUESTED",
        // No standing decision (none, or a dismissed review): a comment still
        // prompts the reviewer to decide.
        _ if pr
            .reviews
            .iter()
            .any(|r| r.author == me && r.state == ReviewState::Commented) =>
        {
            "COMMENTED"
        }
        _ => "",
    }
}

/// (my_vote, action) for a PR where I'm a reviewer.
fn reviewer_state(pr: &OpenPr, me: &str) -> (String, String) {
    let vote = my_vote(pr, me);
    let vote_label = match vote {
        "APPROVED" => "approved",
        "CHANGES_REQUESTED" => "changes",
        "COMMENTED" => "commented",
        _ => "-",
    }
    .to_string();
    // A draft is the author's to finish, so it is passive for a reviewer even
    // while a review request sits on it.
    if pr.is_draft {
        return (vote_label, "draft".into());
    }
    let requested = pr.review_requests.iter().any(|login| login == me);
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

/// Pagination and retry knobs for the PR-search round trips.
#[derive(Clone, Copy, Debug)]
pub struct Fetch {
    /// PRs requested per search page. Smaller pages keep each request inside
    /// the forge's time budget.
    pub batch_size: u32,
    /// Extra attempts per page after a failure. Zero means fail on the first
    /// error.
    pub retries: u32,
}

impl Default for Fetch {
    fn default() -> Self {
        Self {
            batch_size: DEFAULT_BATCH_SIZE,
            retries: 0,
        }
    }
}

pub const DEFAULT_BATCH_SIZE: u32 = 25;

/// Turn the fetched sections into the report. Pure, so unit-tested. `ignored`
/// holds the check-name globs discounted from each PR's CHECK verdict.
fn classify(data: Sections, want_mine: bool, want_reviews: bool, ignored: &[String]) -> PrsReport {
    let me = data.viewer;

    let mine_views: Vec<MinePrView> = if want_mine {
        data.mine
            .iter()
            .filter(|pr| pr.number != 0)
            .map(|pr| MinePrView {
                number: pr.number,
                url: pr.url.clone(),
                issue_ids: issue_ids_of(&pr.head_ref_name, &pr.title),
                review_state: review_text(pr).to_string(),
                check_state: checks_cell(&check_verdict(pr, ignored)),
                action: mine_action(pr, ignored),
            })
            .collect()
    } else {
        Vec::new()
    };

    let review_views: Vec<ReviewPrView> = if want_reviews {
        let mut seen: BTreeMap<u64, OpenPr> = BTreeMap::new();
        for pr in data
            .review_requested
            .into_iter()
            .chain(data.reviewed_by)
            .filter(|pr| pr.number != 0 && pr.author != me)
        {
            seen.entry(pr.number).or_insert(pr);
        }
        seen.into_values()
            .map(|pr| {
                let (my_vote, action) = reviewer_state(&pr, &me);
                ReviewPrView {
                    number: pr.number,
                    url: pr.url.clone(),
                    issue_ids: issue_ids_of(&pr.head_ref_name, &pr.title),
                    author: pr.author.clone(),
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

// views + gather
// ----------------------------------------------------------------

/// Persisted in the CLI's pr-status snapshot cache; a new field needs
/// `#[serde(default)]` or old caches read as empty.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinePrView {
    pub number: u64,
    pub url: String,
    #[serde(default)]
    pub issue_ids: Vec<String>,
    pub review_state: String,
    pub check_state: String,
    pub action: String,
}

/// Persisted in the CLI's pr-status snapshot cache; a new field needs
/// `#[serde(default)]` or old caches read as empty.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPrView {
    pub number: u64,
    pub url: String,
    #[serde(default)]
    pub issue_ids: Vec<String>,
    pub author: String,
    pub my_vote: String,
    pub action: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrsReport {
    pub mine: Vec<MinePrView>,
    pub reviews: Vec<ReviewPrView>,
}

/// Backoff before retry `attempt` (1-based): 1s, 2s, 4s, then 8s for the rest.
fn backoff(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_secs(1 << attempt.saturating_sub(1).min(3))
}

/// One page, retried up to `retries` times. The last error is what surfaces.
fn fetch_page(
    forge: &dyn Forge,
    repo: &Repo,
    section: Section,
    f: Fetch,
    cursor: Option<&str>,
) -> Result<OpenPrPage> {
    let mut attempt = 0;
    loop {
        match forge.open_prs(repo, section, f.batch_size, cursor) {
            Ok(page) => return Ok(page),
            Err(_) if attempt < f.retries => {
                attempt += 1;
                std::thread::sleep(backoff(attempt));
            }
            Err(e) => return Err(e),
        }
    }
}

/// The viewer login plus every PR of one fully-paged section.
type SectionPrs = Result<(String, Vec<OpenPr>)>;

/// Follow page cursors until the forge reports no more, accumulating PRs.
/// `next` fetches one page for a given cursor; split from the transport so the
/// loop is unit-testable. Returns the viewer login alongside the PRs.
fn paginate(mut next: impl FnMut(Option<&str>) -> Result<OpenPrPage>) -> SectionPrs {
    let mut prs = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = next(cursor.as_deref())?;
        prs.extend(page.prs);
        match page.next {
            Some(c) => cursor = Some(c),
            None => return Ok((page.viewer, prs)),
        }
    }
}

/// Every open PR in one section, paged at `f.batch_size`.
fn fetch_section(forge: &dyn Forge, repo: &Repo, section: Section, f: Fetch) -> SectionPrs {
    paginate(|cursor| fetch_page(forge, repo, section, f, cursor))
}

/// Fetch and classify the caller's open PRs in `repo`. Neither flag set means
/// both groups. Stateless: no diff cache is read or written. `t`'s linked
/// issues are unioned into each row via [`apply_tracker_links`], which costs
/// one extra batched round trip on both tracker paths: opted into by
/// `resolve_pr_links` on Linear, unconditional on GitHub.
#[allow(clippy::too_many_arguments)]
pub fn gather(
    forge: &dyn Forge,
    repo: &Repo,
    mine: bool,
    reviews: bool,
    ignored_checks: &[String],
    resolve_pr_links: bool,
    fetch: Fetch,
    t: &dyn Tracker,
) -> Result<PrsReport> {
    let want_mine = mine || !reviews;
    let want_reviews = reviews || !mine;

    // Only the sections the report will render are fetched, and they run
    // concurrently: each paginates independently, so serialising them would
    // multiply the wall clock by the section count.
    let mut wanted = Vec::new();
    if want_mine {
        wanted.push(Section::Mine);
    }
    if want_reviews {
        wanted.push(Section::ReviewRequested);
        wanted.push(Section::ReviewedBy);
    }
    let fetched: Vec<(Section, SectionPrs)> = std::thread::scope(|s| {
        let handles: Vec<_> = wanted
            .iter()
            .map(|&sec| (sec, s.spawn(move || fetch_section(forge, repo, sec, fetch))))
            .collect();
        handles
            .into_iter()
            .map(|(sec, h)| {
                (
                    sec,
                    h.join()
                        .unwrap_or_else(|_| Err(anyhow::anyhow!("{sec:?} search thread panicked"))),
                )
            })
            .collect()
    });

    let mut data = Sections {
        viewer: String::new(),
        mine: Vec::new(),
        review_requested: Vec::new(),
        reviewed_by: Vec::new(),
    };
    for (sec, res) in fetched {
        let (login, prs) = res?;
        if data.viewer.is_empty() {
            data.viewer = login;
        }
        match sec {
            Section::Mine => data.mine = prs,
            Section::ReviewRequested => data.review_requested = prs,
            Section::ReviewedBy => data.reviewed_by = prs,
        }
    }

    let mut report = classify(data, want_mine, want_reviews, ignored_checks);
    apply_tracker_links(&mut report, t, resolve_pr_links);
    Ok(report)
}

/// Attach each PR's closing issues. `resolve_pr_links` gates Linear only: it is
/// a `[linear]` key and does not become a global switch. GitHub answers
/// unconditionally, at the cost of a batched round trip of its own.
pub(crate) fn apply_tracker_links(report: &mut PrsReport, t: &dyn Tracker, resolve_pr_links: bool) {
    let gated = match t.kind() {
        TrackerKind::Linear => !resolve_pr_links,
        TrackerKind::Github | TrackerKind::None => false,
    };
    if gated {
        return;
    }
    let urls: Vec<String> = report
        .mine
        .iter()
        .map(|pr| pr.url.clone())
        .chain(report.reviews.iter().map(|pr| pr.url.clone()))
        .collect();
    apply_linked(report, &t.issues_for_prs(&urls));
}

#[cfg(test)]
mod tests {
    use devkit_common::tracker::{TrackerKind, fake};

    use super::*;

    fn report_with_pr(url: &str) -> PrsReport {
        PrsReport {
            mine: vec![MinePrView {
                number: 1,
                url: url.to_string(),
                issue_ids: vec![],
                review_state: "-".into(),
                check_state: "-".into(),
                action: "-".into(),
            }],
            reviews: vec![],
        }
    }

    #[test]
    fn pr_rows_get_their_closing_issues_from_the_tracker() {
        // issues_for_prs had no caller anywhere: prs::gather called
        // linear::issues_for_prs directly, so a GitHub PR row's issue column
        // would simply stay empty.
        let t = fake::FakeTracker::new()
            .with_links("https://github.com/o/r/pull/7", vec!["ENG-1", "ENG-2"]);
        let mut report = report_with_pr("https://github.com/o/r/pull/7");
        apply_tracker_links(&mut report, &t, true);
        assert_eq!(report.mine[0].issue_ids, vec!["ENG-1", "ENG-2"]);
    }

    #[test]
    fn resolve_pr_links_still_gates_linear_only() {
        // The flag is a `[linear]` key and keeps its Linear meaning rather
        // than becoming a global switch. GitHub pays a batched round trip of
        // its own here, ungated.
        let lin = fake::FakeTracker::new()
            .with_kind(TrackerKind::Linear)
            .with_links("https://github.com/o/r/pull/7", vec!["ENG-1"]);
        let mut report = report_with_pr("https://github.com/o/r/pull/7");
        apply_tracker_links(&mut report, &lin, false);
        assert!(report.mine[0].issue_ids.is_empty());

        let gh = fake::FakeTracker::new()
            .with_kind(TrackerKind::Github)
            .with_links("https://github.com/o/r/pull/7", vec!["9"]);
        let mut report = report_with_pr("https://github.com/o/r/pull/7");
        apply_tracker_links(&mut report, &gh, false);
        assert_eq!(report.mine[0].issue_ids, vec!["9"]);
    }

    /// A GitHub GraphQL PR node, read through the GitHub forge's own parser,
    /// so the fixtures below stay in the shape GitHub answers with.
    fn node(json: serde_json::Value) -> OpenPr {
        devkit_common::forge::github::open_pr_from_node(&json).unwrap()
    }

    fn nodes(json: serde_json::Value) -> Vec<OpenPr> {
        json.as_array().unwrap().iter().cloned().map(node).collect()
    }

    // A representative response parses into the views with the same
    // classification the old per-`gh pr list` path produced.
    #[test]
    fn parses_and_classifies() {
        let data = Sections {
            viewer: "me".into(),
            mine: nodes(serde_json::json!([
              { "number": 10, "url": "u10", "headRefName": "lev/eng-1-foo",
                "isDraft": false, "reviewDecision": "APPROVED", "mergeable": "MERGEABLE",
                "author": {"login": "me"},
                "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "SUCCESS"}}}]},
                "reviews": {"nodes": [{"author": {"login": "alice"}, "state": "APPROVED", "submittedAt": "2026-06-20T10:00:00Z"}]},
                "reviewRequests": {"nodes": []} }
            ])),
            review_requested: nodes(serde_json::json!([
              { "number": 20, "url": "u20", "headRefName": "igork/ff-b01-thing",
                "title": "feat(api): flag-gate thing [SWE-2]",
                "isDraft": false, "reviewDecision": "REVIEW_REQUIRED", "mergeable": "MERGEABLE",
                "author": {"login": "bob"},
                "commits": {"nodes": []},
                "reviews": {"nodes": []},
                "reviewRequests": {"nodes": [{"requestedReviewer": {"login": "me"}}]} }
            ])),
            reviewed_by: Vec::new(),
        };
        let report = classify(data, true, true, &[]);
        assert_eq!(report.mine.len(), 1);
        assert_eq!(report.mine[0].number, 10);
        assert_eq!(report.mine[0].issue_ids, vec!["ENG-1"]);
        assert_eq!(report.mine[0].review_state, "approved");
        assert_eq!(report.mine[0].check_state, "ok");
        assert_eq!(report.mine[0].action, "MERGE");
        assert_eq!(report.reviews.len(), 1);
        assert_eq!(report.reviews[0].number, 20);
        assert_eq!(report.reviews[0].issue_ids, vec!["SWE-2"]);
        assert_eq!(report.reviews[0].my_vote, "-");
        assert_eq!(report.reviews[0].action, "REVIEW NEEDED");
    }

    fn page(numbers: &[u64], next: Option<&str>) -> OpenPrPage {
        OpenPrPage {
            viewer: "me".into(),
            prs: numbers
                .iter()
                .map(|n| node(serde_json::json!({ "number": n })))
                .collect(),
            next: next.map(str::to_string),
        }
    }

    // Every page is followed, and each request carries the previous page's
    // cursor. That is the whole point of paging: no PR is dropped past the
    // first page.
    #[test]
    fn paginate_follows_cursors_across_pages() {
        let mut seen_cursors: Vec<Option<String>> = Vec::new();
        let (login, prs) = paginate(|c| {
            seen_cursors.push(c.map(str::to_string));
            Ok(match c {
                None => page(&[1, 2], Some("c1")),
                Some("c1") => page(&[3, 4], Some("c2")),
                _ => page(&[5], None),
            })
        })
        .unwrap();
        assert_eq!(login, "me");
        let got: Vec<u64> = prs.iter().map(|n| n.number).collect();
        assert_eq!(got, vec![1, 2, 3, 4, 5]);
        assert_eq!(seen_cursors, vec![
            None,
            Some("c1".to_string()),
            Some("c2".to_string())
        ]);
    }

    // A page error aborts the whole section: a partial PR list would silently
    // under-report, which is worse than failing loudly.
    #[test]
    fn paginate_propagates_page_error() {
        let mut calls = 0;
        let res = paginate(|_| {
            calls += 1;
            if calls == 1 {
                Ok(page(&[1], Some("c1")))
            } else {
                Err(anyhow::anyhow!("HTTP 504"))
            }
        });
        assert!(res.is_err());
        assert_eq!(calls, 2);
    }

    #[test]
    fn backoff_grows_then_caps() {
        assert_eq!(backoff(1).as_secs(), 1);
        assert_eq!(backoff(2).as_secs(), 2);
        assert_eq!(backoff(3).as_secs(), 4);
        assert_eq!(backoff(4).as_secs(), 8);
        assert_eq!(backoff(9).as_secs(), 8);
    }

    // GitHub returns `submittedAt: null` for a PENDING review. The node must
    // still deserialize, with the timestamp treated as empty.
    #[test]
    fn pending_review_with_null_submitted_at_parses() {
        let pr = node(serde_json::json!({
            "number": 1, "url": "u", "headRefName": "h",
            "isDraft": false, "reviewDecision": null, "mergeable": "MERGEABLE",
            "author": {"login": "x"},
            "commits": {"nodes": []},
            "reviews": {"nodes": [
                {"author": {"login": "me"}, "state": "PENDING", "submittedAt": null}
            ]},
            "reviewRequests": {"nodes": []}
        }));
        assert_eq!(pr.reviews[0].submitted_at, "");
    }

    fn mine_node(
        decision: Option<&str>,
        mergeable: &str,
        draft: bool,
        rollup: Option<&str>,
    ) -> OpenPr {
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
        let cell = |rollup| checks_cell(&check_verdict(&mine_node(None, "x", false, rollup), &[]));
        assert_eq!(cell(None), "-");
        assert_eq!(cell(Some("SUCCESS")), "ok");
        assert_eq!(cell(Some("FAILURE")), "fail");
        assert_eq!(cell(Some("ERROR")), "fail");
        assert_eq!(cell(Some("PENDING")), "run");
        assert_eq!(cell(Some("EXPECTED")), "run");
    }

    #[test]
    fn glob_match_star_question_and_case() {
        assert!(glob_match("vercel*", "vercel-deploy"));
        assert!(glob_match("*preview*", "Vercel – Preview Comments")); // case-insensitive
        assert!(glob_match("ci/?", "ci/a"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("lint", "lint"));
        assert!(!glob_match("vercel*", "lint"));
        assert!(!glob_match("ci/?", "ci/ab"));
    }

    /// Build a PR node carrying explicit rollup contexts (CheckRun shape).
    fn pr_with_contexts(rollup: &str, contexts: serde_json::Value) -> OpenPr {
        node(serde_json::json!({
            "number": 1, "url": "u", "headRefName": "h", "isDraft": false,
            "reviewDecision": "APPROVED", "mergeable": "MERGEABLE", "author": {"login": "me"},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {
                "state": rollup, "contexts": {"nodes": contexts}
            }}}]},
            "reviews": {"nodes": []}, "reviewRequests": {"nodes": []}
        }))
    }

    // A rollup that GitHub reports as FAILURE, but whose only failing check is
    // ignored, reads green — flagged as masking one ignored failure — and the
    // PR becomes mergeable rather than "fix CI".
    #[test]
    fn ignored_check_masks_rollup_failure() {
        let pr = pr_with_contexts(
            "FAILURE",
            serde_json::json!([
                {"name": "lint", "status": "COMPLETED", "conclusion": "SUCCESS"},
                {"name": "vercel-deploy", "status": "COMPLETED", "conclusion": "FAILURE"}
            ]),
        );
        let ignored = vec!["vercel*".to_string()];
        assert_eq!(checks_cell(&check_verdict(&pr, &ignored)), "ok (1 ignored)");
        assert_eq!(mine_action(&pr, &ignored), "MERGE");
        // Without the ignore pattern the real failure stands, named.
        assert_eq!(checks_cell(&check_verdict(&pr, &[])), "fail: vercel-deploy");
        assert_eq!(mine_action(&pr, &[]), "fix CI -> merge");
    }

    // A genuine (non-ignored) failure is still reported by name even when an
    // ignored check is also red.
    #[test]
    fn real_failure_survives_ignore() {
        let pr = pr_with_contexts(
            "FAILURE",
            serde_json::json!([
                {"name": "test", "status": "COMPLETED", "conclusion": "FAILURE"},
                {"name": "vercel-deploy", "status": "COMPLETED", "conclusion": "FAILURE"}
            ]),
        );
        let ignored = vec!["vercel*".to_string()];
        assert_eq!(checks_cell(&check_verdict(&pr, &ignored)), "fail: test");
        assert_eq!(mine_action(&pr, &ignored), "fix CI -> merge");
    }

    // GitHub's rollup returns every attempt of a re-run check. A stale
    // CANCELLED attempt superseded by a later SUCCESS of the same name must
    // not poison the verdict: only the most recent attempt per name is
    // judged. Here the lone genuine failure is an ignored Vercel deploy, so
    // the PR reads mergeable.
    #[test]
    fn rerun_supersedes_cancelled_attempt() {
        let pr = pr_with_contexts(
            "FAILURE",
            serde_json::json!([
                {"name": "review gate", "status": "COMPLETED",
                 "conclusion": "CANCELLED", "startedAt": "2026-06-30T10:00:00Z"},
                {"name": "review gate", "status": "COMPLETED",
                 "conclusion": "SUCCESS", "startedAt": "2026-06-30T10:05:00Z"},
                {"context": "Vercel – hq", "state": "FAILURE"}
            ]),
        );
        let ignored = vec!["Vercel*hq".to_string()];
        assert_eq!(checks_cell(&check_verdict(&pr, &ignored)), "ok (1 ignored)");
        assert_eq!(mine_action(&pr, &ignored), "MERGE");
    }

    // The latest attempt wins even when it regresses: a check that passed and
    // was then re-run to failure reads red, not green.
    #[test]
    fn rerun_regression_reads_red() {
        let pr = pr_with_contexts(
            "FAILURE",
            serde_json::json!([
                {"name": "test", "status": "COMPLETED",
                 "conclusion": "SUCCESS", "startedAt": "2026-06-30T10:00:00Z"},
                {"name": "test", "status": "COMPLETED",
                 "conclusion": "FAILURE", "startedAt": "2026-06-30T10:05:00Z"}
            ]),
        );
        assert_eq!(checks_cell(&check_verdict(&pr, &[])), "fail: test");
    }

    // A StatusContext (external status) shape is classified by its `state`, and
    // a non-terminal CheckRun status reads as still running.
    #[test]
    fn status_context_and_running_check() {
        let pr = pr_with_contexts(
            "PENDING",
            serde_json::json!([
                {"context": "ci/build", "state": "SUCCESS"},
                {"name": "deploy", "status": "IN_PROGRESS", "conclusion": null}
            ]),
        );
        assert_eq!(checks_cell(&check_verdict(&pr, &[])), "run");
    }

    // With no per-check contexts the verdict falls back to the aggregate
    // rollup.
    #[test]
    fn check_verdict_falls_back_to_rollup() {
        let pr = mine_node(Some("APPROVED"), "MERGEABLE", false, Some("FAILURE"));
        assert_eq!(
            checks_cell(&check_verdict(&pr, &["vercel*".to_string()])),
            "fail"
        );
    }

    #[test]
    fn approved_green_merges() {
        assert_eq!(
            mine_action(
                &mine_node(Some("APPROVED"), "MERGEABLE", false, Some("SUCCESS")),
                &[]
            ),
            "MERGE"
        );
    }
    #[test]
    fn approved_with_failing_ci() {
        assert_eq!(
            mine_action(
                &mine_node(Some("APPROVED"), "MERGEABLE", false, Some("FAILURE")),
                &[]
            ),
            "fix CI -> merge"
        );
    }
    #[test]
    fn changes_requested_action() {
        assert_eq!(
            mine_action(
                &mine_node(Some("CHANGES_REQUESTED"), "MERGEABLE", false, None),
                &[]
            ),
            "address changes"
        );
    }
    #[test]
    fn draft_action() {
        assert_eq!(
            mine_action(&mine_node(None, "MERGEABLE", true, None), &[]),
            "draft"
        );
    }

    /// A node addressing a change request: the human's `CHANGES_REQUESTED`
    /// followed by my own `COMMENTED` replies (e.g. answering a bot's inline
    /// threads), with `requested` controlling whether the human is
    /// re-requested.
    fn change_request_node(requested: bool) -> OpenPr {
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
        assert_eq!(
            mine_action(&change_request_node(false), &[]),
            "address changes"
        );
    }

    // Once the change-requester is re-requested they are back in
    // reviewRequests, so the action flips to "await re-review".
    #[test]
    fn re_requested_awaits_re_review() {
        assert_eq!(
            mine_action(&change_request_node(true), &[]),
            "await re-review"
        );
    }

    /// Null-decision node (repo requires no reviews): `requested` controls the
    /// pending reviewRequests list, `reviews` the submitted review list.
    fn no_decision_node(
        mergeable: &str,
        rollup: &str,
        requested: bool,
        reviews: serde_json::Value,
    ) -> OpenPr {
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
        let pr = no_decision_node(
            "MERGEABLE",
            "SUCCESS",
            false,
            serde_json::json!({"nodes": []}),
        );
        assert_eq!(review_text(&pr), "not requested");
        assert_eq!(mine_action(&pr, &[]), "MERGE (unreviewed)");
    }

    // The unreviewed arm carries the same CI/conflict gating as the approved
    // arm, so "merge" is never claimed while something blocks it.
    #[test]
    fn unrequested_pr_with_failing_ci() {
        let pr = no_decision_node(
            "MERGEABLE",
            "FAILURE",
            false,
            serde_json::json!({"nodes": []}),
        );
        assert_eq!(mine_action(&pr, &[]), "fix CI -> merge (unreviewed)");
    }
    #[test]
    fn unrequested_pr_with_conflict() {
        let pr = no_decision_node(
            "CONFLICTING",
            "SUCCESS",
            false,
            serde_json::json!({"nodes": []}),
        );
        assert_eq!(mine_action(&pr, &[]), "rebase -> merge (unreviewed)");
    }

    // A pending review request means someone is expected to review: the wait
    // is real even though the repo requires no review.
    #[test]
    fn pending_request_still_awaits_review() {
        let pr = no_decision_node(
            "MERGEABLE",
            "SUCCESS",
            true,
            serde_json::json!({"nodes": []}),
        );
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

    // A change request from a non-required reviewer (or bot) that GitHub does
    // not surface in `reviewDecision` still shows as "changes" / "address
    // changes".
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
        assert_eq!(mine_action(&pr, &[]), "address changes");
    }

    // A later APPROVED clears a standing change request; a later COMMENTED does
    // not.
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
    // A PR approved when the repo requires no reviews: GitHub returns an empty
    // `reviewDecision`, so approval must be derived from the standing
    // per-author review. An earlier CHANGES_REQUESTED superseded by a later
    // APPROVED, plus a third party's COMMENTED reviews, still reads as
    // approved.
    #[test]
    fn empty_decision_with_standing_approval_reads_approved() {
        let pr = node(serde_json::json!({
            "number": 3348, "url": "u", "headRefName": "lev/swe-9898-foo", "isDraft": false,
            "reviewDecision": "", "mergeable": "MERGEABLE", "author": {"login": "me"},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "FAILURE"}}}]},
            "reviews": {"nodes": [
                {"author": {"login": "igor"}, "state": "CHANGES_REQUESTED", "submittedAt": "2026-06-23T12:01:19Z"},
                {"author": {"login": "biscuit"}, "state": "COMMENTED", "submittedAt": "2026-06-24T17:54:39Z"},
                {"author": {"login": "igor"}, "state": "APPROVED", "submittedAt": "2026-06-26T16:09:41Z"}
            ]},
            "reviewRequests": {"nodes": []}
        }));
        assert_eq!(review_text(&pr), "approved");
        assert_eq!(mine_action(&pr, &[]), "fix CI -> merge");
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
        assert_eq!(
            review_text(&mine_node(None, "x", false, None)),
            "not requested"
        );
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
    // A later COMMENTED reply (e.g. answering a thread) does not clear a
    // standing CHANGES_REQUESTED: the effective vote stays "changes" /
    // "awaiting author fixes".
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

    // A later APPROVED supersedes an earlier CHANGES_REQUESTED (real decision
    // change).
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
    fn a_draft_is_not_review_needed() {
        let pr = node(serde_json::json!({
            "number": 1, "url": "u", "headRefName": "h", "isDraft": true,
            "reviewDecision": null, "mergeable": "MERGEABLE",
            "author": { "login": "someone" },
            "reviewRequests": { "nodes": [
                { "requestedReviewer": { "login": "me" } }
            ] }
        }));
        let (_, action) = reviewer_state(&pr, "me");
        assert_eq!(action, "draft");
    }

    #[test]
    fn issue_ids_of_finds_swe() {
        assert_eq!(issue_ids_of("lev/swe-123-fix", ""), vec!["SWE-123"]);
        assert!(issue_ids_of("main", "").is_empty());
    }
    #[test]
    fn issue_ids_of_finds_non_swe_prefix() {
        assert_eq!(issue_ids_of("lev/eng-1234-fix", ""), vec!["ENG-1234"]);
        assert_eq!(issue_ids_of("feature/abc-9-thing", ""), vec!["ABC-9"]);
    }
    #[test]
    fn issue_ids_of_falls_back_to_title() {
        assert_eq!(
            issue_ids_of(
                "igork/ff-b01-thing",
                "feat(api): flag-gate thing [SWE-10412]"
            ),
            vec!["SWE-10412"]
        );
        assert_eq!(
            issue_ids_of("lev/eng-1-fix", "chore: touches SWE-999 too"),
            vec!["ENG-1"]
        );
        assert!(issue_ids_of("main", "no id anywhere").is_empty());
    }

    #[test]
    fn merge_linked_unions_and_dedups() {
        let mut ids = vec!["ENG-123".to_string()];
        merge_linked(&mut ids, &["eng-123".to_string(), "SWE-6".to_string()]);
        assert_eq!(ids, vec!["ENG-123", "SWE-6"]);
        let mut empty: Vec<String> = vec![];
        merge_linked(&mut empty, &["SWE-7".to_string()]);
        assert_eq!(empty, vec!["SWE-7"]);
        let mut untouched = vec!["ENG-1".to_string()];
        merge_linked(&mut untouched, &[]);
        assert_eq!(untouched, vec!["ENG-1"]);
    }

    #[test]
    fn apply_linked_hits_both_sections_by_url() {
        let mut report = PrsReport {
            mine: vec![MinePrView {
                number: 1,
                url: "u1".into(),
                issue_ids: vec!["ENG-1".into()],
                review_state: "-".into(),
                check_state: "ok".into(),
                action: "MERGE".into(),
            }],
            reviews: vec![ReviewPrView {
                number: 2,
                url: "u2".into(),
                issue_ids: vec![],
                author: "a".into(),
                my_vote: "-".into(),
                action: "REVIEW NEEDED".into(),
            }],
        };
        let linked = HashMap::from([
            ("u1".to_string(), vec![
                "ENG-1".to_string(),
                "SWE-6".to_string(),
            ]),
            ("u2".to_string(), vec!["SWE-7".to_string()]),
        ]);
        apply_linked(&mut report, &linked);
        assert_eq!(report.mine[0].issue_ids, vec!["ENG-1", "SWE-6"]);
        assert_eq!(report.reviews[0].issue_ids, vec!["SWE-7"]);
    }
}
