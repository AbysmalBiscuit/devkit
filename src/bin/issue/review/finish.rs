use anyhow::{Context, Result};
use devkit_common::cmd::{gh_json, git};
use devkit_ports::config::Person;
use serde::Deserialize;
use std::collections::HashMap;

use super::{
    Target, deliver, parse_args, person_by_login, resolve_target, target_from_person, with_fields,
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

    let mut vars = tmpls.variables.clone();
    vars.extend(parse_args(&args.args, &tmpls.variables)?);

    // PR from the current branch (best effort), unless --pr is given.
    let branch_pr = git(&["rev-parse", "--abbrev-ref", "HEAD"], &start)
        .ok()
        .and_then(|branch| {
            gh_json::<Vec<PrLite>>(
                &[
                    "pr",
                    "list",
                    "--head",
                    branch.trim(),
                    "--state",
                    "all",
                    "--json",
                    "number",
                    "--limit",
                    "1",
                ],
                &start,
            )
            .ok()
            .and_then(|v| v.into_iter().next())
            .map(|p| p.number)
        });
    let number = resolve_pr(branch_pr, args.pr)?;

    let view: PrFull = gh_json(
        &[
            "pr",
            "view",
            &number.to_string(),
            "--json",
            "url,title,author",
        ],
        &start,
    )?;
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

    let base = with_fields(
        &serde_json::json!({}),
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
        &base,
        &vars,
        None,
        &targets,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use devkit_ports::config::Person;
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
}
