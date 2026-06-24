use chrono::{DateTime, Utc};
use devkit_common::cmd::{capture, gh_json};
use devkit_common::linear::{self, AssignedIssue};
use serde::{Deserialize, Serialize};

use super::bucket::parse_ts;
use super::cache;

/// How long a cached timeline fetch stays fresh. The timeline charts show
/// slow-moving trends, so a few minutes of staleness is invisible; the live
/// at-a-glance panel above them is never cached. `--no-cache` forces a refetch.
const TTL_SECS: u64 = 900;

/// Linear issues assigned to me, with history (empty if no key / on error).
/// With `use_cache`, a fresh prior fetch is reused; failures are never cached.
/// `on_page` is called after each page with the running total so the caller can
/// update a progress indicator.
pub fn issues(use_cache: bool, on_page: impl FnMut(usize)) -> Vec<AssignedIssue> {
    let Some(key) = devkit_common::secrets::resolve("LINEAR_API_KEY") else {
        return Vec::new();
    };
    if use_cache && let Some(v) = cache::get::<Vec<AssignedIssue>>("issues", TTL_SECS) {
        return v;
    }
    let v = match linear::assigned_issue_history_with_progress(&key, on_page) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Linear history fetch failed: {e}");
            Vec::new()
        }
    };
    if use_cache && !v.is_empty() {
        cache::put("issues", &v);
    }
    v
}

/// Timeline origin: my Linear account creation, else the earliest issue createdAt.
pub fn origin(issues: &[AssignedIssue]) -> Option<DateTime<Utc>> {
    if let Some(key) = devkit_common::secrets::resolve("LINEAR_API_KEY")
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

/// A `pr_timeline` result reduced to unix-second stamps so it serializes without
/// chrono's serde feature; reconstituted into `DateTime`s on the way back out.
#[derive(Serialize, Deserialize)]
struct PrTimelineCache {
    opened: Vec<i64>,
    merged: Vec<i64>,
    additions: i64,
    deletions: i64,
}

fn to_datetimes(stamps: &[i64]) -> Vec<DateTime<Utc>> {
    stamps
        .iter()
        .filter_map(|&s| DateTime::from_timestamp(s, 0))
        .collect()
}

/// (opened stamps, merged stamps, total additions, total deletions) for my PRs.
/// With `use_cache`, a fresh prior fetch is reused; failures are never cached.
pub fn pr_timeline(
    all_roles: bool,
    use_cache: bool,
) -> (Vec<DateTime<Utc>>, Vec<DateTime<Utc>>, i64, i64) {
    let key = if all_roles {
        "pr-timeline-all"
    } else {
        "pr-timeline-mine"
    };
    if use_cache && let Some(c) = cache::get::<PrTimelineCache>(key, TTL_SECS) {
        return (
            to_datetimes(&c.opened),
            to_datetimes(&c.merged),
            c.additions,
            c.deletions,
        );
    }
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
    let opened: Vec<DateTime<Utc>> = prs
        .iter()
        .filter_map(|p| p.created_at.as_deref().and_then(parse_ts))
        .collect();
    let merged: Vec<DateTime<Utc>> = prs
        .iter()
        .filter_map(|p| p.merged_at.as_deref().and_then(parse_ts))
        .collect();
    let add = prs.iter().map(|p| p.additions).sum();
    let del = prs.iter().map(|p| p.deletions).sum();
    if use_cache && !(opened.is_empty() && merged.is_empty()) {
        cache::put(
            key,
            &PrTimelineCache {
                opened: opened.iter().map(|d| d.timestamp()).collect(),
                merged: merged.iter().map(|d| d.timestamp()).collect(),
                additions: add,
                deletions: del,
            },
        );
    }
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
