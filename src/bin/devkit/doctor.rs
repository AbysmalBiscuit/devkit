use anyhow::Result;
use devkit_common::progress::Steps;
use devkit_common::secrets::{self, Source};
use devkit_common::{linear, slack};

#[derive(Debug, PartialEq, Eq)]
enum Check {
    Ok(String),
    Invalid(String),
    Unreachable,
    Unset(&'static str),
}

struct Row {
    key: &'static str,
    source: Source,
    check: Check,
}

const HINT_LINEAR: &str = "run: devkit auth linear   (https://linear.app/settings/api)";
const HINT_SLACK: &str = "run: devkit auth slack    (Slack app → OAuth & Permissions)";
const HINT_WORKSPACE: &str = "optional — falls back to the Linear API for issue links";

/// Exit non-zero only when a credential that is set fails validation. An unset
/// credential is a warning; an unreachable host is not a hard failure.
fn worst_exit(rows: &[Row]) -> i32 {
    if rows.iter().any(|r| matches!(r.check, Check::Invalid(_))) {
        1
    } else {
        0
    }
}

fn is_unreachable(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<ureq::Error>(),
        Some(ureq::Error::Transport(_))
    )
}

fn validate_linear(v: &str) -> Check {
    match linear::validate(v) {
        Ok(id) => Check::Ok(format!(
            "workspace \"{}\" ({})",
            id.workspace_url_key, id.viewer_email
        )),
        Err(e) if is_unreachable(&e) => Check::Unreachable,
        Err(e) => Check::Invalid(e.to_string()),
    }
}

fn validate_slack(v: &str) -> Check {
    match slack::validate(v) {
        Ok(id) => Check::Ok(format!("team \"{}\" (user {})", id.team, id.user)),
        Err(e) if is_unreachable(&e) => Check::Unreachable,
        Err(e) => Check::Invalid(e.to_string()),
    }
}

fn gather(steps: &Steps) -> Vec<Row> {
    vec![
        Row {
            key: "linear_api_key",
            source: secrets::source("LINEAR_API_KEY"),
            check: match secrets::resolve("LINEAR_API_KEY") {
                Some(v) => steps.during("Validating Linear API key…", || validate_linear(&v)),
                None => Check::Unset(HINT_LINEAR),
            },
        },
        Row {
            key: "linear_workspace",
            source: secrets::source("LINEAR_WORKSPACE"),
            check: match secrets::resolve("LINEAR_WORKSPACE") {
                Some(v) => Check::Ok(v),
                None => Check::Unset(HINT_WORKSPACE),
            },
        },
        Row {
            key: "slack_token",
            source: secrets::source("SLACK_TOKEN"),
            check: match secrets::resolve("SLACK_TOKEN") {
                Some(v) => steps.during("Validating Slack token…", || validate_slack(&v)),
                None => Check::Unset(HINT_SLACK),
            },
        },
    ]
}

fn source_label(s: &Source) -> &'static str {
    match s {
        Source::Env => "env",
        Source::File => "file",
        Source::Unset => "unset",
    }
}

fn print_human(rows: &[Row]) {
    for r in rows {
        let (mark, detail) = match &r.check {
            Check::Ok(d) => ("✓", d.clone()),
            Check::Invalid(d) => ("✗", d.clone()),
            Check::Unreachable => ("?", "unreachable".to_string()),
            Check::Unset(hint) => ("·", format!("unset — {hint}")),
        };
        println!("{mark} {:16} {:5} {detail}", r.key, source_label(&r.source));
    }
}

fn print_json(rows: &[Row]) {
    let arr: Vec<_> = rows
        .iter()
        .map(|r| {
            let (status, detail): (&str, Option<String>) = match &r.check {
                Check::Ok(d) => ("ok", Some(d.clone())),
                Check::Invalid(d) => ("invalid", Some(d.clone())),
                Check::Unreachable => ("unreachable", None),
                Check::Unset(h) => ("unset", Some((*h).to_string())),
            };
            serde_json::json!({
                "key": r.key,
                "source": source_label(&r.source),
                "status": status,
                "detail": detail,
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&arr).unwrap());
}

pub fn run(json: bool) -> Result<()> {
    let total = usize::from(secrets::resolve("LINEAR_API_KEY").is_some())
        + usize::from(secrets::resolve("SLACK_TOKEN").is_some());
    let steps = Steps::with_total(total);
    let rows = gather(&steps);
    steps.clear();
    if json {
        print_json(&rows);
    } else {
        print_human(&rows);
    }
    if worst_exit(&rows) != 0 {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(check: Check) -> Row {
        Row {
            key: "x",
            source: Source::Unset,
            check,
        }
    }

    #[test]
    fn invalid_fails_exit() {
        let rows = vec![
            row(Check::Ok("ok".into())),
            row(Check::Invalid("bad".into())),
        ];
        assert_eq!(worst_exit(&rows), 1);
    }

    #[test]
    fn unset_and_unreachable_pass_exit() {
        let rows = vec![
            row(Check::Unset("h")),
            row(Check::Unreachable),
            row(Check::Ok("ok".into())),
        ];
        assert_eq!(worst_exit(&rows), 0);
    }
}
