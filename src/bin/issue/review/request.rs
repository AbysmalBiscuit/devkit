use anyhow::{Context, Result, bail};
use devkit_common::cmd::{gh_capture, gh_json_in, git};
use devkit_common::github;
use devkit_common::progress::Steps;
use devkit_config::Person;
use serde::Deserialize;
use std::collections::HashMap;

use super::finish::{Fallback, decide_fallback, ensure_unambiguous_gh_match, resolve_acting};
use super::{
    PrAction, Target, action_for, base_ctx, deliver, guard_branch, is_human_login, parse_args,
    person_by_login, render_review, require_pr_title, require_reviewer, resolve_target,
    target_from_person, with_fields,
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

/// The existing PR for head branch `branch`, over direct HTTP when possible
/// else `gh pr list`. `Ok(None)` means no PR.
fn existing_pr(branch: &str, cwd: &str, repo: &github::Repo) -> Result<Option<github::PrBrief>> {
    let looked = github::pr_by_head(repo, branch);
    if decide_fallback(&looked) == Fallback::No {
        return resolve_acting(&looked);
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
            "number,state,url,headRefName,headRefOid",
        ],
        repo,
        cwd,
    )?;
    ensure_unambiguous_gh_match(v.len())?;
    Ok(v.into_iter().next().map(|p| github::PrBrief {
        number: p.number,
        state: p.state,
        url: p.url,
        head_ref_name: p.head_ref_name,
        head_ref_oid: p.head_ref_oid,
        head_repo_owner: None,
    }))
}

/// `--pr <URL|number>`: a GitHub PR URL keeps its own repository; a bare
/// number means `pr_repo`.
fn parse_pr_flag(s: &str) -> Result<github::PrLocator> {
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
fn verify_created(repo: &github::Repo, number: u64, head: &str) -> Result<()> {
    let created = github::pr_meta_full(repo, number).context("fetching the PR just created")?;
    super::finish::assert_belongs(&created, head)
        .context("the PR just created does not carry this worktree's commits")
}

/// The record to write once a PR is resolved: the existing record with its
/// `pr` field replaced. `None` when there is no record to attach it to — an
/// `issue review request` run outside a worktree `issue setup` created.
pub(crate) fn record_with_pr(
    record: Option<&devkit_common::record::IssueRecord>,
    loc: github::PrLocator,
) -> Option<devkit_common::record::IssueRecord> {
    record.map(|r| devkit_common::record::IssueRecord {
        pr: Some(loc),
        ..r.clone()
    })
}

/// `--no-notify` pins the targets to whatever `--to` resolved to — possibly none —
/// instead of falling back to the PR's current reviewers. `None` means no override:
/// resolve them as usual.
pub(crate) fn pinned_targets(explicit: &[Target], no_notify: bool) -> Option<Vec<Target>> {
    no_notify.then(|| explicit.to_vec())
}

/// Notify-targets on the AddReviewer path: explicit `--to`, else the PR's
/// existing human reviewers (reverse-looked-up).
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

    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"], &start)?
        .trim()
        .to_string();
    guard_branch(&branch)?;

    let steps = Steps::persistent();
    if !args.no_push {
        steps
            .during_result("Pushing branch…", || {
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

    let head = git(&["rev-parse", "HEAD"], &start)?.trim().to_string();

    // Explicit `--pr`, then the record, then the worktree branch's PR.
    let explicit_loc = args.pr.as_deref().map(parse_pr_flag).transpose()?;
    let record_loc = record.as_ref().and_then(|r| r.pr.clone());
    let resolved_loc = super::finish::resolve_locator(explicit_loc.as_ref(), record_loc.as_ref());
    let repo = acting_repo(resolved_loc.as_ref(), &repos)?;

    // The PR this run already knows about, paired with the locator to persist
    // for it once resolved — either the one that named it, or `repo` when
    // branch discovery found it.
    let (existing, existing_loc): (Option<github::PrBrief>, Option<github::PrLocator>) =
        match &resolved_loc {
            Some(loc) => {
                let pr = steps
                    .during_result(&format!("Fetching PR #{}…", loc.number), || {
                        github::pr_meta_full(&repo, loc.number)
                    })
                    .with_context(|| fetch_context(loc.number, explicit_loc.is_none()))?;
                (Some(pr), Some(loc.clone()))
            }
            None => {
                let found = steps.during_result("Looking up existing PR…", || {
                    existing_pr(&branch, &start, &repo)
                })?;
                let loc = found.as_ref().map(|p| github::PrLocator {
                    repo: Some(repo.slug.clone()),
                    number: p.number,
                });
                (found, loc)
            }
        };

    let (pr_url, targets, final_loc) = match action_for(existing.as_ref().map(|p| p.state.as_str()))
    {
        PrAction::Stop(reason) => bail!("{reason}"),
        PrAction::AddReviewer => {
            let pr = existing.expect("AddReviewer implies an existing PR");
            let loc = existing_loc.expect("AddReviewer implies a resolved locator");
            // Mutating an existing PR is gated before the call: a mismatch here
            // is refused before a single reviewer is added.
            super::finish::assert_belongs(&pr, &head)?;
            let targets = match pinned_targets(&explicit, args.no_notify) {
                Some(t) => t,
                None => steps.during_result("Resolving reviewers…", || {
                    resolve_request_targets(&explicit, pr.number, &start, &repo, people)
                })?,
            };
            let (logins, warnings) = reviewer_logins(&targets);
            for w in &warnings {
                eprintln!("warning: {w}");
            }
            if !logins.is_empty() {
                steps
                    .during_result("Adding reviewers…", || {
                        gh_capture(
                            &[
                                "pr",
                                "edit",
                                &pr.number.to_string(),
                                "--add-reviewer",
                                &logins.join(","),
                            ],
                            &repo,
                            &start,
                        )
                    })
                    .context("gh pr edit --add-reviewer failed")?;
            }
            (pr.url, targets, loc)
        }
        PrAction::Create => {
            require_pr_title(&pr_title)?;
            require_reviewer(&explicit, loaded.config.defaults.require_pr_reviewer)?;
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
                .during_result("Creating PR…", || gh_capture(&gh_args, &repo, &start))
                .context("gh pr create failed")?;
            let url = out
                .lines()
                .rev()
                .find(|l| l.contains("://"))
                .context("could not parse a PR URL from `gh pr create` output")?
                .trim()
                .to_string();
            let loc = github::PrLocator::from_url(&url)
                .context("could not parse a PR number from `gh pr create` output")?;
            let created_repo = loc.resolve(&repos)?;
            // The gate runs before the record is written and before any
            // notification goes out.
            verify_created(&created_repo, loc.number, &head).with_context(|| {
                format!("{url} is open with nothing recorded and no notification sent")
            })?;
            (url, explicit, loc)
        }
    };

    if let Some(rec) = record_with_pr(record.as_ref(), final_loc) {
        devkit_common::record::write(std::path::Path::new(&toplevel), &rec)?;
    }

    if args.no_notify {
        println!("{pr_url}");
        return Ok(());
    }

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
