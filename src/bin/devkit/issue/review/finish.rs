use std::collections::HashMap;

use anyhow::{Context, Result};
use devkit_common::{
    forge::{self, PrLocator},
    progress::Steps,
    vcs::{Vcs, VersionControl},
};
use devkit_config::Person;
use devkit_ports::templates::worktree_context;

use super::{
    REVIEW_FINISH_CONTEXT_KEYS, Target, check_required, deliver, parse_args, person_by_login,
    resolve_target, target_from_person, with_fields,
};
use crate::{
    issue::pr::resolve::{existing, resolve_acting, resolve_locator},
    template::VarArgs,
};

pub struct Args {
    pub body: Option<String>,
    pub to: Vec<String>,
    pub pr: Option<u64>,
    pub vars: VarArgs,
    pub dir: Option<String>,
    pub config: Option<String>,
}

/// The worktree branch's PR number, or the error naming `--pr`. Branch
/// discovery is the last resort: an explicit `--pr` and the record are both
/// resolved into a locator before this is reached.
pub(crate) fn resolve_pr(branch_pr: Option<u64>) -> Result<u64> {
    branch_pr.context("no PR for the current branch; pass --pr <number>")
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
    let forge = forge::resolve(&loaded.config.forge, &loaded.config.github, &start, None);
    let f = forge.forge.as_ref();
    let pr_repo = forge.repos.prs()?;

    let caller = devkit_common::caller::caller();
    let mut vars = tmpls.defaults();
    let given = parse_args(&args.vars, &tmpls.declared())?;
    check_required(
        "pr review finish",
        &loaded.config,
        &[tmpls.review_finish()],
        REVIEW_FINISH_CONTEXT_KEYS,
        &given,
        caller,
    )?;
    vars.extend(given);

    let steps = Steps::persistent();
    let here = std::path::Path::new(&start);
    let branch = Vcs::at(here).branch(here).ok();
    let record = devkit_common::vcs::checkout_root(std::path::Path::new(&start))
        .ok()
        .and_then(|top| devkit_common::record::read(&top));

    // Explicit `--pr`, then the record, then the worktree branch's PR. A
    // recorded locator can name a repository other than `pr_repo`.
    let explicit_loc = args.pr.map(|number| PrLocator { repo: None, number });
    let record_loc = record.as_ref().and_then(|r| r.pr.clone());
    let resolved_loc = resolve_locator(explicit_loc.as_ref(), record_loc.as_ref());
    let (number, repo) = match &resolved_loc {
        Some(loc) => (loc.number, loc.resolve(&forge.repos)?),
        None => {
            let branch_pr = match branch.as_deref() {
                Some(b) => steps.during_result("Looking up PR for branch...", || {
                    resolve_acting(&f.pr_by_head(pr_repo, b))
                })?,
                None => None,
            };
            (resolve_pr(branch_pr.map(|p| p.number))?, pr_repo.clone())
        }
    };

    // No head-oid gate here, unlike every other path that resolves a PR:
    // `review finish` is the reviewer's command, run in a worktree
    // `pr checkout` built, where `HEAD` goes stale the moment the author pushes
    // again. Requiring the PR's head to equal `HEAD` would refuse the ordinary
    // flow. Nothing here mutates the PR or the record — the effect is a Slack
    // message to the author.
    let view = steps.during_result(&format!("Fetching PR #{number}..."), || {
        existing(f, &repo, number)
    })?;
    let author_login = view.author_login;

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

    let base = worktree_context(record.as_ref(), Some(branch.as_deref().unwrap_or("")));
    let notify_ctx = with_fields(&base, &[
        ("pr_url", serde_json::json!(view.url)),
        ("pr_title", serde_json::json!(view.title)),
        (
            "author",
            serde_json::json!(author_login.unwrap_or_default()),
        ),
        ("input", serde_json::json!(args.body.unwrap_or_default())),
    ]);
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
    use std::collections::HashMap;

    use devkit_config::Person;

    use super::*;

    #[test]
    fn resolve_pr_takes_the_branchs_pr_or_names_the_flag() {
        assert_eq!(resolve_pr(Some(7)).unwrap(), 7);
        let err = resolve_pr(None).unwrap_err().to_string();
        assert!(err.contains("--pr"), "{err}");
    }

    #[test]
    fn author_target_reverse_looks_up_or_errors() {
        let people = HashMap::from([("lev".to_string(), Person {
            slack: "U_LEV".into(),
            github: Some("LevValle".into()),
        })]);
        let t = author_target("levvalle", &people).unwrap();
        assert_eq!(t.name, "lev");
        assert_eq!(t.channel, "U_LEV");
        assert!(author_target("ghost", &people).is_err());
    }
}
