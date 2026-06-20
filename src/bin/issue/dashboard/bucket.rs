use chrono::{DateTime, Datelike, Duration, Months, NaiveDate, Utc};
use devkit_common::linear::{AssignedIssue, StateRef};
use std::collections::HashMap;

/// Parse an RFC3339 timestamp to UTC. Linear uses `…Z`; git `%aI` uses `+01:00`.
pub fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn midnight(d: DateTime<Utc>) -> DateTime<Utc> {
    d.date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc()
}

/// Period-start datetimes spanning first..=now for the chosen bucket.
pub fn bucket_starts(first: DateTime<Utc>, now: DateTime<Utc>, bucket: &str) -> Vec<DateTime<Utc>> {
    let start = match bucket {
        "day" => midnight(first),
        "month" => NaiveDate::from_ymd_opt(first.year(), first.month(), 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc(),
        _ => {
            let m = midnight(first);
            m - Duration::days(m.weekday().num_days_from_monday() as i64)
        }
    };
    let step = |d: DateTime<Utc>| match bucket {
        "day" => d + Duration::days(1),
        "month" => d + Months::new(1),
        _ => d + Duration::days(7),
    };
    let mut out = Vec::new();
    let mut cur = start;
    while cur <= now {
        out.push(cur);
        cur = step(cur);
    }
    out
}

/// Index of the period containing `t`, or None if `t` precedes the first period.
pub fn bucket_index(starts: &[DateTime<Utc>], t: DateTime<Utc>) -> Option<usize> {
    let mut idx = None;
    for (i, s) in starts.iter().enumerate() {
        if *s <= t {
            idx = Some(i);
        } else {
            break;
        }
    }
    idx
}

pub fn label_for(start: DateTime<Utc>, bucket: &str) -> String {
    if bucket == "month" {
        start.format("%b %Y").to_string()
    } else {
        start.format("%b %d").to_string()
    }
}

/// Finest bucket whose bar count fits `width`: day, else week, else month.
pub fn choose_bucket(first: DateTime<Utc>, now: DateTime<Utc>, width: usize) -> &'static str {
    let span_days = (now - first).num_days() + 1;
    // Each rendered bar occupies ~2 columns (a block glyph plus a separator),
    // and ~12 columns are reserved for the y-axis labels, so the number of bars
    // that fit is roughly (width - 12) / 2.
    let max_bars = std::cmp::max(8, width.saturating_sub(12) / 2) as i64;
    if span_days <= max_bars {
        "day"
    } else if span_days / 7 <= max_bars {
        "week"
    } else {
        "month"
    }
}

/// Tally timestamps into per-bucket counts.
pub fn tally(starts: &[DateTime<Utc>], dates: &[DateTime<Utc>]) -> Vec<u32> {
    let mut counts = vec![0u32; starts.len()];
    for d in dates {
        if let Some(i) = bucket_index(starts, *d) {
            counts[i] += 1;
        }
    }
    counts
}

// --- issue state replay ---------------------------------------------------------

/// A single issue reduced to: created time, the state before its first transition,
/// and its transitions sorted ascending by time.
pub struct Replay {
    pub created: Option<DateTime<Utc>>,
    pub initial: String,
    pub transitions: Vec<(DateTime<Utc>, String)>,
}

/// Build a `Replay` and record every state's (kind, color) into `meta`.
pub fn parse_issue(iss: &AssignedIssue, meta: &mut HashMap<String, (String, String)>) -> Replay {
    meta.entry(iss.state.name.clone())
        .or_insert((iss.state.kind.clone(), iss.state.color.clone()));
    let mut raw: Vec<(DateTime<Utc>, Option<String>, String)> = Vec::new();
    for (when, from, to) in &iss.history {
        for s in [from, to].into_iter().flatten() {
            meta.entry(s.name.clone())
                .or_insert((s.kind.clone(), s.color.clone()));
        }
        if let (Some(t), Some(to_state)) = (parse_ts(when), to) {
            raw.push((
                t,
                from.as_ref().map(|s: &StateRef| s.name.clone()),
                to_state.name.clone(),
            ));
        }
    }
    raw.sort_by_key(|x| x.0);
    let initial = raw
        .first()
        .and_then(|(_, f, _)| f.clone())
        .unwrap_or_else(|| iss.state.name.clone());
    Replay {
        created: parse_ts(&iss.created_at),
        initial,
        transitions: raw.into_iter().map(|(t, _, to)| (t, to)).collect(),
    }
}

/// The issue's workflow state as of time `t`, or None if not yet created.
pub fn state_at(r: &Replay, t: DateTime<Utc>) -> Option<String> {
    match r.created {
        Some(c) if c <= t => {}
        _ => return None,
    }
    let mut state = r.initial.clone();
    for (when, to) in &r.transitions {
        if *when <= t {
            state = to.clone();
        } else {
            break;
        }
    }
    Some(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dt(s: &str) -> DateTime<Utc> {
        parse_ts(s).unwrap()
    }

    #[test]
    fn daily_buckets_are_inclusive() {
        let starts = bucket_starts(
            dt("2026-01-01T08:00:00Z"),
            dt("2026-01-03T23:00:00Z"),
            "day",
        );
        assert_eq!(starts.len(), 3);
        assert_eq!(label_for(starts[0], "day"), "Jan 01");
    }
    #[test]
    fn weekly_buckets_anchor_on_monday() {
        // 2026-01-01 is a Thursday; the week start is Monday 2025-12-29.
        let starts = bucket_starts(
            dt("2026-01-01T00:00:00Z"),
            dt("2026-01-10T00:00:00Z"),
            "week",
        );
        assert_eq!(starts[0].weekday(), chrono::Weekday::Mon);
        assert_eq!(starts[0].format("%Y-%m-%d").to_string(), "2025-12-29");
    }
    #[test]
    fn monthly_steps_by_calendar_month() {
        let starts = bucket_starts(
            dt("2026-01-15T00:00:00Z"),
            dt("2026-03-02T00:00:00Z"),
            "month",
        );
        assert_eq!(starts.len(), 3);
        assert_eq!(label_for(starts[1], "month"), "Feb 2026");
    }
    #[test]
    fn bucket_index_before_first_is_none() {
        let starts = bucket_starts(
            dt("2026-01-02T00:00:00Z"),
            dt("2026-01-04T00:00:00Z"),
            "day",
        );
        assert_eq!(bucket_index(&starts, dt("2026-01-01T00:00:00Z")), None);
        assert_eq!(bucket_index(&starts, dt("2026-01-03T12:00:00Z")), Some(1));
    }
    #[test]
    fn choose_bucket_widens_with_span() {
        let first = dt("2026-01-01T00:00:00Z");
        // width 100 → max_bars = max(8, (100-12)/2) = 44.
        assert_eq!(choose_bucket(first, dt("2026-01-05T00:00:00Z"), 100), "day"); // 5-day span ≤ 44
        assert_eq!(
            choose_bucket(first, dt("2026-06-01T00:00:00Z"), 100),
            "week"
        ); // 152 days: >44 days, 22 weeks ≤ 44
        assert_eq!(
            choose_bucket(first, dt("2031-01-01T00:00:00Z"), 100),
            "month"
        );
    }
    #[test]
    fn tally_counts_per_bucket() {
        let starts = bucket_starts(
            dt("2026-01-01T00:00:00Z"),
            dt("2026-01-03T00:00:00Z"),
            "day",
        );
        let dates = vec![
            dt("2026-01-01T05:00:00Z"),
            dt("2026-01-01T09:00:00Z"),
            dt("2026-01-03T01:00:00Z"),
        ];
        assert_eq!(tally(&starts, &dates), vec![2, 0, 1]);
    }
    #[test]
    fn state_at_replays_transitions() {
        let iss = AssignedIssue {
            identifier: "ENG-1".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            state: StateRef {
                name: "Done".into(),
                kind: "completed".into(),
                color: "#0f0".into(),
            },
            history: vec![
                (
                    "2026-01-02T00:00:00Z".into(),
                    Some(StateRef {
                        name: "Todo".into(),
                        kind: "unstarted".into(),
                        color: "#888".into(),
                    }),
                    Some(StateRef {
                        name: "In Progress".into(),
                        kind: "started".into(),
                        color: "#00f".into(),
                    }),
                ),
                (
                    "2026-01-04T00:00:00Z".into(),
                    Some(StateRef {
                        name: "In Progress".into(),
                        kind: "started".into(),
                        color: "#00f".into(),
                    }),
                    Some(StateRef {
                        name: "Done".into(),
                        kind: "completed".into(),
                        color: "#0f0".into(),
                    }),
                ),
            ],
        };
        let mut meta = HashMap::new();
        let r = parse_issue(&iss, &mut meta);
        assert_eq!(r.initial, "Todo");
        assert_eq!(state_at(&r, dt("2025-12-31T00:00:00Z")), None); // before creation
        assert_eq!(
            state_at(&r, dt("2026-01-01T12:00:00Z")).as_deref(),
            Some("Todo")
        );
        assert_eq!(
            state_at(&r, dt("2026-01-03T00:00:00Z")).as_deref(),
            Some("In Progress")
        );
        assert_eq!(
            state_at(&r, dt("2026-01-05T00:00:00Z")).as_deref(),
            Some("Done")
        );
        assert!(meta.contains_key("In Progress"));
    }
}
