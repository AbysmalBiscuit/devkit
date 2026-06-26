use anyhow::{Context, Result, bail};
use devkit_common::progress::Steps;
use devkit_common::slack;
use devkit_ports::config::Person;
use std::collections::{BTreeMap, HashMap};

pub(crate) mod finish;
pub(crate) mod request;

/// What to do given an existing PR's state (or none) for the current branch.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PrAction {
    Create,
    AddReviewer,
    Stop(String),
}

/// A resolved Slack recipient: a person alias or a `#channel`.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    /// Conversation id/name passed to `chat.postMessage`.
    pub channel: String,
    /// Template `{{ name }}`: the people alias, or the channel string.
    pub name: String,
    /// Template `{{ slack_id }}`: the person's user id; `None` for a channel.
    pub slack_id: Option<String>,
    /// GitHub login, when this is a person with a `github` handle.
    pub github: Option<String>,
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

/// Reject an empty rendered PR title on the create path.
pub(crate) fn require_pr_title(title: &str) -> Result<()> {
    if title.trim().is_empty() {
        bail!("--pr-title is required to create a PR");
    }
    Ok(())
}

/// Build a `Target` from a `[people]` alias and its entry.
pub(crate) fn target_from_person(alias: &str, p: &Person) -> Target {
    Target {
        channel: p.slack.clone(),
        name: alias.to_string(),
        slack_id: Some(p.slack.clone()),
        github: p.github.clone(),
    }
}

/// Classify one `--to` value. `#…` is a channel (Slack-only); anything else
/// must be a `[people]` alias.
pub(crate) fn resolve_target(value: &str, people: &HashMap<String, Person>) -> Result<Target> {
    if value.starts_with('#') {
        return Ok(Target {
            channel: value.to_string(),
            name: value.to_string(),
            slack_id: None,
            github: None,
        });
    }
    let p = people.get(value).with_context(|| {
        format!("unknown person alias `{value}` — add it under [people] in devkit.toml, or use `#channel`")
    })?;
    Ok(target_from_person(value, p))
}

/// Find the `[people]` alias + entry whose github login matches (case-insensitive).
pub(crate) fn person_by_login<'a>(
    login: &str,
    people: &'a HashMap<String, Person>,
) -> Option<(&'a String, &'a Person)> {
    people.iter().find(|(_, p)| {
        p.github
            .as_deref()
            .is_some_and(|g| g.eq_ignore_ascii_case(login))
    })
}

/// A GitHub login is a human reviewer unless it is an app/bot (`name[bot]`).
pub(crate) fn is_human_login(login: &str) -> bool {
    !login.ends_with("[bot]")
}

/// Parse repeated `--arg key=value` pairs, validating each key against the
/// declared `[templates.variables]` allowlist.
pub(crate) fn parse_args(
    pairs: &[String],
    allowed: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for pair in pairs {
        let (k, v) = pair
            .split_once('=')
            .with_context(|| format!("--arg must be key=value, got `{pair}`"))?;
        if !allowed.contains_key(k) {
            bail!("--arg `{k}` is not declared in [templates.variables]");
        }
        out.insert(k.to_string(), v.to_string());
    }
    Ok(out)
}

/// Clone `base` and add extra fields for a single template render.
pub(crate) fn with_fields(
    base: &serde_json::Value,
    extra: &[(&str, serde_json::Value)],
) -> serde_json::Value {
    let mut m = base.as_object().cloned().unwrap_or_default();
    for (k, v) in extra {
        m.insert((*k).into(), v.clone());
    }
    serde_json::Value::Object(m)
}

/// Base context shared by every review template: branch + issue record fields.
pub(crate) fn base_ctx(
    record: Option<&crate::record::IssueRecord>,
    branch: &str,
) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert("branch".into(), serde_json::json!(branch));
    if let Some(r) = record {
        m.insert("issue".into(), serde_json::json!(r.issue));
        m.insert("slug".into(), serde_json::json!(r.slug));
        m.insert("apps".into(), serde_json::json!(r.apps));
    }
    serde_json::Value::Object(m)
}

/// Per-recipient context: `name` + `slack_id` bound on top of `base`.
pub(crate) fn recipient_ctx(base: &serde_json::Value, t: &Target) -> serde_json::Value {
    with_fields(
        base,
        &[
            ("name", serde_json::json!(t.name)),
            (
                "slack_id",
                serde_json::json!(t.slack_id.clone().unwrap_or_default()),
            ),
        ],
    )
}

/// Render a review template, attaching the template key and, when the setup
/// record is absent, a hint that an `{{ issue }}`/`{{ slug }}` reference needs it.
pub(crate) fn render_review(
    tmpl: &str,
    key: &str,
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
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

/// Render `tmpl` once per target (binding `name`/`slack_id`) and Slack each.
/// With no `SLACK_TOKEN`, print the resolved intents instead.
pub(crate) fn deliver(
    tmpl: &str,
    key: &str,
    base: &serde_json::Value,
    vars: &BTreeMap<String, String>,
    missing_at: Option<&str>,
    targets: &[Target],
    steps: &Steps,
) -> Result<()> {
    let token = devkit_common::secrets::resolve("SLACK_TOKEN");
    for t in targets {
        let ctx = recipient_ctx(base, t);
        let text = render_review(tmpl, key, &ctx, vars, missing_at)?;
        match &token {
            Some(tok) => {
                steps.during(&format!("Notifying {} on Slack…", t.name), || {
                    slack::post_message(tok, &t.channel, &text)
                })?;
                println!("Sent to {} ({})", t.name, t.channel);
            }
            None => {
                let intent =
                    serde_json::json!({ "to": t.name, "channel": t.channel, "text": text });
                println!("{}", serde_json::to_string_pretty(&intent)?);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use devkit_ports::config::Person;
    use std::collections::{BTreeMap, HashMap};

    fn person(slack: &str, gh: Option<&str>) -> Person {
        Person {
            slack: slack.into(),
            github: gh.map(String::from),
        }
    }
    fn people() -> HashMap<String, Person> {
        HashMap::from([
            ("lev".to_string(), person("U_LEV", Some("LevValle"))),
            ("igor".to_string(), person("U_IGOR", None)),
        ])
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
    fn require_pr_title_rejects_empty() {
        assert!(require_pr_title("  ").is_err());
        assert!(require_pr_title("Fix login").is_ok());
    }

    #[test]
    fn default_review_request_appends_url() {
        let t = devkit_ports::config::Templates::default();
        let ctx = serde_json::json!({"input": "please review", "pr_url": "https://gh/pr/1"});
        let out = devkit_common::template::render(t.review_request(), &ctx, &t.variables).unwrap();
        assert_eq!(out, "please review https://gh/pr/1");
    }

    #[test]
    fn resolve_target_classifies_channel_person_and_unknown() {
        let p = people();
        let chan = resolve_target("#eng", &p).unwrap();
        assert_eq!(chan.channel, "#eng");
        assert_eq!(chan.name, "#eng");
        assert!(chan.slack_id.is_none());
        assert!(chan.github.is_none());

        let lev = resolve_target("lev", &p).unwrap();
        assert_eq!(lev.channel, "U_LEV");
        assert_eq!(lev.name, "lev");
        assert_eq!(lev.slack_id.as_deref(), Some("U_LEV"));
        assert_eq!(lev.github.as_deref(), Some("LevValle"));

        assert!(resolve_target("nobody", &p).is_err());
    }

    #[test]
    fn person_by_login_is_case_insensitive() {
        let p = people();
        let (alias, _) = person_by_login("levvalle", &p).unwrap();
        assert_eq!(alias, "lev");
        assert!(person_by_login("ghost", &p).is_none());
    }

    #[test]
    fn is_human_login_excludes_bots() {
        assert!(is_human_login("LevValle"));
        assert!(!is_human_login("coderabbitai[bot]"));
    }

    #[test]
    fn parse_args_validates_against_allowlist() {
        let allowed = BTreeMap::from([("team".to_string(), "platform".to_string())]);
        let ok = parse_args(&["team=infra".to_string()], &allowed).unwrap();
        assert_eq!(ok.get("team").map(String::as_str), Some("infra"));
        assert!(parse_args(&["ghost=x".to_string()], &allowed).is_err());
        assert!(parse_args(&["noeq".to_string()], &allowed).is_err());
    }

    #[test]
    fn recipient_ctx_binds_name_and_slack_id() {
        let base = serde_json::json!({"pr_url": "u"});
        let person_t = Target {
            channel: "U_LEV".into(),
            name: "lev".into(),
            slack_id: Some("U_LEV".into()),
            github: Some("LevValle".into()),
        };
        let c = recipient_ctx(&base, &person_t);
        assert_eq!(c["name"], "lev");
        assert_eq!(c["slack_id"], "U_LEV");
        assert_eq!(c["pr_url"], "u");

        let chan_t = Target {
            channel: "#eng".into(),
            name: "#eng".into(),
            slack_id: None,
            github: None,
        };
        let c = recipient_ctx(&base, &chan_t);
        assert_eq!(c["name"], "#eng");
        assert_eq!(c["slack_id"], "");
    }
}
