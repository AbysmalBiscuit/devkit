use std::{sync::Arc, time::SystemTime};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use devkit_todo::activity::{Activity, ActivityStore, Event, Snapshot, What};
use tokio_postgres::types::{ToSql, Type};

use crate::Database;

/// Subagent runs and claim intervals kept in the database beside the todos,
/// under the same root, so every machine reports into one log.
///
/// Every write and the read is one call to a function in the `devkit` schema,
/// the same one the supabase backend calls, so both write the same records.
pub struct PostgresActivity {
    db: Arc<Database>,
    root: String,
}

impl PostgresActivity {
    pub fn new(db: Arc<Database>, root: impl Into<String>) -> Self {
        Self {
            db,
            root: root.into(),
        }
    }

    /// Runs `sql`, a call to an activity function that returns nothing,
    /// with the root as `$1` and `args` after it.
    fn call(&self, sql: &str, args: &[(&(dyn ToSql + Sync), Type)]) -> Result<()> {
        let mut params: Vec<(&(dyn ToSql + Sync), Type)> = vec![(&self.root, Type::TEXT)];
        params.extend_from_slice(args);
        self.db.run(async |client| {
            client.execute_typed(sql, &params).await?;
            Ok(())
        })
    }

    /// Appends `lines`, a JSON array of log lines, those without an `at`
    /// stamped by the database's clock.
    fn append(&self, lines: &serde_json::Value) -> Result<()> {
        let lines = lines.to_string();
        self.call("SELECT devkit.activity_record($1, $2::jsonb)", &[(
            &lines,
            Type::TEXT,
        )])
    }

    fn mark(&self, session: &str, agent: &str, at: Option<DateTime<Utc>>) -> Result<()> {
        self.call("SELECT devkit.activity_seen($1, $2, $3, $4)", &[
            (&session, Type::TEXT),
            (&agent, Type::TEXT),
            (&at, Type::TIMESTAMPTZ),
        ])
    }

    /// Every run and interval as of `now`, or as of the call by the
    /// database's clock when `None`, read from one snapshot.
    fn read_as_of(&self, now: Option<DateTime<Utc>>) -> Result<Activity> {
        let snapshot: String = self.db.run(async |client| {
            Ok(client
                .query_typed_one("SELECT devkit.activity_read($1, $2)::text", &[
                    (&self.root, Type::TEXT),
                    (&now, Type::TIMESTAMPTZ),
                ])
                .await?
                .get(0))
        })?;
        let snapshot: Snapshot =
            serde_json::from_str(&snapshot).context("reading the activity log")?;
        Ok(snapshot.into())
    }
}

impl ActivityStore for PostgresActivity {
    fn record(&self, event: &Event) -> Result<()> {
        self.record_all(std::slice::from_ref(event))
    }

    /// One call for the whole batch, each event keeping its own time: a
    /// claim event's is the database's, taken when its change was made.
    fn record_all(&self, events: &[Event]) -> Result<()> {
        self.append(&serde_json::to_value(events)?)
    }

    fn record_now(&self, what: &What) -> Result<()> {
        self.append(&serde_json::to_value([what])?)
    }

    fn seen(&self, session: &str, agent: &str) -> Result<()> {
        self.mark(session, agent, None)
    }

    fn seen_at(&self, session: &str, agent: &str, when: SystemTime) -> Result<()> {
        self.mark(session, agent, Some(when.into()))
    }

    fn forget(&self, session: &str, agent: Option<&str>) -> Result<()> {
        self.call("SELECT devkit.activity_forget($1, $2, $3)", &[
            (&session, Type::TEXT),
            (&agent, Type::TEXT),
        ])
    }

    /// An event whose payload does not parse is skipped.
    fn read(&self, now: DateTime<Utc>) -> Result<Activity> {
        self.read_as_of(Some(now))
    }

    fn read_now(&self) -> Result<Activity> {
        self.read_as_of(None)
    }
}
