//! Finding the PR a run acts on, and recording it.
//!
//! Every `pr` command starts here, and so does `pr review request`:
//! push the branch, then resolve the PR from `--pr`, the worktree's record, or
//! the branch. Opening one is `create`'s job alone.

use std::path::Path;

use anyhow::{Context, Result};
use devkit_common::{
    forge::{self, Forge, HeadLookup, PrBrief, PrLocator, Repo, Repos},
    progress::Steps,
    vcs::{Vcs, VersionControl},
};

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

/// The single PR an acting path may operate on. Ambiguity is refused rather
/// than ranked: two forks proposing one branch name is the case that produces
/// two candidates, and picking one would act on a stranger's PR.
pub(crate) fn resolve_acting(l: &HeadLookup) -> Result<Option<PrBrief>> {
    match l {
        HeadLookup::Unique(p) => Ok(Some(p.clone())),
        HeadLookup::NoMatch => Ok(None),
        HeadLookup::Ambiguous(c) => {
            let list = c
                .iter()
                .map(|p| format!("#{} ({})", p.number, p.url))
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!("several PRs share this head branch: {list}; pass --pr to choose one")
        }
        HeadLookup::Unavailable(why) => {
            anyhow::bail!("could not look up the PR for this branch: {why}")
        }
    }
}

/// Explicit locator, then the record, then branch discovery. `--pr` means one
/// thing everywhere, use this PR for this run, and does not itself write
/// anything; `review request` recording what it acted on is what makes it a
/// rebind.
pub(crate) fn resolve_locator(
    explicit: Option<&PrLocator>,
    record: Option<&PrLocator>,
) -> Option<PrLocator> {
    explicit.or(record).cloned()
}

/// A PR entering an acting path must carry this worktree's commits. How it was
/// chosen does not change what it can do: a branch-discovered `Unique` is
/// unique only among one repository's PRs, so another fork's same-named branch
/// gives the identical answer.
///
/// `head_ref_oid` is the branch head the PR carries, not the commit that
/// landed on the base, so a squashed or rebased merge still compares equal.
pub(crate) fn assert_belongs(pr: &PrBrief, head: &str) -> Result<()> {
    anyhow::ensure!(
        pr.head_ref_oid == head,
        "PR #{} is at {} but this worktree is at {head}, so it does not carry this work",
        pr.number,
        pr.head_ref_oid
    );
    Ok(())
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
    assert_belongs(&created, head)
        .context("the PR just created does not carry this worktree's commits")
}

/// Point the worktree's record at its resolved PR, leaving its other fields
/// alone. A worktree with no record, as outside one `workspace setup` created,
/// gets none.
pub(crate) fn record_pr(worktree: &Path, loc: PrLocator) -> Result<()> {
    devkit_common::record::update(worktree, |rec| {
        if let Some(rec) = rec {
            rec.pr = Some(loc);
        }
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
    let resolved_loc = resolve_locator(args.explicit_pr.as_ref(), record_loc.as_ref());
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
                resolve_acting(&forge.pr_by_head(&repo, args.branch))
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
    fn record_pr_replaces_only_the_pr_field() {
        let wt = tempfile::tempdir().unwrap();
        let base = devkit_common::record::IssueRecord {
            issue: "ENG-1".into(),
            slug: "fix-login".into(),
            apps: vec!["web".into()],
            ..Default::default()
        };
        devkit_common::record::write(wt.path(), &base).unwrap();
        let loc = PrLocator {
            repo: Some("o/r".into()),
            number: 9,
        };
        record_pr(wt.path(), loc.clone()).unwrap();
        let got = devkit_common::record::read(wt.path()).unwrap();
        assert_eq!(got, devkit_common::record::IssueRecord {
            pr: Some(loc.clone()),
            ..base
        });

        let bare = tempfile::tempdir().unwrap();
        record_pr(bare.path(), loc).unwrap();
        assert!(devkit_common::record::read(bare.path()).is_none());
    }

    #[test]
    fn an_ambiguous_lookup_refuses_on_an_acting_path() {
        let err = resolve_acting(&HeadLookup::Ambiguous(vec![brief(7), brief(8)]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("#7") && err.contains("#8"), "{err}");
    }

    fn brief(n: u64) -> PrBrief {
        PrBrief {
            number: n,
            state: "OPEN".into(),
            url: format!("https://github.com/o/r/pull/{n}"),
            title: String::new(),
            head_ref_name: "feat/x".into(),
            head_ref_oid: "cafe1".into(),
            head_repo_owner: None,
            is_draft: false,
            author_login: None,
        }
    }

    fn loc(repo: Option<&str>, number: u64) -> PrLocator {
        PrLocator {
            repo: repo.map(str::to_string),
            number,
        }
    }

    fn brief_at(oid: &str) -> PrBrief {
        PrBrief {
            number: 5,
            head_ref_oid: oid.into(),
            ..brief(5)
        }
    }

    #[test]
    fn precedence_is_explicit_then_record_then_branch() {
        // review finish --pr wins over branch discovery by contract today.
        // Making the record unconditionally authoritative would either
        // disable that flag silently or leave an undocumented way
        // around the new rule.
        let ex = loc(None, 7);
        let rec = loc(Some("up/app"), 9);
        assert_eq!(resolve_locator(Some(&ex), Some(&rec)), Some(ex.clone()));
        assert_eq!(resolve_locator(None, Some(&rec)), Some(rec));
        assert_eq!(resolve_locator(None, None), None); // branch discovery
    }

    #[test]
    fn a_pr_that_is_not_this_worktrees_head_is_refused() {
        // --pr with a mistyped number names a real PR that resolves cleanly,
        // the record makes it authoritative, and its merge lets issue
        // end run `git branch -D` on a worktree whose work never
        // landed.
        let pr = brief_at("cafe1234");
        assert!(assert_belongs(&pr, "cafe1234").is_ok());
        let err = assert_belongs(&pr, "beef5678").unwrap_err().to_string();
        assert!(
            err.contains("cafe1234") && err.contains("beef5678"),
            "{err}"
        );
    }
}
