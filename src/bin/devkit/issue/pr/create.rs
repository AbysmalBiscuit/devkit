use anyhow::{Context, Result, bail};
use devkit_common::cmd::gh_capture;
use devkit_common::git::Git;
use devkit_common::github;
use devkit_common::progress::Steps;
use devkit_config::PrCreateState;
use std::path::Path;

use super::resolve::{Existing, parse_pr_flag, record_with_pr, resolve_existing, verify_created};
use super::{add_reviewers, require_reviewer_for_ready, reviewer_logins};
use crate::issue::review::{
    PrAction, Target, action_for, base_ctx, finish, guard_branch, parse_args, render_review,
    resolve_target, with_fields,
};

pub struct Args {
    pub draft: bool,
    pub ready: bool,
    pub to: Vec<String>,
    pub base: Option<String>,
    pub pr_title: Option<String>,
    pub pr_body: Option<String>,
    pub no_push: bool,
    /// Use this PR for this run: a GitHub PR URL keeps its own repository, a
    /// bare number means `pr_repo`. Replaces a wrong recorded binding, since
    /// recording what this run acts on is what makes it a rebind.
    pub pr: Option<String>,
    pub args: Vec<String>,
    pub dir: Option<String>,
    pub config: Option<String>,
}

/// The state a create should use: an explicit flag, else the configured
/// default. Clap makes the two flags mutually exclusive, so both being set is
/// unreachable.
fn wanted_state(draft: bool, ready: bool, configured: PrCreateState) -> PrCreateState {
    match (draft, ready) {
        (true, _) => PrCreateState::Draft,
        (_, true) => PrCreateState::Ready,
        (false, false) => configured,
    }
}

/// What to print when a run reused a PR whose draft state contradicts an
/// explicit flag. `create` never flips an existing PR, so saying nothing would
/// leave the user believing the flag applied.
fn reuse_note(number: u64, pr_is_draft: bool, asked: Option<PrCreateState>) -> Option<String> {
    let asked = asked?;
    let matches = match asked {
        PrCreateState::Draft => pr_is_draft,
        PrCreateState::Ready => !pr_is_draft,
    };
    if matches {
        return None;
    }
    let (is, flag, way_back) = if pr_is_draft {
        ("a draft", "--ready", "issue pr ready")
    } else {
        ("ready for review", "--draft", "gh pr ready --undo")
    };
    Some(format!(
        "PR #{number} already exists and is {is}.\n\
         {flag} was ignored. To move it: {way_back}"
    ))
}

/// Reject an empty rendered PR title. Only a run that opens a PR needs one.
fn require_pr_title(title: &str) -> Result<()> {
    if title.trim().is_empty() {
        bail!("--pr-title is required to create a PR");
    }
    Ok(())
}

/// The PR this run acts on, created or reused.
pub(crate) struct Resolved {
    pub url: String,
    pub locator: github::PrLocator,
}

/// Renders the PR body on demand.
type RenderBody<'a> = Box<dyn FnOnce() -> Result<String> + 'a>;

pub(crate) struct Ensure<'a> {
    pub existing: Existing<'a>,
    pub head: &'a str,
    pub state: PrCreateState,
    /// The explicit `--draft` / `--ready`, or `None` when neither was passed.
    /// `reuse_note` reports only a flag the user actually typed.
    pub asked: Option<PrCreateState>,
    pub base: String,
    pub pr_title: String,
    /// Deferred rather than rendered: a `pr_body` template reading `{{ issue }}`
    /// cannot be rendered outside a worktree `issue setup` created, and a run
    /// that only reuses a PR never needs a body.
    pub pr_body: RenderBody<'a>,
    /// GitHub logins to request as reviewers.
    pub reviewers: Vec<String>,
    /// `defaults.require_pr_reviewer`: whether opening a PR ready for review
    /// demands a human reviewer.
    pub require_reviewer: bool,
    pub steps: &'a Steps,
}

/// The body to open a PR with, rendered only when this run is about to open
/// one. Reuse leaves the template untouched, so a body that needs the issue
/// record cannot fail a run that has no use for it.
fn body_for(action: &PrAction, render: RenderBody<'_>) -> Result<Option<String>> {
    match action {
        PrAction::Create => render().map(Some),
        PrAction::AddReviewer | PrAction::Stop(_) => Ok(None),
    }
}

/// Resolve this branch's PR and, when there is none, open one. A reused PR
/// keeps the draft state it already has.
pub(crate) fn ensure(args: Ensure<'_>) -> Result<Resolved> {
    let found = resolve_existing(&args.existing)?;
    let start = args.existing.start;
    let steps = args.steps;
    let joined = args.reviewers.join(",");

    let action = action_for(found.pr.as_ref().map(|p| p.state.as_str()));
    let pr_body = body_for(&action, args.pr_body)?;

    let resolved = match action {
        PrAction::Stop(reason) => bail!("{reason}"),
        PrAction::AddReviewer => {
            let pr = found.pr.expect("AddReviewer implies an existing PR");
            let locator = found
                .locator
                .expect("AddReviewer implies a resolved locator");
            // Mutating an existing PR is gated before the call: a mismatch here
            // is refused before a single reviewer is added.
            finish::assert_belongs(&pr, args.head)?;
            add_reviewers(pr.number, &args.reviewers, &found.repo, start, steps)?;
            if let Some(note) = reuse_note(pr.number, pr.is_draft, args.asked) {
                eprintln!("{note}");
            }
            Resolved {
                url: pr.url,
                locator,
            }
        }
        PrAction::Create => {
            require_pr_title(&args.pr_title)?;
            // A draft is not gated: an unreviewed draft is not a violation.
            match args.state {
                PrCreateState::Ready => {
                    require_reviewer_for_ready(&[], &args.reviewers, args.require_reviewer)?;
                }
                PrCreateState::Draft => {}
            }
            let pr_body = pr_body.expect("Create implies a rendered body");
            let mut gh_args = vec![
                "pr",
                "create",
                "--base",
                &args.base,
                "--title",
                &args.pr_title,
                "--body",
                &pr_body,
            ];
            if !args.reviewers.is_empty() {
                gh_args.push("--reviewer");
                gh_args.push(&joined);
            }
            match args.state {
                PrCreateState::Draft => gh_args.push("--draft"),
                PrCreateState::Ready => {}
            }
            let out = steps
                .during_result("Creating PR…", || {
                    gh_capture(&gh_args, &found.repo, start)
                })
                .context("gh pr create failed")?;
            let url = out
                .lines()
                .rev()
                .find(|l| l.contains("://"))
                .context("could not parse a PR URL from `gh pr create` output")?
                .trim()
                .to_string();
            let locator = github::PrLocator::from_url(&url)
                .context("could not parse a PR number from `gh pr create` output")?;
            let created_repo = locator.resolve(args.existing.repos)?;
            // The gate runs before the record is written and before any
            // notification goes out.
            verify_created(&created_repo, locator.number, args.head)
                .with_context(|| format!("{url} is open with nothing recorded"))?;
            Resolved { url, locator }
        }
    };

    if let Some(rec) = record_with_pr(args.existing.record, resolved.locator.clone()) {
        let toplevel = devkit_common::git::checkout_root(Path::new(start))?;
        devkit_common::record::write(&toplevel, &rec)?;
    }
    Ok(resolved)
}

pub fn run(args: Args) -> Result<()> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());
    let loaded =
        devkit_ports::load::load(args.config.as_deref().map(Path::new), Path::new(&start))?;
    let people = &loaded.config.people;
    let tmpls = &loaded.config.templates;
    let repos = github::Repos::resolve(&loaded.config.github, &start, None);

    let mut vars = tmpls.variables.clone();
    vars.extend(parse_args(&args.args, &tmpls.variables)?);

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

    let toplevel = devkit_common::git::checkout_root(Path::new(&start))?
        .to_string_lossy()
        .into_owned();
    let record = devkit_common::record::read(Path::new(&toplevel));
    let missing_at = if record.is_none() {
        Some(toplevel.as_str())
    } else {
        None
    };

    let ctx = base_ctx(record.as_ref(), &branch);
    let pr_title = render_review(
        tmpls.pr_title(),
        "pr_title",
        &with_fields(
            &ctx,
            &[(
                "input",
                serde_json::json!(args.pr_title.clone().unwrap_or_default()),
            )],
        ),
        &vars,
        missing_at,
    )?;
    let body_ctx = with_fields(
        &ctx,
        &[
            (
                "input",
                serde_json::json!(args.pr_body.clone().unwrap_or_default()),
            ),
            ("pr_title", serde_json::json!(pr_title)),
        ],
    );

    let head = Git::at(Path::new(&start))
        .args(["rev-parse", "HEAD"])
        .output()?
        .trim()
        .to_string();

    // Only a flag the user typed can be reported as ignored; deriving this from
    // the resolved state would warn on every reuse under the default config.
    let asked = if args.draft {
        Some(PrCreateState::Draft)
    } else if args.ready {
        Some(PrCreateState::Ready)
    } else {
        None
    };

    let steps = Steps::persistent();
    let resolved = ensure(Ensure {
        existing: Existing {
            start: &start,
            branch: &branch,
            repos: &repos,
            record: record.as_ref(),
            explicit_pr: args.pr.as_deref().map(parse_pr_flag).transpose()?,
            no_push: args.no_push,
            steps: &steps,
        },
        head: &head,
        state: wanted_state(
            args.draft,
            args.ready,
            loaded.config.defaults.pr_create_state,
        ),
        asked,
        base: args
            .base
            .clone()
            .unwrap_or_else(|| loaded.config.defaults.pr_base.clone()),
        pr_title,
        pr_body: Box::new(|| {
            render_review(tmpls.pr_body(), "pr_body", &body_ctx, &vars, missing_at)
        }),
        reviewers,
        require_reviewer: loaded.config.defaults.require_pr_reviewer,
        steps: &steps,
    })?;

    println!("{}", resolved.url);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_flag_takes_the_configured_state() {
        assert_eq!(
            wanted_state(false, false, PrCreateState::Draft),
            PrCreateState::Draft
        );
        assert_eq!(
            wanted_state(false, false, PrCreateState::Ready),
            PrCreateState::Ready
        );
    }

    #[test]
    fn an_explicit_flag_beats_the_config() {
        assert_eq!(
            wanted_state(true, false, PrCreateState::Ready),
            PrCreateState::Draft
        );
        assert_eq!(
            wanted_state(false, true, PrCreateState::Draft),
            PrCreateState::Ready
        );
    }

    #[test]
    fn require_pr_title_rejects_empty() {
        assert!(require_pr_title("  ").is_err());
        assert!(require_pr_title("Fix login").is_ok());
    }

    #[test]
    fn reuse_reports_a_state_flag_it_did_not_apply() {
        let note = reuse_note(
            123,
            /* pr_is_draft */ false,
            Some(PrCreateState::Draft),
        );
        let note = note.expect("a contradicted flag is reported");
        assert!(note.contains("#123"), "names the PR: {note}");
        assert!(
            note.contains("gh pr ready --undo"),
            "names the way out: {note}"
        );
    }

    #[test]
    fn reuse_says_nothing_when_the_state_already_matches() {
        assert!(reuse_note(123, false, Some(PrCreateState::Ready)).is_none());
        assert!(reuse_note(123, true, Some(PrCreateState::Draft)).is_none());
        assert!(reuse_note(123, true, None).is_none());
    }

    /// The hazard the deferred body exists for: `render_review` is strict about
    /// undefined variables, and `base_ctx` binds `issue` only when the worktree
    /// has an `issue setup` record.
    #[test]
    fn a_body_template_reading_the_record_fails_without_one() {
        let ctx = base_ctx(None, "lev/eng-1-fix");
        let vars = std::collections::BTreeMap::new();
        assert!(render_review("Closes {{ issue }}", "pr_body", &ctx, &vars, None).is_err());

        let record = devkit_common::record::IssueRecord {
            issue: "ENG-1".into(),
            slug: "fix-login".into(),
            apps: Vec::new(),
            summary: None,
            pr: None,
        };
        let ctx = base_ctx(Some(&record), "lev/eng-1-fix");
        let out = render_review("Closes {{ issue }}", "pr_body", &ctx, &vars, None).unwrap();
        assert_eq!(out, "Closes ENG-1");
    }

    #[test]
    fn a_reused_pr_never_renders_the_body() {
        for action in [
            PrAction::AddReviewer,
            PrAction::Stop("already merged".into()),
        ] {
            let rendered = std::cell::Cell::new(false);
            let body = body_for(
                &action,
                Box::new(|| {
                    rendered.set(true);
                    bail!("`pr_body` reads the issue record")
                }),
            )
            .expect("reuse must not fail on a body it has no use for");
            assert!(body.is_none());
            assert!(!rendered.get(), "the body template must not be rendered");
        }
    }

    #[test]
    fn a_created_pr_renders_the_body() {
        let body = body_for(&PrAction::Create, Box::new(|| Ok("Closes ENG-1".into())))
            .expect("a create renders its body");
        assert_eq!(body.as_deref(), Some("Closes ENG-1"));
    }
}
