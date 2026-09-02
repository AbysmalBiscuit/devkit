use anyhow::{Context, Result};
use devkit_common::cmd::gh_json_in;
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

/// Refuse a `gh pr list` fallback result naming more than one PR. Both
/// fallback call sites dropped `--limit 1` so they can see a second
/// candidate instead of silently taking whichever came first.
pub(crate) fn ensure_unambiguous_gh_match(matches: usize) -> Result<()> {
    anyhow::ensure!(
        matches <= 1,
        "several PRs share this head branch — pass --pr to choose one"
    );
    Ok(())
}

/// PR number for head branch `b`, over direct HTTP when a token is available,
/// else `gh pr list`. `Ok(None)` means no PR (whichever path answered).
fn branch_pr_number(b: &str, cwd: &str, repo: &github::Repo) -> Result<Option<u64>> {
    let looked = github::pr_by_head(repo, b);
    if decide_fallback(&looked) == Fallback::No {
        return Ok(resolve_acting(&looked)?.map(|p| p.number));
    }
    let v: Vec<PrLite> = gh_json_in(
        &[
            "pr", "list", "--head", b, "--state", "all", "--json", "number",
        ],
        repo,
        cwd,
    )?;
    ensure_unambiguous_gh_match(v.len())?;
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

/// The worktree branch's PR number, or the error naming `--pr`. Branch
/// discovery is the last resort: an explicit `--pr` and the record are both
/// resolved into a locator before this is reached.
pub(crate) fn resolve_pr(branch_pr: Option<u64>) -> Result<u64> {
    branch_pr.context("no PR for the current branch; pass --pr <number>")
}

/// Explicit locator, then the record, then branch discovery. `--pr` means one
/// thing everywhere — use this PR for this run — and does not itself write
/// anything; `review request` recording what it acted on is what makes it a
/// rebind.
pub(crate) fn resolve_locator(
    explicit: Option<&github::PrLocator>,
    record: Option<&github::PrLocator>,
) -> Option<github::PrLocator> {
    explicit.or(record).cloned()
}

/// A PR entering an acting path must carry this worktree's commits. How it was
/// chosen does not change what it can do: a branch-discovered `Unique` is
/// unique only among one repository's PRs, so another fork's same-named branch
/// gives the identical answer.
///
/// `headRefOid` is the branch head the PR carries, not the commit that landed
/// on the base, so a squashed or rebased merge still compares equal.
pub(crate) fn assert_belongs(pr: &github::PrBrief, head: &str) -> Result<()> {
    anyhow::ensure!(
        pr.head_ref_oid == head,
        "PR #{} is at {} but this worktree is at {head} — it does not carry this work",
        pr.number,
        pr.head_ref_oid
    );
    Ok(())
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
    let branch = devkit_common::git::branch(std::path::Path::new(&start)).ok();
    let record = devkit_common::git::checkout_root(std::path::Path::new(&start))
        .ok()
        .and_then(|top| devkit_common::record::read(&top));

    // Explicit `--pr`, then the record, then the worktree branch's PR (best
    // effort). A recorded locator can name a repository other than `pr_repo`.
    let explicit_loc = args
        .pr
        .map(|number| github::PrLocator { repo: None, number });
    let record_loc = record.as_ref().and_then(|r| r.pr.clone());
    let resolved_loc = resolve_locator(explicit_loc.as_ref(), record_loc.as_ref());
    let (number, repo): (u64, github::Repo) = match &resolved_loc {
        Some(loc) => (loc.number, loc.resolve(&repos)?),
        None => {
            let branch_pr = branch.as_deref().and_then(|b| {
                steps
                    .during_result("Looking up PR for branch…", || {
                        branch_pr_number(b, &start, pr_repo)
                    })
                    .ok()
                    .flatten()
            });
            (resolve_pr(branch_pr)?, pr_repo.clone())
        }
    };

    // No head-oid gate here, unlike every other path that resolves a PR:
    // `review finish` is the reviewer's command, run in a worktree
    // `checkout-pr` built, where `HEAD` goes stale the moment the author pushes
    // again. Requiring the PR's head to equal `HEAD` would refuse the ordinary
    // flow. Nothing here mutates the PR or the record — the effect is a Slack
    // message to the author.
    let view: PrFull = steps.during_result(&format!("Fetching PR #{number}…"), || {
        fetch_pr_full(number, &start, &repo)
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
    fn resolve_pr_takes_the_branchs_pr_or_names_the_flag() {
        assert_eq!(resolve_pr(Some(7)).unwrap(), 7);
        let err = resolve_pr(None).unwrap_err().to_string();
        assert!(err.contains("--pr"), "{err}");
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
            is_draft: false,
        }
    }

    fn loc(repo: Option<&str>, number: u64) -> github::PrLocator {
        github::PrLocator {
            repo: repo.map(str::to_string),
            number,
        }
    }

    fn brief_at(oid: &str) -> devkit_common::github::PrBrief {
        devkit_common::github::PrBrief {
            number: 5,
            state: "OPEN".into(),
            url: "https://github.com/o/r/pull/5".into(),
            head_ref_name: "feat/x".into(),
            head_ref_oid: oid.into(),
            head_repo_owner: None,
            is_draft: false,
        }
    }

    #[test]
    fn precedence_is_explicit_then_record_then_branch() {
        // review finish --pr wins over branch discovery by contract today. Making
        // the record unconditionally authoritative would either disable that flag
        // silently or leave an undocumented way around the new rule.
        let ex = loc(None, 7);
        let rec = loc(Some("up/app"), 9);
        assert_eq!(resolve_locator(Some(&ex), Some(&rec)), Some(ex.clone()));
        assert_eq!(resolve_locator(None, Some(&rec)), Some(rec));
        assert_eq!(resolve_locator(None, None), None); // branch discovery
    }

    #[test]
    fn a_pr_that_is_not_this_worktrees_head_is_refused() {
        // --pr with a mistyped number names a real PR that resolves cleanly, the
        // record makes it authoritative, and its merge lets issue end run
        // `git branch -D` on a worktree whose work never landed.
        let pr = brief_at("cafe1234");
        assert!(assert_belongs(&pr, "cafe1234").is_ok());
        let err = assert_belongs(&pr, "beef5678").unwrap_err().to_string();
        assert!(
            err.contains("cafe1234") && err.contains("beef5678"),
            "{err}"
        );
    }
}
