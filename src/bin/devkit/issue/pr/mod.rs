use anyhow::{Result, bail};
use devkit_common::cmd::gh_json_in;
use devkit_common::github;
use serde::Deserialize;

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
    let any_human = existing
        .iter()
        .chain(added)
        .any(|l| crate::issue::review::is_human_login(l));
    if !any_human {
        bail!(
            "refusing to mark this PR ready with no human reviewer \
             (defaults.require_pr_reviewer is set) — pass --to, or add a \
             reviewer on GitHub"
        );
    }
    Ok(())
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

    #[test]
    fn a_bot_is_not_a_reviewer() {
        assert!(require_reviewer_for_ready(&["dependabot[bot]".into()], &[], true).is_err());
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
