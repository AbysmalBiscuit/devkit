//! Finding the PR a run acts on, and recording it.
//!
//! Every `issue pr` command starts here, and so does `issue review request`:
//! push the branch, then resolve the PR from `--pr`, the worktree's record, or
//! the branch. Opening one is `create`'s job alone.

use anyhow::{Context, Result};
use devkit_common::cmd::gh_json_in;
use devkit_common::git::Git;
use devkit_common::github;
use devkit_common::progress::Steps;
use serde::Deserialize;
use std::path::Path;

use crate::issue::review::finish;

/// The flat shape `gh pr list --json` returns, converted into `github::PrBrief`
/// so the fallback path carries the same head oid the direct-HTTP path does.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrFlat {
    number: u64,
    state: String,
    url: String,
    head_ref_name: String,
    head_ref_oid: String,
    #[serde(default)]
    is_draft: bool,
}

/// The existing PR for head branch `branch`, over direct HTTP when possible
/// else `gh pr list`. `Ok(None)` means no PR.
fn existing_pr(branch: &str, cwd: &str, repo: &github::Repo) -> Result<Option<github::PrBrief>> {
    let looked = github::pr_by_head(repo, branch);
    if finish::decide_fallback(&looked) == finish::Fallback::No {
        return finish::resolve_acting(&looked);
    }
    let v: Vec<PrFlat> = gh_json_in(
        &[
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "all",
            "--json",
            "number,state,url,headRefName,headRefOid,isDraft",
        ],
        repo,
        cwd,
    )?;
    finish::ensure_unambiguous_gh_match(v.len())?;
    Ok(v.into_iter().next().map(|p| github::PrBrief {
        number: p.number,
        state: p.state,
        url: p.url,
        head_ref_name: p.head_ref_name,
        head_ref_oid: p.head_ref_oid,
        head_repo_owner: None,
        is_draft: p.is_draft,
    }))
}

/// `--pr <URL|number>`: a GitHub PR URL keeps its own repository; a bare
/// number means `pr_repo`.
pub(crate) fn parse_pr_flag(s: &str) -> Result<github::PrLocator> {
    if let Some(loc) = github::PrLocator::from_url(s) {
        return Ok(loc);
    }
    Ok(github::PrLocator {
        repo: None,
        number: s
            .trim()
            .parse()
            .with_context(|| format!("--pr is not a PR URL or number: {s}"))?,
    })
}

/// The repository this run acts on: the one a resolved locator names, else
/// `pr_repo`. The fetch the head-oid gate validates and the `gh pr edit` it
/// protects must read the same repository — gating one repository's PR while
/// editing another's is how a fork's same-numbered PR collects a stranger's
/// reviewers.
fn acting_repo(loc: Option<&github::PrLocator>, repos: &github::Repos) -> Result<github::Repo> {
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
            "fetching PR #{number}, recorded for this worktree — pass \
             `--pr <URL|number>` to bind it to a different PR"
        )
    } else {
        format!("fetching PR #{number}")
    }
}

/// A PR that was just created must carry this worktree's commits, which can
/// only be checked once it exists — so this runs after the call rather than
/// before it. A failure here leaves the PR open on GitHub, which is why the
/// caller says so in the error.
pub(crate) fn verify_created(repo: &github::Repo, number: u64, head: &str) -> Result<()> {
    let created = github::pr_meta_full(repo, number).context("fetching the PR just created")?;
    finish::assert_belongs(&created, head)
        .context("the PR just created does not carry this worktree's commits")
}

/// The record to write once a PR is resolved: the existing record with its
/// `pr` field replaced. `None` when there is no record to attach it to — a run
/// outside a worktree `issue setup` created.
pub(crate) fn record_with_pr(
    record: Option<&devkit_common::record::IssueRecord>,
    loc: github::PrLocator,
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
    pub repos: &'a github::Repos,
    pub record: Option<&'a devkit_common::record::IssueRecord>,
    pub explicit_pr: Option<github::PrLocator>,
    pub no_push: bool,
    pub steps: &'a Steps,
}

/// What resolution found. `pr` is `None` when this branch has no PR.
pub(crate) struct Found {
    pub pr: Option<github::PrBrief>,
    pub locator: Option<github::PrLocator>,
    pub repo: github::Repo,
}

/// Push the branch, then resolve this run's PR: explicit `--pr`, then the
/// record, then branch discovery. The repository a locator names wins over
/// `pr_repo`, so the fetch that the head-oid gate validates and the edit it
/// protects read the same repository.
pub(crate) fn resolve_existing(args: &Existing<'_>) -> Result<Found> {
    if !args.no_push {
        args.steps
            .during_result("Pushing branch…", || {
                Git::at(Path::new(args.start))
                    .args(["push", "-u", "origin", args.branch])
                    .timeout(devkit_common::git::SLOW_TIMEOUT)
                    .output()
            })
            .context("git push failed (refusing to force-push)")?;
    }

    let record_loc = args.record.and_then(|r| r.pr.clone());
    let resolved_loc = finish::resolve_locator(args.explicit_pr.as_ref(), record_loc.as_ref());
    let repo = acting_repo(resolved_loc.as_ref(), args.repos)?;

    match resolved_loc {
        Some(loc) => {
            let pr = args
                .steps
                .during_result(&format!("Fetching PR #{}…", loc.number), || {
                    github::pr_meta_full(&repo, loc.number)
                })
                .with_context(|| fetch_context(loc.number, args.explicit_pr.is_none()))?;
            Ok(Found {
                pr: Some(pr),
                locator: Some(loc),
                repo,
            })
        }
        None => {
            let pr = args.steps.during_result("Looking up existing PR…", || {
                existing_pr(args.branch, args.start, &repo)
            })?;
            let locator = pr.as_ref().map(|p| github::PrLocator {
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

    #[test]
    fn parse_pr_flag_keeps_a_urls_repository_but_not_a_bare_numbers() {
        let pasted = parse_pr_flag("https://github.com/o/r/pull/9").unwrap();
        assert_eq!(pasted.repo.as_deref(), Some("o/r"));
        assert_eq!(pasted.number, 9);

        let bare = parse_pr_flag("9").unwrap();
        assert_eq!(bare.repo, None);
        assert_eq!(bare.number, 9);

        assert!(parse_pr_flag("not-a-pr").is_err());
    }

    #[test]
    fn the_acting_repository_comes_from_the_locator_not_pr_repo() {
        let cfg = devkit_config::GithubConfig {
            issues_repo: None,
            pr_repo: Some("up/app".into()),
        };
        let repos = github::Repos::from_parts(&cfg, None, None);

        let pasted = parse_pr_flag("https://github.com/me/fork/pull/9").unwrap();
        assert_eq!(acting_repo(Some(&pasted), &repos).unwrap().slug, "me/fork");

        let bare = parse_pr_flag("9").unwrap();
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
        };
        let loc = github::PrLocator {
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
