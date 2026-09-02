use anyhow::{Context, Result, bail};
use devkit_common::cmd::{gh_capture, gh_json_in};
use devkit_common::github;
use devkit_common::progress::Steps;
use serde::Deserialize;

use crate::issue::review::{PrAction, action_for, is_human_login};

pub(crate) mod create;
pub(crate) mod ready;

#[derive(Deserialize)]
struct ReviewsView {
    reviews: Vec<SubmittedReview>,
}

/// One entry of `gh pr view --json reviews`. The REST API names the reviewer
/// `user`; `gh` names it `author`, and leaves it null for an account that no
/// longer exists.
#[derive(Deserialize)]
struct SubmittedReview {
    #[serde(default)]
    author: Option<ReviewAuthor>,
}

#[derive(Deserialize)]
struct ReviewAuthor {
    #[serde(default)]
    login: Option<String>,
}

/// The distinct logins behind a `gh pr view --json reviews` payload; one person
/// submitting several reviews is one reviewer.
fn gh_review_logins(view: ReviewsView) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in view.reviews {
        let Some(login) = r.author.and_then(|a| a.login) else {
            continue;
        };
        if !out.contains(&login) {
            out.push(login);
        }
    }
    out
}

/// Logins that have submitted a review on PR `pr`, over direct HTTP when a
/// token is available, else `gh pr view --json reviews`. A machine
/// authenticated only through `gh auth login` resolves no bearer token, and the
/// direct call errors without one.
fn submitted_reviewer_logins(pr: u64, cwd: &str, repo: &github::Repo) -> Result<Vec<String>> {
    if github::token().is_some()
        && let Ok(logins) = github::submitted_reviewers(&repo.slug, pr)
    {
        return Ok(logins);
    }
    let view: ReviewsView = gh_json_in(
        &["pr", "view", &pr.to_string(), "--json", "reviews"],
        repo,
        cwd,
    )?;
    Ok(gh_review_logins(view))
}

/// Every login already tied to PR `pr` as a reviewer: pending requests plus
/// anyone who has submitted a review. GitHub drops a login from the pending
/// list the moment they review, so a PR that collected an early look would
/// otherwise count nobody.
pub(crate) fn reviewer_logins_on(pr: u64, cwd: &str, repo: &github::Repo) -> Result<Vec<String>> {
    let mut out = crate::issue::review::request::requested_reviewer_logins(pr, cwd, repo)?;
    out.extend(submitted_reviewer_logins(pr, cwd, repo)?);
    Ok(out)
}

/// Marking a PR ready acts on one that exists. Opening one is `issue pr
/// create`'s job, so a branch with no PR is an error naming it rather than a
/// silent create.
pub(crate) fn require_existing_pr(pr_state: Option<&str>) -> Result<()> {
    match action_for(pr_state) {
        PrAction::Create => bail!(
            "no PR for this branch — run `issue pr create` first, \
             or pass --pr <URL|number>"
        ),
        PrAction::AddReviewer => Ok(()),
        PrAction::Stop(reason) => bail!("{reason}"),
    }
}

/// Refuse a run that would leave a PR ready for review with no human reviewer,
/// when `defaults.require_pr_reviewer` is set.
///
/// `existing` are the logins already on the PR and `added` the ones this run
/// requests. Both count: GitHub drops a reviewer from `reviewRequests` the
/// moment they review, so counting pending requests alone would refuse a PR
/// that has already been looked at.
pub(crate) fn require_reviewer_for_ready(
    existing: &[String],
    added: &[String],
    required: bool,
) -> Result<()> {
    if !required {
        return Ok(());
    }
    let any_human = existing.iter().chain(added).any(|l| is_human_login(l));
    if !any_human {
        bail!(
            "refusing to mark this PR ready with no human reviewer \
             (defaults.require_pr_reviewer is set) — pass --to, or add a \
             reviewer on GitHub"
        );
    }
    Ok(())
}

/// Request `logins` as reviewers on PR `number`. A run with none to add makes
/// no call at all, so an empty `--to` never touches the PR.
pub(crate) fn add_reviewers(
    number: u64,
    logins: &[String],
    repo: &github::Repo,
    cwd: &str,
    steps: &Steps,
) -> Result<()> {
    if logins.is_empty() {
        return Ok(());
    }
    let joined = logins.join(",");
    steps
        .during_result("Adding reviewers…", || {
            gh_capture(
                &["pr", "edit", &number.to_string(), "--add-reviewer", &joined],
                repo,
                cwd,
            )
        })
        .context("gh pr edit --add-reviewer failed")?;
    Ok(())
}

/// Whether the PR's own reviewers still decide the gate. What this run requests
/// can satisfy it on its own, and with the gate off nothing has to: either way
/// the lookup is two network round trips that change no answer.
fn needs_reviewer_lookup(added: &[String], required: bool) -> bool {
    require_reviewer_for_ready(&[], added, required).is_err()
}

/// Refuse a run about to make PR `number` ready for review with no human
/// reviewer. Call it only on the run that performs the flip: an already-ready
/// PR is not made ready by anything happening here.
pub(crate) fn gate_ready(
    number: u64,
    added: &[String],
    required: bool,
    repo: &github::Repo,
    cwd: &str,
    steps: &Steps,
) -> Result<()> {
    if !needs_reviewer_lookup(added, required) {
        return Ok(());
    }
    let already = steps.during_result("Resolving reviewers…", || {
        reviewer_logins_on(number, cwd, repo)
    })?;
    require_reviewer_for_ready(&already, added, required)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gate_is_off_unless_configured() {
        assert!(require_reviewer_for_ready(&[], &[], false).is_ok());
    }

    #[test]
    fn a_ready_pr_needs_a_human_reviewer() {
        let err = require_reviewer_for_ready(&[], &[], true).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("--to"), "names the way to satisfy it: {msg}");
    }

    #[test]
    fn an_existing_reviewer_satisfies_the_gate() {
        assert!(require_reviewer_for_ready(&["igoracc".into()], &[], true).is_ok());
    }

    #[test]
    fn a_reviewer_added_this_run_satisfies_the_gate() {
        assert!(require_reviewer_for_ready(&[], &["igoracc".into()], true).is_ok());
    }

    /// The PR's own reviewer list is fetched only when it can still change the
    /// verdict: with the gate off, or with a human already in `--to`, the two
    /// round trips decide nothing.
    #[test]
    fn the_reviewer_lookup_is_skipped_when_it_decides_nothing() {
        assert!(!needs_reviewer_lookup(&[], false));
        assert!(!needs_reviewer_lookup(&["igoracc".into()], false));
        assert!(!needs_reviewer_lookup(&["igoracc".into()], true));
        assert!(needs_reviewer_lookup(&[], true));
        assert!(needs_reviewer_lookup(&["dependabot[bot]".into()], true));
    }

    #[test]
    fn a_bot_is_not_a_reviewer() {
        assert!(require_reviewer_for_ready(&["dependabot[bot]".into()], &[], true).is_err());
    }

    #[test]
    fn no_pr_names_the_command_that_opens_one() {
        let msg = format!("{}", require_existing_pr(None).unwrap_err());
        assert!(
            msg.contains("issue pr create"),
            "names the way forward: {msg}"
        );
        assert!(require_existing_pr(Some("OPEN")).is_ok());
        assert!(require_existing_pr(Some("MERGED")).is_err());
    }

    /// `gh` reports the reviewer under `author`, not the REST API's `user`, and
    /// nulls it for an account that no longer exists.
    #[test]
    fn the_gh_fallback_reads_authors_and_dedupes() {
        let view: ReviewsView = serde_json::from_value(serde_json::json!({
            "reviews": [
                { "author": { "login": "igoracc" }, "state": "COMMENTED" },
                { "author": { "login": "igoracc" }, "state": "APPROVED" },
                { "author": null, "state": "APPROVED" }
            ]
        }))
        .expect("gh reviews payload");
        assert_eq!(gh_review_logins(view), vec!["igoracc".to_string()]);
    }
}
