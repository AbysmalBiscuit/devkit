use anyhow::{Context, Result, bail};
use devkit_common::cmd::{capture, gh_json, git};
use devkit_common::slack;
use devkit_ports::config::Person;
use serde::Deserialize;
use std::collections::HashMap;

/// What to do given an existing PR's state (or none) for the current branch.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PrAction {
    Create,
    AddReviewer,
    Stop(String),
}

/// Branches we must never open a review against.
pub(crate) fn guard_branch(branch: &str) -> Result<()> {
    if branch == "staging" || branch == "main" || branch == "HEAD" {
        bail!("refusing to review from base branch `{branch}` — switch to a feature branch");
    }
    Ok(())
}

/// Map a detected PR state to the next action.
pub(crate) fn action_for(pr_state: Option<&str>) -> PrAction {
    match pr_state {
        None => PrAction::Create,
        Some("OPEN") => PrAction::AddReviewer,
        Some("MERGED") => PrAction::Stop("PR already merged — nothing to review".into()),
        Some("CLOSED") => PrAction::Stop("PR is closed — nothing to review".into()),
        Some(other) => PrAction::Stop(format!("unexpected PR state `{other}`")),
    }
}

/// Resolve the GitHub reviewer handle: explicit flag wins, else the alias's github.
pub(crate) fn resolve_reviewer(explicit: Option<&str>, person: &Person) -> Result<String> {
    if let Some(r) = explicit {
        return Ok(r.to_string());
    }
    person
        .github
        .clone()
        .context("no --reviewer given and alias has no `github` handle")
}

/// Reject an empty rendered PR title on the create path.
pub(crate) fn require_pr_title(title: &str) -> Result<()> {
    if title.trim().is_empty() {
        bail!("--pr-title is required to create a PR");
    }
    Ok(())
}

/// Render a review template, attaching the template key and, when the setup
/// record is absent, a hint that an `{{ issue }}`/`{{ slug }}` reference needs it.
fn render_review(
    tmpl: &str,
    key: &str,
    ctx: &serde_json::Value,
    vars: &std::collections::BTreeMap<String, String>,
    missing_record_at: Option<&str>,
) -> Result<String> {
    let mut r = devkit_common::template::render(tmpl, ctx, vars)
        .with_context(|| format!("rendering `{key}` template"));
    if let Some(worktree) = missing_record_at {
        r = r.with_context(|| {
            format!("no .devkit/issue.toml found in {worktree} — was it created by `issue setup`?")
        });
    }
    r
}

/// Base context shared by every review template.
fn base_ctx(
    record: Option<&crate::record::IssueRecord>,
    branch: &str,
    reviewer: &str,
    to: &str,
) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert("branch".into(), serde_json::json!(branch));
    m.insert("reviewer".into(), serde_json::json!(reviewer));
    m.insert("to".into(), serde_json::json!(to));
    if let Some(r) = record {
        m.insert("issue".into(), serde_json::json!(r.issue));
        m.insert("slug".into(), serde_json::json!(r.slug));
        m.insert("apps".into(), serde_json::json!(r.apps));
    }
    serde_json::Value::Object(m)
}

/// Clone `base` and add extra fields for a single template render.
fn with_fields(base: &serde_json::Value, extra: &[(&str, serde_json::Value)]) -> serde_json::Value {
    let mut m = base.as_object().cloned().unwrap_or_default();
    for (k, v) in extra {
        m.insert((*k).into(), v.clone());
    }
    serde_json::Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn person(gh: Option<&str>) -> Person {
        Person {
            slack: "U1".into(),
            github: gh.map(String::from),
        }
    }
    #[test]
    fn guard_rejects_base_branches() {
        assert!(guard_branch("staging").is_err());
        assert!(guard_branch("main").is_err());
        assert!(guard_branch("lev/eng-1-fix").is_ok());
    }
    #[test]
    fn action_maps_pr_state() {
        assert_eq!(action_for(None), PrAction::Create);
        assert_eq!(action_for(Some("OPEN")), PrAction::AddReviewer);
        assert!(matches!(action_for(Some("MERGED")), PrAction::Stop(_)));
        assert!(matches!(action_for(Some("CLOSED")), PrAction::Stop(_)));
    }
    #[test]
    fn reviewer_prefers_explicit_then_alias() {
        assert_eq!(
            resolve_reviewer(Some("octocat"), &person(Some("exampleuser"))).unwrap(),
            "octocat"
        );
        assert_eq!(
            resolve_reviewer(None, &person(Some("exampleuser"))).unwrap(),
            "exampleuser"
        );
        assert!(resolve_reviewer(None, &person(None)).is_err());
    }
    #[test]
    fn default_slack_appends_url() {
        let t = devkit_ports::config::Templates::default();
        let ctx = serde_json::json!({"input": "please review", "pr_url": "https://gh/pr/1"});
        let out = devkit_common::template::render(t.slack(), &ctx, &t.variables).unwrap();
        assert_eq!(out, "please review https://gh/pr/1");
    }

    #[test]
    fn require_pr_title_rejects_empty() {
        assert!(require_pr_title("  ").is_err());
        assert!(require_pr_title("Fix login").is_ok());
    }
}

pub struct ReviewArgs {
    pub body: Option<String>,
    pub to: String,
    pub reviewer: Option<String>,
    pub base: Option<String>,
    pub pr_title: Option<String>,
    pub pr_body: Option<String>,
    pub no_push: bool,
    pub dir: Option<String>,
    pub config: Option<String>,
}

#[derive(Deserialize)]
struct PrView {
    number: u64,
    state: String,
    url: String,
}

#[derive(serde::Serialize)]
struct SlackIntent<'a> {
    slack_id: &'a str,
    text: &'a str,
    pr_url: &'a str,
    github: &'a str,
    branch: &'a str,
}

pub fn run(args: ReviewArgs) -> Result<()> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());
    let loaded = devkit_ports::load::load(
        args.config.as_deref().map(std::path::Path::new),
        std::path::Path::new(&start),
    )?;
    let people: &HashMap<String, Person> = &loaded.config.people;
    let person = people.get(&args.to).with_context(|| {
        format!(
            "unknown person alias `{}` — add it under [people] in devkit.toml",
            args.to
        )
    })?;
    let reviewer = resolve_reviewer(args.reviewer.as_deref(), person)?;

    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"], &start)?
        .trim()
        .to_string();
    guard_branch(&branch)?;

    if !args.no_push {
        // Never force-push; surface the rejection verbatim.
        git(&["push", "-u", "origin", &branch], &start)
            .context("git push failed (refusing to force-push)")?;
    }

    let toplevel = git(&["rev-parse", "--show-toplevel"], &start)?
        .trim()
        .to_string();
    let record = crate::record::read(std::path::Path::new(&toplevel));
    let missing_at = if record.is_none() { Some(toplevel.as_str()) } else { None };
    let tmpls = &loaded.config.templates;
    let vars = &tmpls.variables;
    let base = base_ctx(record.as_ref(), &branch, &reviewer, &args.to);

    let pr_title = render_review(
        tmpls.pr_title(),
        "pr_title",
        &with_fields(
            &base,
            &[("input", serde_json::json!(args.pr_title.clone().unwrap_or_default()))],
        ),
        vars,
        missing_at,
    )?;
    let pr_body = render_review(
        tmpls.pr_body(),
        "pr_body",
        &with_fields(
            &base,
            &[
                ("input", serde_json::json!(args.pr_body.clone().unwrap_or_default())),
                ("pr_title", serde_json::json!(pr_title)),
            ],
        ),
        vars,
        missing_at,
    )?;

    let existing: Option<PrView> = gh_json::<Vec<PrView>>(
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
    )?
    .into_iter()
    .next();

    let pr_url = match action_for(existing.as_ref().map(|p| p.state.as_str())) {
        PrAction::Stop(reason) => bail!("{reason}"),
        PrAction::AddReviewer => {
            let pr = existing.expect("AddReviewer implies an existing PR");
            capture(
                "gh",
                &[
                    "pr",
                    "edit",
                    &pr.number.to_string(),
                    "--add-reviewer",
                    &reviewer,
                ],
                Some(&start),
            )
            .context("gh pr edit --add-reviewer failed")?;
            pr.url
        }
        PrAction::Create => {
            require_pr_title(&pr_title)?;
            let base_branch = args
                .base
                .clone()
                .unwrap_or_else(|| loaded.config.defaults.pr_base.clone());
            let out = capture(
                "gh",
                &[
                    "pr",
                    "create",
                    "--base",
                    &base_branch,
                    "--reviewer",
                    &reviewer,
                    "--title",
                    &pr_title,
                    "--body",
                    &pr_body,
                ],
                Some(&start),
            )
            .context("gh pr create failed")?;
            out.lines()
                .rev()
                .find(|l| l.contains("://"))
                .context("could not parse a PR URL from `gh pr create` output")?
                .trim()
                .to_string()
        }
    };

    let text = render_review(
        tmpls.slack(),
        "slack",
        &with_fields(
            &base,
            &[
                ("input", serde_json::json!(args.body.clone().unwrap_or_default())),
                ("pr_title", serde_json::json!(pr_title)),
                ("pr_url", serde_json::json!(pr_url)),
            ],
        ),
        vars,
        missing_at,
    )?;

    match std::env::var("SLACK_TOKEN").ok().filter(|t| !t.is_empty()) {
        Some(token) => {
            slack::post_message(&token, &person.slack, &text)?;
            println!("Sent to {} ({}). PR: {pr_url}", args.to, person.slack);
        }
        None => {
            let intent = SlackIntent {
                slack_id: &person.slack,
                text: &text,
                pr_url: &pr_url,
                github: &reviewer,
                branch: &branch,
            };
            println!("{}", serde_json::to_string_pretty(&intent)?);
        }
    }
    Ok(())
}
