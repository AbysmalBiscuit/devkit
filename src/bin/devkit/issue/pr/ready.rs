use anyhow::{Context, Result};
use devkit_common::cmd::gh_capture;
use devkit_common::git::Git;
use devkit_common::github;
use devkit_common::progress::Steps;
use std::path::Path;

use super::resolve::{Existing, parse_pr_flag, record_with_pr, resolve_existing};
use super::{add_reviewers, gate_ready, require_existing_pr, reviewer_logins};
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
    let (reviewers, warnings) = reviewer_logins(&explicit);
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
    add_reviewers(pr.number, &reviewers, &repo, &start, &steps)?;

    // `gh pr ready` on a PR that is already ready is a no-op, and the gate
    // guards the flip rather than the run, so a ready PR is neither judged nor
    // called about. Refusing before the flip leaves a draft a draft.
    if pr.is_draft {
        gate_ready(
            pr.number,
            &reviewers,
            loaded.config.defaults.require_pr_reviewer,
            pr.author_login.as_deref(),
            &repo,
            &start,
            &steps,
        )?;
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
