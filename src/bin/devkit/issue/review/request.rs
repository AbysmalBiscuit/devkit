use anyhow::{Result, bail};
use devkit_common::cmd::gh_json_in;
use devkit_common::git::Git;
use devkit_common::github;
use devkit_common::progress::Steps;
use devkit_config::{Person, PrCreateState};
use serde::Deserialize;
use std::collections::HashMap;

use crate::issue::pr::create;

use super::{
    Target, base_ctx, deliver, guard_branch, is_human_login, parse_args, person_by_login,
    render_review, resolve_target, target_from_person, with_fields,
};

pub struct Args {
    pub body: Option<String>,
    pub to: Vec<String>,
    pub base: Option<String>,
    pub pr_title: Option<String>,
    pub pr_body: Option<String>,
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewRequestsView {
    review_requests: Vec<ReviewRequest>,
}

#[derive(Deserialize)]
struct ReviewRequest {
    #[serde(default)]
    login: Option<String>,
}

/// GitHub logins among targets that can be requested as reviewers, plus warnings
/// for people that have no github handle. Channels are silently Slack-only.
pub(crate) fn reviewer_logins(targets: &[Target]) -> (Vec<String>, Vec<String>) {
    let mut logins = Vec::new();
    let mut warnings = Vec::new();
    for t in targets {
        match &t.github {
            Some(login) => logins.push(login.clone()),
            None if t.slack_id.is_some() => {
                warnings.push(format!(
                    "`{}` has no github handle; not added as reviewer",
                    t.name
                ));
            }
            None => {}
        }
    }
    (logins, warnings)
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

/// Logins currently requested as reviewers on PR `pr`, over direct HTTP when a
/// token is available, else `gh pr view --json reviewRequests`.
fn requested_reviewer_logins(pr: u64, cwd: &str, repo: &github::Repo) -> Result<Vec<String>> {
    if github::token().is_some()
        && let Ok(logins) = github::requested_reviewers(&repo.slug, pr)
    {
        return Ok(logins);
    }
    let view: ReviewRequestsView = gh_json_in(
        &["pr", "view", &pr.to_string(), "--json", "reviewRequests"],
        repo,
        cwd,
    )?;
    Ok(view
        .review_requests
        .into_iter()
        .filter_map(|r| r.login)
        .collect())
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
    let pr_title = render_review(
        tmpls.pr_title(),
        "pr_title",
        &with_fields(
            &base,
            &[(
                "input",
                serde_json::json!(args.pr_title.clone().unwrap_or_default()),
            )],
        ),
        &vars,
        missing_at,
    )?;
    let body_ctx = with_fields(
        &base,
        &[
            (
                "input",
                serde_json::json!(args.pr_body.clone().unwrap_or_default()),
            ),
            ("pr_title", serde_json::json!(pr_title)),
        ],
    );

    let head = Git::at(std::path::Path::new(&start))
        .args(["rev-parse", "HEAD"])
        .output()?
        .trim()
        .to_string();

    let steps = Steps::persistent();
    let resolved = create::ensure(create::Ensure {
        existing: create::Existing {
            start: &start,
            branch: &branch,
            repos: &repos,
            record: record.as_ref(),
            explicit_pr: args.pr.as_deref().map(create::parse_pr_flag).transpose()?,
            no_push: args.no_push,
            steps: &steps,
        },
        head: &head,
        // Asking for a review means the PR is ready for one, whatever
        // `defaults.pr_create_state` says.
        state: PrCreateState::Ready,
        asked: None,
        base: args
            .base
            .clone()
            .unwrap_or_else(|| loaded.config.defaults.pr_base.clone()),
        pr_title: pr_title.clone(),
        pr_body: Box::new(|| {
            render_review(tmpls.pr_body(), "pr_body", &body_ctx, &vars, missing_at)
        }),
        reviewers,
        require_reviewer: loaded.config.defaults.require_pr_reviewer,
        steps: &steps,
    })?;

    let targets = if resolved.created {
        explicit
    } else {
        match pinned_targets(&explicit, args.no_notify) {
            Some(t) => t,
            None => {
                let repo = resolved.locator.resolve(&repos)?;
                steps.during_result("Resolving reviewers…", || {
                    resolve_request_targets(&explicit, resolved.pr.number, &start, &repo, people)
                })?
            }
        }
    };

    if args.no_notify {
        println!("{}", resolved.url);
        return Ok(());
    }

    let notify_ctx = with_fields(
        &base,
        &[
            ("pr_url", serde_json::json!(resolved.url)),
            ("pr_title", serde_json::json!(pr_title)),
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

    fn chan(name: &str) -> Target {
        Target {
            channel: name.into(),
            name: name.into(),
            slack_id: None,
            github: None,
        }
    }
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
    fn reviewer_logins_collects_handles_and_warns() {
        let targets = vec![
            person("lev", Some("LevValle")),
            person("igor", None),
            chan("#eng"),
        ];
        let (logins, warnings) = reviewer_logins(&targets);
        assert_eq!(logins, vec!["LevValle"]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("igor"));
    }

    /// `require_pr_reviewer` is satisfied by what `--to` contributes as a GitHub
    /// reviewer, so a `#channel` and a person with no handle both leave a create
    /// with nobody to review it.
    #[test]
    fn the_reviewer_gate_reads_handles_not_slack_recipients() {
        let (logins, _) = reviewer_logins(&[chan("#eng"), person("igor", None)]);
        assert!(logins.is_empty());
        assert!(crate::issue::review::require_reviewer(&logins, true).is_err());

        let (logins, _) = reviewer_logins(&[chan("#eng"), person("lev", Some("LevValle"))]);
        assert!(crate::issue::review::require_reviewer(&logins, true).is_ok());
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
