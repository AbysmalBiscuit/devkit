use anyhow::{Context, Result, bail};
use devkit_common::cmd::{capture, gh_json, git};
use devkit_common::progress::Steps;
use devkit_ports::config::Person;
use serde::Deserialize;
use std::collections::HashMap;

use super::{
    PrAction, Target, action_for, base_ctx, deliver, guard_branch, is_human_login, parse_args,
    person_by_login, render_review, require_pr_title, resolve_target, target_from_person,
    with_fields,
};

pub struct Args {
    pub body: Option<String>,
    pub to: Vec<String>,
    pub base: Option<String>,
    pub pr_title: Option<String>,
    pub pr_body: Option<String>,
    pub no_push: bool,
    pub args: Vec<String>,
    pub dir: Option<String>,
    pub config: Option<String>,
}

#[derive(Deserialize)]
struct PrView {
    number: u64,
    state: String,
    url: String,
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

/// Notify-targets on the AddReviewer path: explicit `--to`, else the PR's
/// existing human reviewers (reverse-looked-up).
fn resolve_request_targets(
    explicit: &[Target],
    pr: u64,
    cwd: &str,
    people: &HashMap<String, Person>,
) -> Result<Vec<Target>> {
    if !explicit.is_empty() {
        return Ok(explicit.to_vec());
    }
    let view: ReviewRequestsView = gh_json(
        &["pr", "view", &pr.to_string(), "--json", "reviewRequests"],
        cwd,
    )?;
    let logins: Vec<String> = view
        .review_requests
        .into_iter()
        .filter_map(|r| r.login)
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

    let mut vars = tmpls.variables.clone();
    vars.extend(parse_args(&args.args, &tmpls.variables)?);

    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"], &start)?
        .trim()
        .to_string();
    guard_branch(&branch)?;

    let steps = Steps::new();
    if !args.no_push {
        steps
            .during("Pushing branch…", || {
                git(&["push", "-u", "origin", &branch], &start)
            })
            .context("git push failed (refusing to force-push)")?;
    }

    let explicit: Vec<Target> = args
        .to
        .iter()
        .map(|v| resolve_target(v, people))
        .collect::<Result<_>>()?;

    let toplevel = git(&["rev-parse", "--show-toplevel"], &start)?
        .trim()
        .to_string();
    let record = crate::record::read(std::path::Path::new(&toplevel));
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

    let existing: Option<PrView> = steps
        .during("Looking up existing PR…", || {
            gh_json::<Vec<PrView>>(
                &[
                    "pr",
                    "list",
                    "--head",
                    &branch,
                    "--state",
                    "all",
                    "--json",
                    "number,state,url",
                    "--limit",
                    "1",
                ],
                &start,
            )
        })?
        .into_iter()
        .next();

    let (pr_url, targets) = match action_for(existing.as_ref().map(|p| p.state.as_str())) {
        PrAction::Stop(reason) => bail!("{reason}"),
        PrAction::AddReviewer => {
            let pr = existing.expect("AddReviewer implies an existing PR");
            let targets = steps.during("Resolving reviewers…", || {
                resolve_request_targets(&explicit, pr.number, &start, people)
            })?;
            let (logins, warnings) = reviewer_logins(&targets);
            for w in &warnings {
                eprintln!("warning: {w}");
            }
            if !logins.is_empty() {
                steps
                    .during("Adding reviewers…", || {
                        capture(
                            "gh",
                            &[
                                "pr",
                                "edit",
                                &pr.number.to_string(),
                                "--add-reviewer",
                                &logins.join(","),
                            ],
                            Some(&start),
                        )
                    })
                    .context("gh pr edit --add-reviewer failed")?;
            }
            (pr.url, targets)
        }
        PrAction::Create => {
            require_pr_title(&pr_title)?;
            if explicit.is_empty() {
                bail!("at least one --to is required to create a PR");
            }
            let (logins, warnings) = reviewer_logins(&explicit);
            for w in &warnings {
                eprintln!("warning: {w}");
            }
            let pr_body = render_review(
                tmpls.pr_body(),
                "pr_body",
                &with_fields(
                    &base,
                    &[
                        (
                            "input",
                            serde_json::json!(args.pr_body.clone().unwrap_or_default()),
                        ),
                        ("pr_title", serde_json::json!(pr_title)),
                    ],
                ),
                &vars,
                missing_at,
            )?;
            let base_branch = args
                .base
                .clone()
                .unwrap_or_else(|| loaded.config.defaults.pr_base.clone());
            let joined = logins.join(",");
            let mut gh_args = vec![
                "pr",
                "create",
                "--base",
                &base_branch,
                "--title",
                &pr_title,
                "--body",
                &pr_body,
            ];
            if !logins.is_empty() {
                gh_args.push("--reviewer");
                gh_args.push(&joined);
            }
            let out = steps
                .during("Creating PR…", || capture("gh", &gh_args, Some(&start)))
                .context("gh pr create failed")?;
            let url = out
                .lines()
                .rev()
                .find(|l| l.contains("://"))
                .context("could not parse a PR URL from `gh pr create` output")?
                .trim()
                .to_string();
            (url, explicit)
        }
    };

    let notify_ctx = with_fields(
        &base,
        &[
            ("pr_url", serde_json::json!(pr_url)),
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

    #[test]
    fn targets_from_logins_reverse_looks_up_and_warns() {
        use devkit_ports::config::Person;
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
