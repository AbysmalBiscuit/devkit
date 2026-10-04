use std::path::Path;

use anyhow::Result;
use devkit_common::{
    forge,
    progress::Steps,
    vcs::{Vcs, VersionControl},
};

use super::{
    Gate, add_reviewers, gate_ready, require_existing_pr,
    resolve::{Existing, assert_belongs, parse_pr_flag, record_pr, resolve_existing},
    reviewer_logins,
};
use crate::issue::review::{Target, guard_branch, resolve_target};

pub struct Args {
    pub to: Vec<String>,
    pub no_push: bool,
    /// Use this PR for this run: a PR URL keeps its own repository, a
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
    let forge = forge::resolve(&loaded.config.forge, &loaded.config.github, &start, None);

    let here = Path::new(&start);
    let vcs = Vcs::at(here);
    let branch = vcs.branch(here)?;
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

    let toplevel = devkit_common::vcs::checkout_root(Path::new(&start))?;
    let record = devkit_common::record::read(&toplevel);

    let head = vcs.revision(here)?;

    let steps = Steps::persistent();
    let found = resolve_existing(&Existing {
        start: &start,
        branch: &branch,
        forge: &forge,
        record: record.as_ref(),
        explicit_pr: args
            .pr
            .as_deref()
            .map(|s| parse_pr_flag(s, forge.forge.as_ref()))
            .transpose()?,
        no_push: args.no_push,
        steps: &steps,
    })?;
    require_existing_pr(found.pr.as_ref().map(|p| p.state.as_str()))?;
    let pr = found.pr.expect("require_existing_pr rejects a missing PR");
    let locator = found.locator.expect("a resolved PR carries a locator");
    let repo = found.repo;
    // Every mutation below is gated on the PR carrying this worktree's commits.
    assert_belongs(&pr, &head)?;

    // Recorded before the flip, not after: the binding is true the moment the
    // PR is resolved and verified, and a run that dies mid-flight then leaves
    // the record naming a PR that exists rather than the forge holding a state
    // change nothing local knows about.
    if record.is_some() {
        record_pr(&toplevel, locator)?;
    }

    let f = forge.forge.as_ref();
    add_reviewers(f, &repo, pr.number, &reviewers, &steps)?;

    // The gate guards the flip rather than the run, so a ready PR is neither
    // judged nor called about. Refusing before the flip leaves a draft a draft.
    if pr.is_draft {
        gate_ready(
            f,
            &repo,
            pr.number,
            &Gate {
                added: &reviewers,
                required: loaded.config.defaults.require_pr_reviewer,
                author: pr.author_login.as_deref(),
            },
            &steps,
        )?;
        steps.during_result("Marking ready for review...", || {
            f.mark_ready(&repo, pr.number)
        })?;
    } else {
        eprintln!("PR #{} is already ready for review.", pr.number);
    }

    println!("{}", pr.url);
    Ok(())
}
