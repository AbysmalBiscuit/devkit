use chrono::{DateTime, Utc};
use devkit_common::cmd::{capture, gh_json_in};
use devkit_common::github;
use devkit_common::tracker::{AssignedIssue, Tracker};
use serde::{Deserialize, Serialize};

use super::bucket::parse_ts;
use super::cache;
use super::cache::CacheScope;

/// How long a cached timeline fetch stays fresh. The timeline charts show
/// slow-moving trends, so a few minutes of staleness is invisible; the live
/// at-a-glance panel above them is never cached. `--no-cache` forces a refetch.
const TTL_SECS: u64 = 900;

/// Issues assigned to me in `tracker`, with history (empty when the tracker
/// is not ready, or on error). With `use_cache`, a fresh prior fetch is
/// reused; failures are never cached. `on_page` is called after each page
/// with the running total so the caller can update a progress indicator.
pub fn issues(
    tracker: &dyn Tracker,
    scope: &CacheScope,
    use_cache: bool,
    mut on_page: impl FnMut(usize),
) -> Vec<AssignedIssue> {
    if use_cache && let Some(v) = cache::get::<Vec<AssignedIssue>>(scope, "issues", TTL_SECS) {
        return v;
    }
    let v = match tracker.assigned_history(&mut on_page) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("issue history fetch failed: {e}");
            Vec::new()
        }
    };
    if use_cache && !v.is_empty() {
        cache::put(scope, "issues", &v);
    }
    v
}

/// Timeline origin: `tracker`'s own answer (e.g. my Linear account creation),
/// else the earliest issue `createdAt`.
pub fn origin(tracker: &dyn Tracker, issues: &[AssignedIssue]) -> Option<DateTime<Utc>> {
    if let Ok(Some(s)) = tracker.timeline_origin()
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
/// `repo` is absent when no PR repository resolves (no `[github] pr_repo` and
/// no github.com `origin`, e.g. a Linear project whose code lives elsewhere) —
/// the section then degrades to empty rather than failing the dashboard.
pub fn pr_timeline(
    all_roles: bool,
    use_cache: bool,
    repo: Option<&github::Repo>,
    scope: &CacheScope,
) -> (Vec<DateTime<Utc>>, Vec<DateTime<Utc>>, i64, i64) {
    let key = if all_roles {
        "pr-timeline-all"
    } else {
        "pr-timeline-mine"
    };
    if use_cache && let Some(c) = cache::get::<PrTimelineCache>(scope, key, TTL_SECS) {
        return (
            to_datetimes(&c.opened),
            to_datetimes(&c.merged),
            c.additions,
            c.deletions,
        );
    }
    let Some(repo) = repo else {
        return (Vec::new(), Vec::new(), 0, 0);
    };
    let fetch = |search: &str| -> Vec<PrTimes> {
        if let Some(v) = fetch_timeline_http(search, repo) {
            return v;
        }
        gh_json_in(
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
            repo,
            ".",
        )
        .unwrap_or_default()
    };
    let prs = if all_roles {
        // The two GitHub queries are independent round trips; run them together.
        let (mut authored, reviewed) = std::thread::scope(|s| {
            let at = s.spawn(|| fetch("author:@me"));
            let rt = s.spawn(|| fetch("reviewed-by:@me"));
            (
                at.join().expect("author PR thread panicked"),
                rt.join().expect("reviewed PR thread panicked"),
            )
        });
        authored.extend(reviewed);
        authored
    } else {
        fetch("author:@me")
    };
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
            scope,
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

/// Timeline PRs for `qualifier` (`author:@me` / `reviewed-by:@me`) over direct
/// HTTP; `None` on no token / transport failure so the caller falls back to `gh`.
fn fetch_timeline_http(qualifier: &str, repo: &github::Repo) -> Option<Vec<PrTimes>> {
    github::token()?;
    let items = github::pr_timeline(&repo.slug, qualifier, 500).ok()?;
    Some(
        items
            .into_iter()
            .map(|t| PrTimes {
                created_at: t.created_at,
                merged_at: t.merged_at,
                additions: t.additions,
                deletions: t.deletions,
            })
            .collect(),
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    /// No PR repository resolved (no `[github] pr_repo`, no github.com
    /// `origin`) must degrade the section to empty rather than panic or
    /// reach for a `Repo` that doesn't exist — `use_cache: false` also rules
    /// out the cache short-circuit above it, so this exercises the `None`
    /// branch directly.
    #[test]
    fn pr_timeline_with_no_repo_degrades_to_empty() {
        let (opened, merged, add, del) = pr_timeline(true, false, None, &test_scope());
        assert!(opened.is_empty());
        assert!(merged.is_empty());
        assert_eq!(add, 0);
        assert_eq!(del, 0);
    }

    use devkit_common::tracker::fake::FakeTracker;
    use devkit_common::tracker::{State, StateKind};

    fn assigned(id: &str) -> AssignedIssue {
        AssignedIssue {
            identifier: id.to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            state: State {
                kind: StateKind::Started,
                name: "In Progress".to_string(),
                color: None,
            },
            history: Vec::new(),
        }
    }

    fn test_scope() -> cache::CacheScope {
        cache::CacheScope {
            tracker: devkit_common::tracker::TrackerKind::Linear,
            repo: String::new(),
            viewer: String::new(),
        }
    }

    /// `assigned_history` and `timeline_origin` had no caller anywhere outside
    /// the tracker module: this fetched Linear directly rather than going
    /// through the tracker devkit already resolved for the project.
    /// `use_cache: false` skips the real cache dir entirely.
    #[test]
    fn the_dashboard_reads_the_configured_tracker() {
        let t = FakeTracker::new().with_assigned(vec![assigned("ENG-1")]);
        let got = issues(&t, &test_scope(), false, |_| {});
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].identifier, "ENG-1");
    }

    #[test]
    fn origin_prefers_the_tracker_timeline_origin() {
        let t = FakeTracker::new().with_timeline_origin("2020-06-15T00:00:00Z");
        let got = origin(&t, &[]).expect("timeline origin parses");
        assert_eq!(got.to_rfc3339(), "2020-06-15T00:00:00+00:00");
    }
}
