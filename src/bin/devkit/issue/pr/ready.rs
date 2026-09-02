use anyhow::{Context, Result};
use devkit_common::cmd::gh_capture;
use devkit_common::git::Git;
use devkit_common::github;
use devkit_common::progress::Steps;
use std::path::Path;

use super::create::{Existing, parse_pr_flag, record_with_pr, resolve_existing};
use super::{require_existing_pr, require_reviewer_for_ready, reviewer_logins_on};
use crate::issue::review::{self, Target, guard_branch, resolve_target};

pub struct Args {
    pub to: Vec<String>,
    pub no_push: bool,
    /// Use this PR for this run: a GitHub PR URL keeps its own repository, a
    /// bare number means `pr_repo`.
    pub pr: Option<String>,
    pub dir: Option<String>,
    pub config: Option<String>,
}

/// Whether this run would change the PR: flip a draft, or hand it reviewers it
/// does not have. The reviewer gate guards that transition, not the ready
/// state, so an already-ready PR with no `--to` is left alone rather than
/// looked up and judged.
fn changes_the_pr(is_draft: bool, added: &[String]) -> bool {
    is_draft || !added.is_empty()
}

pub fn run(args: Args) -> Result<()> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());
    let loaded =
        devkit_ports::load::load(args.config.as_deref().map(Path::new), Path::new(&start))?;
    let people = &loaded.config.people;
    let repos = github::Repos::resolve(&loaded.config.github, &start, None);

    let branch = devkit_common::git::branch(Path::new(&start))?;
    guard_branch(&branch)?;

    let explicit: Vec<Target> = args
        .to
        .iter()
        .map(|v| resolve_target(v, people))
        .collect::<Result<_>>()?;
    let (reviewers, warnings) = review::request::reviewer_logins(&explicit);
    for w in &warnings {
        eprintln!("warning: {w}");
    }

    let toplevel = devkit_common::git::checkout_root(Path::new(&start))?;
    let record = devkit_common::record::read(&toplevel);

    let head = Git::at(Path::new(&start))
        .args(["rev-parse", "HEAD"])
        .output()?
        .trim()
        .to_string();

    let steps = Steps::persistent();
    let found = resolve_existing(&Existing {
        start: &start,
        branch: &branch,
        repos: &repos,
        record: record.as_ref(),
        explicit_pr: args.pr.as_deref().map(parse_pr_flag).transpose()?,
        no_push: args.no_push,
        steps: &steps,
    })?;
    require_existing_pr(found.pr.as_ref().map(|p| p.state.as_str()))?;
    let pr = found.pr.expect("require_existing_pr rejects a missing PR");
    let locator = found.locator.expect("a resolved PR carries a locator");
    let repo = found.repo;
    // Every mutation below is gated on the PR carrying this worktree's commits.
    review::finish::assert_belongs(&pr, &head)?;

    let number = pr.number.to_string();
    if !reviewers.is_empty() {
        let joined = reviewers.join(",");
        steps
            .during_result("Adding reviewers…", || {
                gh_capture(
                    &["pr", "edit", &number, "--add-reviewer", &joined],
                    &repo,
                    &start,
                )
            })
            .context("gh pr edit --add-reviewer failed")?;
    }

    if changes_the_pr(pr.is_draft, &reviewers) {
        let required = loaded.config.defaults.require_pr_reviewer;
        // Two network round trips that decide nothing with the gate off.
        let already = if required {
            steps.during_result("Resolving reviewers…", || {
                reviewer_logins_on(pr.number, &start, &repo)
            })?
        } else {
            Vec::new()
        };
        // Refusing before the flip leaves the PR a draft.
        require_reviewer_for_ready(&already, &reviewers, required)?;
    }

    // `gh pr ready` on a PR that is already ready is a no-op; skipping the call
    // keeps the run quiet.
    if pr.is_draft {
        steps
            .during_result("Marking ready for review…", || {
                gh_capture(&["pr", "ready", &number], &repo, &start)
            })
            .context("gh pr ready failed")?;
    } else {
        eprintln!("PR #{number} is already ready for review.");
    }

    if let Some(rec) = record_with_pr(record.as_ref(), locator) {
        devkit_common::record::write(&toplevel, &rec)?;
    }

    println!("{}", pr.url);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An already-ready PR with nothing to add changes nothing, so the run must
    /// not spend a reviewer lookup on it and must not be refused by a gate
    /// guarding a transition it is not making.
    #[test]
    fn an_already_ready_pr_with_nothing_to_add_is_left_alone() {
        assert!(!changes_the_pr(/* is_draft */ false, &[]));
        assert!(changes_the_pr(true, &[]));
        assert!(changes_the_pr(false, &["igoracc".to_string()]));
        assert!(changes_the_pr(true, &["igoracc".to_string()]));
    }
}
