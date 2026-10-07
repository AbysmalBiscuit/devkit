use std::{sync::Arc, time::SystemTime};

use anyhow::Result;
use chrono::{DateTime, Utc};
use devkit_todo::activity::{Activity, ActivityStore, Event, Snapshot, What};
use serde_json::{Value, json};

use crate::Api;

/// Subagent runs and claim intervals kept in the postgres backend's tables
/// under one root, reached over a Supabase project's Data API.
///
/// Every write and the read is one call to the database function the
/// postgres backend calls, so machines on either backend report into one log.
pub struct SupabaseActivity {
    api: Arc<Api>,
    root: String,
}

impl SupabaseActivity {
    pub fn new(api: Arc<Api>, root: impl Into<String>) -> Self {
        Self {
            api,
            root: root.into(),
        }
    }

    /// Appends `lines`, a JSON array of log lines, those without an `at`
    /// stamped by the database's clock.
    fn append(&self, lines: Value) -> Result<()> {
        self.api.call(
            "activity_record",
            &json!({ "p_root": self.root, "p_events": lines }),
        )?;
        Ok(())
    }

    fn mark(&self, session: &str, agent: &str, at: Option<DateTime<Utc>>) -> Result<()> {
        self.api.call(
            "activity_seen",
            &json!({ "p_root": self.root, "p_session": session, "p_agent": agent, "p_at": at }),
        )?;
        Ok(())
    }

    /// Every run and interval as of `now`, or as of the call by the
    /// database's clock when `None`, read from one snapshot.
    fn read_as_of(&self, now: Option<DateTime<Utc>>) -> Result<Activity> {
        let resp = self.api.call(
            "activity_read",
            &json!({ "p_root": self.root, "p_now": now }),
        )?;
        Ok(self.api.body::<Snapshot>(resp)?.into())
    }
}

impl ActivityStore for SupabaseActivity {
    fn record(&self, event: &Event) -> Result<()> {
        self.record_all(std::slice::from_ref(event))
    }

    /// One request for the whole batch, each event keeping its own time.
    fn record_all(&self, events: &[Event]) -> Result<()> {
        self.append(serde_json::to_value(events)?)
    }

    fn record_now(&self, what: &What) -> Result<()> {
        self.append(serde_json::to_value([what])?)
    }

    fn seen(&self, session: &str, agent: &str) -> Result<()> {
        self.mark(session, agent, None)
    }

    fn seen_at(&self, session: &str, agent: &str, when: SystemTime) -> Result<()> {
        self.mark(session, agent, Some(when.into()))
    }

    fn forget(&self, session: &str, agent: Option<&str>) -> Result<()> {
        self.api.call(
            "activity_forget",
            &json!({ "p_root": self.root, "p_session": session, "p_agent": agent }),
        )?;
        Ok(())
    }

    /// An event whose payload does not parse is skipped.
    fn read(&self, now: DateTime<Utc>) -> Result<Activity> {
        self.read_as_of(Some(now))
    }

    fn read_now(&self) -> Result<Activity> {
        self.read_as_of(None)
    }
}
