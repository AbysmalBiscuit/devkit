//! Finding the PR a run acts on, and recording it.
//!
//! Every `issue pr` command starts here, and so does `issue review request`:
//! push the branch, then resolve the PR from `--pr`, the worktree's record, or
//! the branch. Opening one is `create`'s job alone.

use std::path::Path;

use anyhow::{Context, Result};
use devkit_common::{
    forge::{self, Forge, PrBrief, PrLocator, Repo, Repos},
    progress::Steps,
    vcs::{Vcs, VersionControl},
};

use crate::issue::review::finish;

/// `--pr <URL|number>`: a pasted PR URL on the project's forge keeps its own
/// repository; a bare number means the configured PR repository.
pub(crate) fn parse_pr_flag(s: &str, forge: &dyn Forge) -> Result<PrLocator> {
    if let Some(loc) = forge.locate(s) {
        return Ok(loc);
    }
    Ok(PrLocator {
        repo: None,
        number: s
            .trim()
            .parse()
            .with_context(|| format!("--pr is not a PR URL on this forge or a number: {s}"))?,
    })
}

/// The repository this run acts on: the one a resolved locator names, else
/// the PR repository. The fetch the head-oid gate validates and the edit it
/// protects must read the same repository: gating one repository's PR while
/// editing another's is how a fork's same-numbered PR collects a stranger's
/// reviewers.
fn acting_repo(loc: Option<&PrLocator>, repos: &Repos) -> Result<Repo> {
    match loc {
        Some(loc) => loc.resolve(repos),
        None => repos.prs().cloned(),
    }
}

/// Context for a PR fetch that failed. A locator that came from the record
/// names the flag that rebinds the worktree, since the record is not something
/// a user is expected to edit by hand; an explicit `--pr` already is that
/// escape hatch.
fn fetch_context(number: u64, from_record: bool) -> String {
    if from_record {
        format!(
            "fetching PR #{number}, recorded for this worktree: pass \
             `--pr <URL|number>` to bind it to a different PR"
        )
    } else {
        format!("fetching PR #{number}")
    }
}

/// PR `number` in `repo`, erroring when it does not exist.
pub(crate) fn existing(forge: &dyn Forge, repo: &Repo, number: u64) -> Result<PrBrief> {
    forge
        .pr(repo, number)?
        .with_context(|| format!("PR #{number} not found in {}", repo.slug))
}

/// A PR that was just created must carry this worktree's commits, which can
/// only be checked once it exists, so this runs after the call rather than
/// before it. A failure here leaves the PR open on the forge, which is why the
/// caller says so in the error.
pub(crate) fn verify_created(
    forge: &dyn Forge,
    repo: &Repo,
    number: u64,
    head: &str,
) -> Result<()> {
    let created = existing(forge, repo, number).context("fetching the PR just created")?;
    finish::assert_belongs(&created, head)
        .context("the PR just created does not carry this worktree's commits")
}

/// The record to write once a PR is resolved: the existing record with its
/// `pr` field replaced. `None` when there is no record to attach it to, as in
/// a run outside a worktree `issue setup` created.
pub(crate) fn record_with_pr(
    record: Option<&devkit_common::record::IssueRecord>,
    loc: PrLocator,
) -> Option<devkit_common::record::IssueRecord> {
    record.map(|r| devkit_common::record::IssueRecord {
        pr: Some(loc),
        ..r.clone()
    })
}

/// Inputs to the push-and-resolve half.
pub(crate) struct Existing<'a> {
    pub start: &'a str,
    pub branch: &'a str,
    pub forge: &'a forge::Resolved,
    pub record: Option<&'a devkit_common::record::IssueRecord>,
    pub explicit_pr: Option<PrLocator>,
    pub no_push: bool,
    pub steps: &'a Steps,
}

/// What resolution found. `pr` is `None` when this branch has no PR.
pub(crate) struct Found {
    pub pr: Option<PrBrief>,
    pub locator: Option<PrLocator>,
    pub repo: Repo,
}

/// Push the branch, then resolve this run's PR: explicit `--pr`, then the
/// record, then branch discovery. The repository a locator names wins over
/// the PR repository, so the fetch that the head-oid gate validates and the
/// edit it protects read the same repository.
pub(crate) fn resolve_existing(args: &Existing<'_>) -> Result<Found> {
    if !args.no_push {
        args.steps
            .during_result("Pushing branch...", || {
                let here = Path::new(args.start);
                Vcs::at(here).push(here, "origin", args.branch)
            })
            .context("git push failed (refusing to force-push)")?;
    }

    let forge = args.forge.forge.as_ref();
    let record_loc = args.record.and_then(|r| r.pr.clone());
    let resolved_loc = finish::resolve_locator(args.explicit_pr.as_ref(), record_loc.as_ref());
    let repo = acting_repo(resolved_loc.as_ref(), &args.forge.repos)?;

    match resolved_loc {
        Some(loc) => {
            let pr = args
                .steps
                .during_result(&format!("Fetching PR #{}...", loc.number), || {
                    existing(forge, &repo, loc.number)
                })
                .with_context(|| fetch_context(loc.number, args.explicit_pr.is_none()))?;
            Ok(Found {
                pr: Some(pr),
                locator: Some(loc),
                repo,
            })
        }
        None => {
            let pr = args.steps.during_result("Looking up existing PR...", || {
                finish::resolve_acting(&forge.pr_by_head(&repo, args.branch))
            })?;
            let locator = pr.as_ref().map(|p| PrLocator {
                repo: Some(repo.slug.clone()),
                number: p.number,
            });
            Ok(Found { pr, locator, repo })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github() -> forge::github::GithubForge {
        forge::github::GithubForge::new("github.com")
    }

    #[test]
    fn parse_pr_flag_keeps_a_urls_repository_but_not_a_bare_numbers() {
        let pasted = parse_pr_flag("https://github.com/o/r/pull/9", &github()).unwrap();
        assert_eq!(pasted.repo.as_deref(), Some("o/r"));
        assert_eq!(pasted.number, 9);

        let bare = parse_pr_flag("9", &github()).unwrap();
        assert_eq!(bare.repo, None);
        assert_eq!(bare.number, 9);

        assert!(parse_pr_flag("not-a-pr", &github()).is_err());
        assert!(
            parse_pr_flag("https://gitlab.com/o/r/-/merge_requests/9", &github()).is_err(),
            "a URL on another forge is not this project's PR"
        );
    }

    #[test]
    fn the_acting_repository_comes_from_the_locator_not_pr_repo() {
        let repos = Repos::from_parts(
            &devkit_config::GithubConfig::default(),
            &devkit_config::ForgeConfig {
                repo: Some("up/app".into()),
                ..Default::default()
            },
            None,
            None,
        );

        let pasted = parse_pr_flag("https://github.com/me/fork/pull/9", &github()).unwrap();
        assert_eq!(acting_repo(Some(&pasted), &repos).unwrap().slug, "me/fork");

        let bare = parse_pr_flag("9", &github()).unwrap();
        assert_eq!(acting_repo(Some(&bare), &repos).unwrap().slug, "up/app");

        assert_eq!(acting_repo(None, &repos).unwrap().slug, "up/app");
    }

    #[test]
    fn a_recorded_pr_that_will_not_resolve_names_the_rebind_flag() {
        let recorded = fetch_context(9, true);
        assert!(recorded.contains("--pr"), "{recorded}");
        assert!(recorded.contains('9'), "{recorded}");

        let explicit = fetch_context(9, false);
        assert!(!explicit.contains("--pr"), "{explicit}");
    }

    #[test]
    fn record_with_pr_replaces_only_the_pr_field() {
        let base = devkit_common::record::IssueRecord {
            issue: "ENG-1".into(),
            slug: "fix-login".into(),
            apps: vec!["web".into()],
            summary: None,
            pr: None,
            baseline: None,
        };
        let loc = PrLocator {
            repo: Some("o/r".into()),
            number: 9,
        };
        let got = record_with_pr(Some(&base), loc.clone()).expect("a record to update");
        assert_eq!(got.pr, Some(loc.clone()));
        assert_eq!(got.issue, base.issue);
        assert_eq!(got.slug, base.slug);
        assert_eq!(got.apps, base.apps);

        assert!(record_with_pr(None, loc).is_none());
    }
}
