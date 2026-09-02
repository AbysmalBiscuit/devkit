use anyhow::{Context, Result, bail};
use devkit_common::cmd::gh_capture;
use devkit_common::git::Git;
use devkit_common::github;
use devkit_common::progress::Steps;
use devkit_config::Person;
use std::collections::HashMap;

use crate::issue::pr::{
    self, add_reviewers, gate_ready, requested_reviewer_logins, require_existing_pr,
    reviewer_logins,
};

use super::{
    Target, base_ctx, deliver, guard_branch, is_human_login, parse_args, person_by_login,
    resolve_target, target_from_person, with_fields,
};

pub struct Args {
    pub body: Option<String>,
    pub to: Vec<String>,
    pub no_push: bool,
    pub no_notify: bool,
    /// Use this PR for this run: a GitHub PR URL keeps its own repository, a
    /// bare number means `pr_repo`. Replaces a wrong recorded binding, since
    /// recording what this run acts on is what makes it a rebind.
    pub pr: Option<String>,
    pub args: Vec<String>,
    pub dir: Option<String>,
    pub config: Option<String>,
}

/// Build Slack targets from reviewer logins via reverse lookup. Unmatched logins
/// are skipped with a warning.
pub(crate) fn targets_from_logins(
    logins: &[String],
    people: &HashMap<String, Person>,
) -> (Vec<Target>, Vec<String>) {
    let mut targets = Vec::new();
    let mut warnings = Vec::new();
    for login in logins {
        match person_by_login(login, people) {
            Some((alias, p)) => targets.push(target_from_person(alias, p)),
            None => warnings.push(format!("reviewer `{login}` has no [people] alias; skipped")),
        }
    }
    (targets, warnings)
}

/// `--no-notify` pins the targets to whatever `--to` resolved to — possibly none —
/// instead of falling back to the PR's current reviewers. `None` means no override:
/// resolve them as usual.
pub(crate) fn pinned_targets(explicit: &[Target], no_notify: bool) -> Option<Vec<Target>> {
    no_notify.then(|| explicit.to_vec())
}

/// Notify-targets on the reuse path: explicit `--to`, else the PR's existing
/// human reviewers (reverse-looked-up).
fn resolve_request_targets(
    explicit: &[Target],
    pr: u64,
    cwd: &str,
    repo: &github::Repo,
    people: &HashMap<String, Person>,
) -> Result<Vec<Target>> {
    if !explicit.is_empty() {
        return Ok(explicit.to_vec());
    }
    let logins: Vec<String> = requested_reviewer_logins(pr, cwd, repo)?
        .into_iter()
        .filter(|l| is_human_login(l))
        .collect();
    if logins.is_empty() {
        bail!("no reviewers on the PR and no --to given");
    }
    let (targets, warnings) = targets_from_logins(&logins, people);
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    if targets.is_empty() {
        bail!("none of the PR's reviewers map to a [people] alias; pass --to");
    }
    Ok(targets)
}

/// Asking a human to look at a draft is incoherent, so a notifying run promotes
/// it. `--no-notify` tells nobody, and promoting a PR to ready tells everybody.
fn should_flip(is_draft: bool, no_notify: bool) -> bool {
    is_draft && !no_notify
}

pub fn run(args: Args) -> Result<()> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());
    let loaded = devkit_ports::load::load(
        args.config.as_deref().map(std::path::Path::new),
        std::path::Path::new(&start),
    )?;
    let people = &loaded.config.people;
    let tmpls = &loaded.config.templates;
    let repos = github::Repos::resolve(&loaded.config.github, &start, None);

    let mut vars = tmpls.variables.clone();
    vars.extend(parse_args(&args.args, &tmpls.variables)?);

    let branch = devkit_common::git::branch(std::path::Path::new(&start))?;
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

    let toplevel = devkit_common::git::checkout_root(std::path::Path::new(&start))?
        .to_string_lossy()
        .into_owned();
    let record = devkit_common::record::read(std::path::Path::new(&toplevel));
    let missing_at = if record.is_none() {
        Some(toplevel.as_str())
    } else {
        None
    };

    let base = base_ctx(record.as_ref(), &branch);

    let head = Git::at(std::path::Path::new(&start))
        .args(["rev-parse", "HEAD"])
        .output()?
        .trim()
        .to_string();

    let steps = Steps::persistent();
    let found = pr::resolve::resolve_existing(&pr::resolve::Existing {
        start: &start,
        branch: &branch,
        repos: &repos,
        record: record.as_ref(),
        explicit_pr: args
            .pr
            .as_deref()
            .map(pr::resolve::parse_pr_flag)
            .transpose()?,
        no_push: args.no_push,
        steps: &steps,
    })?;
    require_existing_pr(found.pr.as_ref().map(|p| p.state.as_str()))?;
    let pr = found.pr.expect("require_existing_pr rejects a missing PR");
    let locator = found.locator.expect("a resolved PR carries a locator");
    let repo = found.repo;
    // Every mutation below is gated on the PR carrying this worktree's commits.
    super::finish::assert_belongs(&pr, &head)?;

    // Resolving the recipients can refuse the run — a PR with no reviewers and
    // no `--to` names nobody — so it happens before any mutation. Refusing
    // after the flip would leave the PR ready for a review nobody was asked for.
    let targets = match pinned_targets(&explicit, args.no_notify) {
        Some(t) => t,
        None => steps.during_result("Resolving reviewers…", || {
            resolve_request_targets(&explicit, pr.number, &start, &repo, people)
        })?,
    };

    let number = pr.number.to_string();
    add_reviewers(pr.number, &reviewers, &repo, &start, &steps)?;

    if should_flip(pr.is_draft, args.no_notify) {
        // Refusing before the flip leaves the PR a draft.
        gate_ready(
            pr.number,
            &reviewers,
            loaded.config.defaults.require_pr_reviewer,
            &repo,
            &start,
            &steps,
        )?;
        steps
            .during_result("Marking ready for review…", || {
                gh_capture(&["pr", "ready", &number], &repo, &start)
            })
            .context("gh pr ready failed")?;
    }

    if let Some(rec) = pr::resolve::record_with_pr(record.as_ref(), locator) {
        devkit_common::record::write(std::path::Path::new(&toplevel), &rec)?;
    }

    if args.no_notify {
        println!("{}", pr.url);
        return Ok(());
    }

    let full = steps
        .during_result("Fetching PR title…", || {
            super::finish::fetch_pr_full(pr.number, &start, &repo)
        })
        .context("fetching the PR's title")?;

    let notify_ctx = with_fields(
        &base,
        &[
            ("pr_url", serde_json::json!(pr.url)),
            ("pr_title", serde_json::json!(full.title)),
            (
                "input",
                serde_json::json!(args.body.clone().unwrap_or_default()),
            ),
        ],
    );
    deliver(
        tmpls.review_request(),
        "review_request",
        &notify_ctx,
        &vars,
        missing_at,
        &targets,
        &steps,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(name: &str, gh: Option<&str>) -> Target {
        Target {
            channel: format!("U_{name}"),
            name: name.into(),
            slack_id: Some(format!("U_{name}")),
            github: gh.map(String::from),
        }
    }

    #[test]
    fn no_notify_pins_targets_to_explicit() {
        let igor = person("igor", Some("igoracc"));

        let none = pinned_targets(&[], true).expect("--no-notify overrides the fallback");
        assert!(none.is_empty());
        assert!(reviewer_logins(&none).0.is_empty());

        let one = pinned_targets(std::slice::from_ref(&igor), true).expect("override");
        assert_eq!(reviewer_logins(&one).0, vec!["igoracc"]);

        assert!(pinned_targets(&[igor], false).is_none());
    }

    #[test]
    fn notifying_flips_a_draft_but_no_notify_does_not() {
        assert!(should_flip(
            /* is_draft */ true, /* no_notify */ false
        ));
        assert!(!should_flip(true, true));
        assert!(!should_flip(false, false));
        assert!(!should_flip(false, true));
    }

    #[test]
    fn targets_from_logins_reverse_looks_up_and_warns() {
        use devkit_config::Person;
        use std::collections::HashMap;
        let people = HashMap::from([(
            "lev".to_string(),
            Person {
                slack: "U_LEV".into(),
                github: Some("LevValle".into()),
            },
        )]);
        let (targets, warnings) =
            targets_from_logins(&["levvalle".into(), "ghost".into()], &people);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].name, "lev");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("ghost"));
    }
}
