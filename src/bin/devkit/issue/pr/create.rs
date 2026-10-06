use std::path::Path;

use anyhow::{Context, Result, bail};
use devkit_common::{
    caller::Caller,
    forge::{self, Forge, NewPr, PrLocator},
    progress::Steps,
    vcs::{Vcs, VersionControl},
};
use devkit_config::{IssueEvent, PrCreateState};

use super::{
    add_reviewers,
    proof::require_proof,
    render::{Texts, values},
    require_reviewer_for_ready,
    resolve::{Existing, assert_belongs, parse_pr_flag, resolve_existing, verify_created},
    reviewer_logins,
};
use crate::{
    issue::review::{PrAction, Target, action_for, guard_branch, resolve_target},
    template::VarArgs,
};

pub struct Args {
    pub draft: bool,
    pub ready: bool,
    pub to: Vec<String>,
    pub base: Option<String>,
    pub pr_title: Option<String>,
    pub pr_body: Option<String>,
    /// `--attach` values, passed to the forge as given.
    pub attach: Vec<String>,
    pub no_push: bool,
    /// Use this PR for this run: a PR URL keeps its own repository, a
    /// bare number means `pr_repo`. Replaces a wrong recorded binding, since
    /// recording what this run acts on is what makes it a rebind.
    pub pr: Option<String>,
    pub vars: VarArgs,
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
        (
            "ready for review",
            "--draft",
            "convert it to a draft on the forge",
        )
    };
    Some(format!(
        "PR #{number} already exists and is {is}.\n\
         {flag} was ignored. To move it: {way_back}"
    ))
}

/// Reject an empty rendered PR title. Opening or rendering a PR needs one;
/// reusing an open PR does not.
pub(super) fn require_pr_title(title: &str) -> Result<()> {
    if title.trim().is_empty() {
        bail!("--pr-title is required: the pr_title template rendered empty");
    }
    Ok(())
}

/// Refuse `--attach` values gh could not upload, before anything is pushed.
/// A file is read the way gh reads it: the whole value when it names a file,
/// else the longest prefix before a `#` that does, with the rest as alt text.
fn check_attachments(forge: &dyn Forge, dir: &Path, attach: &[String]) -> Result<()> {
    if attach.is_empty() {
        return Ok(());
    }
    if !forge.attaches_media() {
        bail!(
            "--attach uploads through gh and needs a GitHub forge; this project's forge is {}",
            forge.kind()
        );
    }
    for value in attach {
        let prefixes = value.rmatch_indices('#').map(|(i, _)| &value[..i]);
        let found = std::iter::once(value.as_str())
            .chain(prefixes)
            .filter(|p| !p.is_empty())
            .any(|p| dir.join(p).is_file());
        if !found {
            let path = match value.rsplit_once('#') {
                Some((path, _)) if !path.is_empty() => path,
                _ => value,
            };
            bail!("--attach {path}: no such file in {}", dir.display());
        }
    }
    Ok(())
}

/// The PR this run acts on, created or reused.
pub(crate) struct Resolved {
    pub url: String,
    pub locator: PrLocator,
    /// Whether this run claimed the `pr_open` event, in the record write
    /// that names the PR, and so fires it.
    pub pr_open_claimed: bool,
}

/// Renders the PR title on demand.
type RenderTitle<'a> = Box<dyn FnOnce() -> Result<String> + 'a>;

/// Renders the PR body on demand, from the title this run resolved.
type RenderBody<'a> = Box<dyn FnOnce(&str) -> Result<String> + 'a>;

pub(crate) struct Ensure<'a> {
    pub existing: Existing<'a>,
    pub head: &'a str,
    pub state: PrCreateState,
    /// The explicit `--draft` / `--ready`, or `None` when neither was passed.
    /// `reuse_note` reports only a flag the user actually typed.
    pub asked: Option<PrCreateState>,
    pub base: String,
    /// Deferred for the same reason as `pr_body`, and called in the same place:
    /// only a run that opens a PR needs a title.
    pub pr_title: RenderTitle<'a>,
    /// Deferred rather than rendered: a `pr_title`/`pr_body` template reading
    /// `{{ issue }}` cannot be rendered outside a worktree `issue setup`
    /// created, and a run that only reuses a PR needs neither.
    pub pr_body: RenderBody<'a>,
    /// Logins to request as reviewers.
    pub reviewers: Vec<String>,
    /// Uploaded into the body of a PR this run opens.
    pub attachments: &'a [String],
    /// `defaults.require_pr_reviewer`: whether opening a PR ready for review
    /// demands a human reviewer.
    pub require_reviewer: bool,
    pub steps: &'a Steps,
    /// Claim the `pr_open` event in the record, which holds a tracker issue
    /// and has `[issue.events.pr_open]` configured.
    pub claim_pr_open: bool,
}

/// Resolve this branch's PR and, when there is none, open one. A reused PR
/// keeps the draft state it already has.
pub(crate) fn ensure(args: Ensure<'_>) -> Result<Resolved> {
    let found = resolve_existing(&args.existing)?;
    let start = args.existing.start;
    let steps = args.steps;
    let forge = args.existing.forge.forge.as_ref();

    let action = action_for(found.pr.as_ref().map(|p| p.state.as_str()));

    let mut resolved = match action {
        PrAction::Stop(reason) => bail!("{reason}"),
        PrAction::AddReviewer => {
            let pr = found.pr.expect("AddReviewer implies an existing PR");
            let locator = found
                .locator
                .expect("AddReviewer implies a resolved locator");
            // Mutating an existing PR is gated before the call: a mismatch here
            // is refused before a single reviewer is added.
            assert_belongs(&pr, args.head)?;
            if !args.attachments.is_empty() {
                bail!(
                    "PR #{n} already exists, and --attach uploads only into a PR this run opens.\n\
                     To add files to it: gh pr edit {n} --attach <file>",
                    n = pr.number
                );
            }
            add_reviewers(forge, &found.repo, pr.number, &args.reviewers, steps)?;
            if let Some(note) = reuse_note(pr.number, pr.is_draft, args.asked) {
                eprintln!("{note}");
            }
            Resolved {
                url: pr.url,
                locator,
                pr_open_claimed: false,
            }
        }
        PrAction::Create => {
            // The gate runs before either template: a run this policy refuses
            // must say so, not die first on a `pr_body` the refusal means it
            // never needed. A draft is not gated — an unreviewed draft is not a
            // violation — and the author is nobody yet, since the PR this run
            // is about to open has none.
            match args.state {
                PrCreateState::Ready => {
                    require_reviewer_for_ready(&[], &args.reviewers, args.require_reviewer, None)?;
                }
                PrCreateState::Draft => {}
            }
            let pr_title = (args.pr_title)()?;
            require_pr_title(&pr_title)?;
            let pr_body = (args.pr_body)(&pr_title)?;
            let new = NewPr {
                base: &args.base,
                head: args.existing.branch,
                title: &pr_title,
                body: &pr_body,
                draft: args.state == PrCreateState::Draft,
                reviewers: &args.reviewers,
                attachments: args.attachments,
            };
            let url = steps.during_result("Creating PR...", || {
                forge.create(&found.repo, &new, Path::new(start))
            })?;
            let locator = forge
                .locate(&url)
                .with_context(|| format!("could not read a PR number from {url}"))?;
            let created_repo = locator.resolve(&args.existing.forge.repos)?;
            // The gate runs before the record is written and before any
            // notification goes out.
            verify_created(forge, &created_repo, locator.number, args.head)
                .with_context(|| format!("{url} is open with nothing recorded"))?;
            Resolved {
                url,
                locator,
                pr_open_claimed: false,
            }
        }
    };

    if args.existing.record.is_some() {
        let toplevel = devkit_common::vcs::checkout_root(Path::new(start))?;
        resolved.pr_open_claimed = devkit_common::record::update(&toplevel, |rec| {
            let Some(rec) = rec else { return false };
            rec.pr = Some(resolved.locator.clone());
            args.claim_pr_open && rec.claim(IssueEvent::PrOpen)
        })?;
    }
    Ok(resolved)
}

pub fn run(args: Args) -> Result<()> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());
    let loaded =
        devkit_ports::load::load(args.config.as_deref().map(Path::new), Path::new(&start))?;
    let people = &loaded.config.people;
    let forge = forge::resolve(&loaded.config.forge, &loaded.config.github, &start, None);
    check_attachments(forge.forge.as_ref(), Path::new(&start), &args.attach)?;

    let caller = devkit_common::caller::caller();
    let vars = values("issue pr", &loaded.config, &args.vars, caller)?;

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

    let tracker_issue = record.as_ref().and_then(|r| r.tracker_issue());
    if let (Some(variable), Some(issue), Caller::Agent) = (
        &loaded.config.defaults.pr_proof_variable,
        tracker_issue.as_deref(),
        caller,
    ) {
        let tracker =
            devkit_common::tracker::resolve(loaded.config.tracker.kind, here, &forge.repos);
        let proof = vars.get(variable).map_or("", String::as_str);
        require_proof(tracker.tracker.as_ref(), issue, proof, variable)?;
    }

    let texts = Texts::new(
        &loaded.config.templates,
        record.as_ref(),
        &branch,
        &toplevel,
        vars,
        args.pr_title,
        args.pr_body,
    );

    let head = vcs.revision(here)?;

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
            forge: &forge,
            record: record.as_ref(),
            explicit_pr: args
                .pr
                .as_deref()
                .map(|s| parse_pr_flag(s, forge.forge.as_ref()))
                .transpose()?,
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
        pr_title: Box::new(|| texts.title()),
        pr_body: Box::new(|title| texts.body(title)),
        reviewers,
        attachments: &args.attach,
        require_reviewer: loaded.config.defaults.require_pr_reviewer,
        steps: &steps,
        claim_pr_open: tracker_issue.is_some() && loaded.config.issue.events.pr_open.is_some(),
    })?;

    println!("{}", resolved.url);
    if let (true, Some(issue)) = (resolved.pr_open_claimed, tracker_issue.as_deref()) {
        crate::issue::event::fire_inline(
            here,
            args.config.as_deref().map(Path::new),
            IssueEvent::PrOpen,
            issue,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use devkit_ports::templates::worktree_context;

    use super::*;
    use crate::issue::review::render_review;

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
            // pr_is_draft
            false,
            Some(PrCreateState::Draft),
        );
        let note = note.expect("a contradicted flag is reported");
        assert!(note.contains("#123"), "names the PR: {note}");
        assert!(
            note.contains("convert it to a draft"),
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
    /// undefined variables, and `worktree_context` binds `issue` only when the
    /// worktree has an `issue setup` record.
    #[test]
    fn a_body_template_reading_the_record_fails_without_one() {
        let ctx = worktree_context(None, Some("lev/eng-1-fix"));
        let vars = std::collections::BTreeMap::new();
        assert!(render_review("Closes {{ issue }}", "pr_body", &ctx, &vars, None).is_err());

        let record = devkit_common::record::IssueRecord {
            issue: "ENG-1".into(),
            slug: "fix-login".into(),
            apps: Vec::new(),
            summary: None,
            pr: None,
            baseline: None,
            ..Default::default()
        };
        let ctx = worktree_context(Some(&record), Some("lev/eng-1-fix"));
        let out = render_review("Closes {{ issue }}", "pr_body", &ctx, &vars, None).unwrap();
        assert_eq!(out, "Closes ENG-1");
    }
}
