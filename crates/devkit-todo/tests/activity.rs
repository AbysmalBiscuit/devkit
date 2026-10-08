use chrono::{DateTime, TimeDelta, Utc};
use devkit_todo::{
    Holder,
    activity::{
        ActivityLog, ActivityStore, BACKSTOP, Event, MAIN_AGENT, RunEnd, SessionState, What,
    },
};

fn t(minutes: i64) -> DateTime<Utc> {
    "2026-10-01T12:00:00Z".parse::<DateTime<Utc>>().unwrap() + TimeDelta::minutes(minutes)
}

fn start(log: &ActivityLog, at: DateTime<Utc>, agent: &str) {
    log.record(&Event {
        at,
        what: What::SubagentStart {
            session: "S".into(),
            agent: agent.into(),
            agent_type: Some("Explore".into()),
        },
    })
    .unwrap();
}

#[test]
fn a_silent_run_closes_as_lost_at_its_agents_last_hook_once_the_backstop_passes() {
    let dir = tempfile::tempdir().unwrap();
    let log = ActivityLog::at(dir.path().to_path_buf());
    start(&log, t(0), "a1");
    log.seen_at("S", "a1", t(5).into()).unwrap();

    let at_backstop = log.read(t(5) + BACKSTOP).unwrap();
    assert_eq!(at_backstop.runs[0].end, None, "{at_backstop:?}");

    let past = log.read(t(5) + BACKSTOP + TimeDelta::seconds(1)).unwrap();
    let run = &past.runs[0];
    assert_eq!(run.end, Some(t(5)), "{run:?}");
    assert_eq!(run.outcome, Some(RunEnd::Lost));
}

#[test]
fn no_run_stays_open_past_the_backstop() {
    let dir = tempfile::tempdir().unwrap();
    let log = ActivityLog::at(dir.path().to_path_buf());
    start(&log, t(0), "never-seen");
    start(&log, t(1), "seen");
    log.seen_at("S", "seen", t(40).into()).unwrap();

    let report = log.read(t(40) + BACKSTOP + TimeDelta::seconds(1)).unwrap();
    assert_eq!(report.runs.len(), 2);
    for run in &report.runs {
        assert_eq!(run.outcome, Some(RunEnd::Lost), "{run:?}");
    }
    let never_seen = report
        .runs
        .iter()
        .find(|r| r.agent == "never-seen")
        .unwrap();
    assert_eq!(never_seen.end, Some(t(0)));
}

#[test]
fn a_session_reads_ended_silent_or_active_by_its_last_sign_of_life() {
    let dir = tempfile::tempdir().unwrap();
    let log = ActivityLog::at(dir.path().to_path_buf());
    log.seen_at("ended.1", MAIN_AGENT, t(0).into()).unwrap();
    log.record(&Event {
        at: t(1),
        what: What::SessionEnd {
            session: "ended.1".into(),
        },
    })
    .unwrap();
    log.seen_at("silent.2", MAIN_AGENT, t(0).into()).unwrap();
    log.seen_at("active.3", MAIN_AGENT, t(0).into()).unwrap();
    log.seen_at("active.3", "a1", t(40).into()).unwrap();

    let now = t(40) + BACKSTOP;
    let activity = log.read(now).unwrap();
    let state = |session: &str| activity.session_state(&Holder::new(session));
    assert_eq!(state("ended.1"), Some(SessionState::Ended { at: t(1) }));
    assert_eq!(
        state("silent.2"),
        Some(SessionState::Silent { since: t(0) })
    );
    assert_eq!(state("active.3/a1"), Some(SessionState::Active));
    assert_eq!(state("unknown"), None);
}

#[test]
fn a_stop_row_without_an_agent_type_parses() {
    let row = r#"{"at":"2026-01-01T00:00:00Z","event":"subagent_stop","session":"s","agent":"a1"}"#;
    let event: Event = serde_json::from_str(row).unwrap();
    assert_eq!(event.what, What::SubagentStop {
        session: "s".into(),
        agent: "a1".into(),
        agent_type: None,
    });
}
