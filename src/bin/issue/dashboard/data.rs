use chrono::{DateTime, Utc};
use devkit_common::cmd::{capture, gh_json};
use devkit_common::linear::{self, AssignedIssue};
use serde::Deserialize;

use super::bucket::parse_ts;

/// Linear issues assigned to me, with history (empty if no key / on error).
pub fn issues() -> Vec<AssignedIssue> {
    let Ok(key) = std::env::var("LINEAR_API_KEY") else {
        return Vec::new();
    };
    match linear::assigned_issue_history(&key) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Linear history fetch failed: {e}");
            Vec::new()
        }
    }
}

/// Timeline origin: my Linear account creation, else the earliest issue createdAt.
pub fn origin(issues: &[AssignedIssue]) -> Option<DateTime<Utc>> {
    if let Ok(key) = std::env::var("LINEAR_API_KEY")
        && let Ok(s) = linear::viewer_created_at(&key)
        && let Some(d) = parse_ts(&s)
    {
        return Some(d);
    }
    issues.iter().filter_map(|i| parse_ts(&i.created_at)).min()
}

#[derive(Deserialize)]
struct PrTimes {
    #[serde(rename = "createdAt", default)]
    created_at: Option<String>,
    #[serde(rename = "mergedAt", default)]
    merged_at: Option<String>,
    #[serde(default)]
    additions: i64,
    #[serde(default)]
    deletions: i64,
}

/// (opened stamps, merged stamps, total additions, total deletions) for my PRs.
pub fn pr_timeline(all_roles: bool) -> (Vec<DateTime<Utc>>, Vec<DateTime<Utc>>, i64, i64) {
    let fetch = |search: &str| -> Vec<PrTimes> {
        gh_json(
            &[
                "pr",
                "list",
                "--search",
                search,
                "--state",
                "all",
                "--limit",
                "500",
                "--json",
                "createdAt,mergedAt,additions,deletions",
            ],
            ".",
        )
        .unwrap_or_default()
    };
    let mut prs = fetch("author:@me");
    if all_roles {
        prs.extend(fetch("reviewed-by:@me"));
    }
    let opened: Vec<_> = prs
        .iter()
        .filter_map(|p| p.created_at.as_deref().and_then(parse_ts))
        .collect();
    let merged: Vec<_> = prs
        .iter()
        .filter_map(|p| p.merged_at.as_deref().and_then(parse_ts))
        .collect();
    let add = prs.iter().map(|p| p.additions).sum();
    let del = prs.iter().map(|p| p.deletions).sum();
    (opened, merged, add, del)
}

/// Author-dates of every commit by `author` in `repo` (empty on error).
pub fn commit_dates(repo: &str, author: &str) -> Vec<DateTime<Utc>> {
    let out = match capture(
        "git",
        &[
            "-C",
            repo,
            "log",
            &format!("--author={author}"),
            "--format=%aI",
        ],
        None,
    ) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("commit history fetch failed for {repo}: {e}");
            return Vec::new();
        }
    };
    out.lines().filter_map(|l| parse_ts(l.trim())).collect()
}
