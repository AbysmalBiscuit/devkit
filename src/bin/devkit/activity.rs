//! `devkit activity`: subagent runs and the time todos were held, over a date
//! range, from the activity log.

use std::{collections::BTreeMap, time::SystemTime};

use anyhow::{Result, anyhow};
use chrono::{DateTime, NaiveDate, TimeDelta, Utc};
use clap::Args;
use devkit_todo::activity::{Activity, ActivityLog, ClaimEnd, Interval, Run, RunEnd, stamp};
use serde::Serialize;

#[derive(Args)]
pub struct ActivityCli {
    /// Start of the range: a date (2026-10-01, midnight UTC) or an RFC 3339
    /// time. Defaults to seven days before now.
    #[arg(long, value_parser = parse_time)]
    since: Option<DateTime<Utc>>,
    /// End of the range, in the same forms. Defaults to now.
    #[arg(long, value_parser = parse_time)]
    until: Option<DateTime<Utc>>,
    /// Emit the report as JSON.
    #[arg(long)]
    json: bool,
}

fn parse_time(s: &str) -> Result<DateTime<Utc>> {
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(date.and_time(chrono::NaiveTime::MIN).and_utc());
    }
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.to_utc())
        .map_err(|_| anyhow!("expected a date like 2026-10-01 or an RFC 3339 time, got {s:?}"))
}

pub fn run(cli: ActivityCli) -> Result<()> {
    let now: DateTime<Utc> = SystemTime::now().into();
    let range = Range {
        since: cli.since.unwrap_or(now - TimeDelta::days(7)),
        until: cli.until.unwrap_or(now),
        now,
    };
    let report = Report::of(ActivityLog::open().read(now)?, &range);
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", report.text());
    }
    Ok(())
}

struct Range {
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    now: DateTime<Utc>,
}

impl Range {
    /// Whole seconds of `start..end` inside the range, an open end running to
    /// now. `None` when none of it is.
    fn seconds(&self, start: DateTime<Utc>, end: Option<DateTime<Utc>>) -> Option<i64> {
        let end = end.unwrap_or(self.now).min(self.until);
        let start = start.max(self.since);
        (start <= end).then(|| (end - start).num_seconds())
    }
}

#[derive(Serialize)]
struct Report {
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    sessions: Vec<SessionRuns>,
    agent_types: Vec<TypeTotal>,
    todos: Vec<TodoHeld>,
}

#[derive(Serialize)]
struct SessionRuns {
    session: String,
    runs: Vec<RunRow>,
}

#[derive(Serialize)]
struct RunRow {
    #[serde(flatten)]
    run: Run,
    /// Seconds of the run inside the range.
    seconds: i64,
}

#[derive(Serialize)]
struct TypeTotal {
    agent_type: String,
    runs: usize,
    seconds: i64,
}

#[derive(Serialize)]
struct TodoHeld {
    todo: String,
    /// The node of its latest interval.
    node: String,
    seconds: i64,
    intervals: Vec<IntervalRow>,
}

#[derive(Serialize)]
struct IntervalRow {
    /// The node when the claim started.
    node: String,
    holder: String,
    start: DateTime<Utc>,
    end: Option<DateTime<Utc>>,
    outcome: Option<ClaimEnd>,
    seconds: i64,
}

impl Report {
    fn of(activity: Activity, range: &Range) -> Self {
        let Activity {
            mut runs,
            mut claims,
        } = activity;
        runs.sort_by_key(|r| r.start);
        claims.sort_by_key(|c| c.start);

        let mut sessions: Vec<SessionRuns> = Vec::new();
        let mut types: BTreeMap<String, TypeTotal> = BTreeMap::new();
        for run in runs {
            let Some(seconds) = range.seconds(run.start, run.end) else {
                continue;
            };
            let total = types.entry(run.label().to_string()).or_insert(TypeTotal {
                agent_type: run.label().to_string(),
                runs: 0,
                seconds: 0,
            });
            total.runs += 1;
            total.seconds += seconds;
            let row = RunRow { run, seconds };
            match sessions.iter_mut().find(|s| s.session == row.run.session) {
                Some(session) => session.runs.push(row),
                None => sessions.push(SessionRuns {
                    session: row.run.session.clone(),
                    runs: vec![row],
                }),
            }
        }
        let mut agent_types: Vec<TypeTotal> = types.into_values().collect();
        agent_types.sort_by_key(|t| std::cmp::Reverse(t.seconds));

        let mut todos: Vec<TodoHeld> = Vec::new();
        for Interval {
            todo,
            node,
            holder,
            start,
            end,
            outcome,
        } in claims
        {
            let Some(seconds) = range.seconds(start, end) else {
                continue;
            };
            let row = IntervalRow {
                node: node.clone(),
                holder: holder.to_string(),
                start,
                end,
                outcome,
                seconds,
            };
            match todos.iter_mut().find(|t| t.todo == todo) {
                Some(held) => {
                    held.node = node;
                    held.seconds += seconds;
                    held.intervals.push(row);
                }
                None => todos.push(TodoHeld {
                    todo,
                    node,
                    seconds,
                    intervals: vec![row],
                }),
            }
        }
        todos.sort_by_key(|t| std::cmp::Reverse(t.seconds));

        Self {
            since: range.since,
            until: range.until,
            sessions,
            agent_types,
            todos,
        }
    }

    fn text(&self) -> String {
        let mut out = format!("activity {} to {}\n", stamp(self.since), stamp(self.until));
        if self.sessions.is_empty() && self.todos.is_empty() {
            out.push_str("\nnothing recorded in this range\n");
            return out;
        }
        for session in &self.sessions {
            out.push_str(&format!("\nsession {}\n", session.session));
            let mut t = devkit_common::ui::table(&["TYPE", "AGENT", "START", "TIME", "OUTCOME"]);
            for RunRow { run, seconds } in &session.runs {
                t.add_row([
                    run.label().to_string(),
                    run.agent.clone(),
                    stamp(run.start),
                    duration(*seconds),
                    outcome(run.outcome),
                ]);
            }
            out.push_str(&format!("{t}\n"));
        }
        if !self.agent_types.is_empty() {
            out.push_str("\nby agent type\n");
            let mut t = devkit_common::ui::table(&["TYPE", "RUNS", "TIME"]);
            for total in &self.agent_types {
                t.add_row([
                    total.agent_type.clone(),
                    total.runs.to_string(),
                    duration(total.seconds),
                ]);
            }
            out.push_str(&format!("{t}\n"));
        }
        if !self.todos.is_empty() {
            out.push_str("\ntodos held\n");
            let mut t = devkit_common::ui::table(&["TODO", "NODE", "HELD", "HOLDERS"]);
            for held in &self.todos {
                let mut holders: Vec<&str> =
                    held.intervals.iter().map(|i| i.holder.as_str()).collect();
                holders.dedup();
                t.add_row([
                    devkit_todo::short_id(&held.todo).to_string(),
                    held.node.clone(),
                    duration(held.seconds),
                    holders.join(", "),
                ]);
            }
            out.push_str(&format!("{t}\n"));
        }
        out
    }
}

/// How a run ended, or `running` while it runs.
fn outcome(end: Option<RunEnd>) -> String {
    end.map_or("running", RunEnd::as_str).to_string()
}

/// `seconds` as `1h 2m`, `5m 2s` or `12s`.
fn duration(seconds: i64) -> String {
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    match (h, m) {
        (0, 0) => format!("{s}s"),
        (0, _) => format!("{m}m {s}s"),
        _ => format!("{h}h {m}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(minutes: i64) -> DateTime<Utc> {
        "2026-10-01T12:00:00Z".parse::<DateTime<Utc>>().unwrap() + TimeDelta::minutes(minutes)
    }

    fn held(node: &str, start: i64, end: i64) -> Interval {
        Interval {
            todo: "1".into(),
            node: node.into(),
            holder: devkit_todo::Holder::new("S"),
            start: t(start),
            end: Some(t(end)),
            outcome: Some(ClaimEnd::Released),
        }
    }

    #[test]
    fn each_interval_keeps_its_node_and_the_todo_shows_its_latest() {
        let activity = Activity {
            runs: Vec::new(),
            claims: vec![held("r.later", 10, 15), held("r.first", 0, 5)],
        };
        let range = Range {
            since: t(-60),
            until: t(60),
            now: t(60),
        };
        let report = Report::of(activity, &range);
        let todo = &report.todos[0];
        let nodes: Vec<&str> = todo.intervals.iter().map(|i| i.node.as_str()).collect();
        assert_eq!(nodes, ["r.first", "r.later"]);
        assert_eq!(todo.node, "r.later");
        let text = report.text();
        assert!(
            text.contains("r.later") && !text.contains("r.first"),
            "{text}"
        );
    }

    #[test]
    fn durations_read_at_a_glance() {
        assert_eq!(duration(12), "12s");
        assert_eq!(duration(302), "5m 2s");
        assert_eq!(duration(3720), "1h 2m");
    }

    #[test]
    fn a_date_is_midnight_utc() {
        assert_eq!(
            parse_time("2026-10-01").unwrap(),
            "2026-10-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert!(parse_time("yesterday").is_err());
    }
}
