use anyhow::{Context, Result};
use devkit_common::cmd::{gh_json_in, git};
use devkit_common::github;
use devkit_common::progress::Steps;
use devkit_config::Person;
use serde::Deserialize;
use std::collections::HashMap;

use super::{
    Target, base_ctx, deliver, parse_args, person_by_login, resolve_target, target_from_person,
    with_fields,
};

pub struct Args {
    pub body: Option<String>,
    pub to: Vec<String>,
    pub pr: Option<u64>,
    pub args: Vec<String>,
    pub dir: Option<String>,
    pub config: Option<String>,
}

#[derive(Deserialize)]
struct PrLite {
    number: u64,
}

#[derive(Deserialize)]
struct PrFull {
    url: String,
    title: String,
    author: Author,
}

#[derive(Deserialize)]
struct Author {
    #[serde(default)]
    login: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Fallback {
    Yes,
    No,
}

/// Only a transport that could not answer sends the caller to `gh`. A definite
/// "no PR" is an answer and must be trusted, or the fallback re-asks a question
/// that was already resolved and can return a different PR.
pub(crate) fn decide_fallback(l: &github::HeadLookup) -> Fallback {
    match l {
        github::HeadLookup::Unavailable(_) => Fallback::Yes,
        github::HeadLookup::Unique(_)
        | github::HeadLookup::NoMatch
        | github::HeadLookup::Ambiguous(_) => Fallback::No,
    }
}

/// The single PR an acting path may operate on. Ambiguity is refused rather
/// than ranked: `review finish` is about to merge or close, and two forks
/// proposing one branch name is the case that produces two candidates.
pub(crate) fn resolve_acting(l: &github::HeadLookup) -> Result<Option<github::PrBrief>> {
    match l {
        github::HeadLookup::Unique(p) => Ok(Some(p.clone())),
        github::HeadLookup::NoMatch => Ok(None),
        github::HeadLookup::Ambiguous(c) => {
            let list = c
                .iter()
                .map(|p| format!("#{} ({})", p.number, p.url))
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!("several PRs share this head branch: {list} — pass --pr to choose one")
        }
        github::HeadLookup::Unavailable(why) => {
            anyhow::bail!("could not look up the PR for this branch: {why}")
        }
    }
}

/// PR number for head branch `b`, over direct HTTP when a token is available,
/// else `gh pr list`. `Ok(None)` means no PR (whichever path answered).
fn branch_pr_number(b: &str, cwd: &str, repo: &github::Repo) -> Result<Option<u64>> {
    let looked = github::pr_by_head(repo, b);
    if decide_fallback(&looked) == Fallback::No {
        return Ok(resolve_acting(&looked)?.map(|p| p.number));
    }
    // `--limit 1` is gone: the fallback must be able to see a second candidate
    // rather than silently taking whichever came first.
    let v: Vec<PrLite> = gh_json_in(
        &[
            "pr", "list", "--head", b, "--state", "all", "--json", "number",
        ],
        repo,
        cwd,
    )?;
    anyhow::ensure!(
        v.len() <= 1,
        "several PRs share this head branch — pass --pr to choose one"
    );
    Ok(v.into_iter().next().map(|p| p.number))
}

/// URL/title/author for PR `n`, over direct HTTP when possible else `gh pr view`.
fn fetch_pr_full(n: u64, cwd: &str, repo: &github::Repo) -> Result<PrFull> {
    if github::token().is_some()
        && let Ok(f) = github::pr_full(&repo.slug, n)
    {
        return Ok(PrFull {
            url: f.url,
            title: f.title,
            author: Author {
                login: f.author_login,
            },
        });
    }
    gh_json_in(
        &["pr", "view", &n.to_string(), "--json", "url,title,author"],
        repo,
        cwd,
    )
}

/// Choose the PR number: explicit `--pr` wins, else the worktree branch's PR.
pub(crate) fn resolve_pr(branch_pr: Option<u64>, pr_flag: Option<u64>) -> Result<u64> {
    pr_flag
        .or(branch_pr)
        .context("no PR for the current branch; pass --pr <number>")
}

/// Build the PR-author Slack target via reverse lookup.
pub(crate) fn author_target(login: &str, people: &HashMap<String, Person>) -> Result<Target> {
    person_by_login(login, people)
        .map(|(alias, p)| target_from_person(alias, p))
        .with_context(|| format!("PR author `{login}` has no [people] alias; pass --to"))
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
    let pr_repo = repos.prs()?;

    let mut vars = tmpls.variables.clone();
    vars.extend(parse_args(&args.args, &tmpls.variables)?);

    let steps = Steps::persistent();
    // PR from the current branch (best effort), unless --pr is given.
    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"], &start)
        .ok()
        .map(|b| b.trim().to_string());
    let branch_pr = branch.as_deref().and_then(|b| {
        steps
            .during_result("Looking up PR for branch…", || {
                branch_pr_number(b, &start, pr_repo)
            })
            .ok()
            .flatten()
    });
    let number = resolve_pr(branch_pr, args.pr)?;

    let record = git(&["rev-parse", "--show-toplevel"], &start)
        .ok()
        .and_then(|top| devkit_common::record::read(std::path::Path::new(top.trim())));

    let view: PrFull = steps.during_result(&format!("Fetching PR #{number}…"), || {
        fetch_pr_full(number, &start, pr_repo)
    })?;
    let author_login = view.author.login;

    let targets: Vec<Target> = if args.to.is_empty() {
        let login = author_login
            .as_deref()
            .context("PR has no author login; pass --to")?;
        vec![author_target(login, people)?]
    } else {
        args.to
            .iter()
            .map(|v| resolve_target(v, people))
            .collect::<Result<_>>()?
    };

    let base = base_ctx(record.as_ref(), branch.as_deref().unwrap_or(""));
    let notify_ctx = with_fields(
        &base,
        &[
            ("pr_url", serde_json::json!(view.url)),
            ("pr_title", serde_json::json!(view.title)),
            (
                "author",
                serde_json::json!(author_login.unwrap_or_default()),
            ),
            ("input", serde_json::json!(args.body.unwrap_or_default())),
        ],
    );
    deliver(
        tmpls.review_finish(),
        "review_finish",
        &notify_ctx,
        &vars,
        None,
        &targets,
        &steps,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use devkit_common::github::HeadLookup;
    use devkit_config::Person;
    use std::collections::HashMap;

    #[test]
    fn resolve_pr_prefers_flag_then_branch() {
        assert_eq!(resolve_pr(Some(7), Some(9)).unwrap(), 9);
        assert_eq!(resolve_pr(Some(7), None).unwrap(), 7);
        assert_eq!(resolve_pr(None, Some(9)).unwrap(), 9);
        assert!(resolve_pr(None, None).is_err());
    }

    #[test]
    fn author_target_reverse_looks_up_or_errors() {
        let people = HashMap::from([(
            "lev".to_string(),
            Person {
                slack: "U_LEV".into(),
                github: Some("LevValle".into()),
            },
        )]);
        let t = author_target("levvalle", &people).unwrap();
        assert_eq!(t.name, "lev");
        assert_eq!(t.channel, "U_LEV");
        assert!(author_target("ghost", &people).is_err());
    }

    #[test]
    fn no_match_does_not_reach_the_gh_fallback() {
        // The bug this replaces: `pr_by_head(..).ok()` turned Some(None) into a
        // satisfied `if let`, so "the API said there is no PR" and "the API failed"
        // both returned Ok(None) — one of them without ever consulting `gh`.
        assert_eq!(decide_fallback(&HeadLookup::NoMatch), Fallback::No);
        assert_eq!(
            decide_fallback(&HeadLookup::Unavailable("no token".into())),
            Fallback::Yes
        );
        assert_eq!(decide_fallback(&HeadLookup::Unique(brief(7))), Fallback::No);
        assert_eq!(
            decide_fallback(&HeadLookup::Ambiguous(vec![brief(7), brief(8)])),
            Fallback::No
        );
    }

    #[test]
    fn an_ambiguous_lookup_refuses_on_an_acting_path() {
        let err = resolve_acting(&HeadLookup::Ambiguous(vec![brief(7), brief(8)]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("#7") && err.contains("#8"), "{err}");
    }

    fn brief(n: u64) -> devkit_common::github::PrBrief {
        devkit_common::github::PrBrief {
            number: n,
            state: "OPEN".into(),
            url: format!("https://github.com/o/r/pull/{n}"),
            head_ref_name: "feat/x".into(),
            head_ref_oid: "cafe1".into(),
            head_repo_owner: None,
        }
    }
}
