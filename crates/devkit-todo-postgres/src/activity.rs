use std::{collections::HashMap, sync::Arc, time::SystemTime};

use anyhow::Result;
use chrono::{DateTime, Utc};
use devkit_todo::activity::{Activity, ActivityStore, Event, What};
use tokio_postgres::{IsolationLevel, types::Type};

use crate::Database;

/// Subagent runs and claim intervals kept in the database beside the todos,
/// under the same root, so every machine reports into one log.
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
}

impl ActivityStore for PostgresActivity {
    fn record(&self, event: &Event) -> Result<()> {
        let what = serde_json::to_string(&event.what)?;
        self.db.run(async |client| {
            client
                .execute_typed(
                    "INSERT INTO devkit.activity (root, at, event) VALUES ($1, $2, $3::jsonb)",
                    &[
                        (&self.root, Type::TEXT),
                        (&event.at, Type::TIMESTAMPTZ),
                        (&what, Type::TEXT),
                    ],
                )
                .await?;
            Ok(())
        })
    }

    fn seen_at(&self, session: &str, agent: &str, when: SystemTime) -> Result<()> {
        let when = DateTime::<Utc>::from(when);
        self.db.run(async |client| {
            client
                .execute_typed(
                    "INSERT INTO devkit.seen (root, session, agent, at) VALUES ($1, $2, $3, $4)
                     ON CONFLICT (root, session, agent) DO UPDATE SET at = excluded.at",
                    &[
                        (&self.root, Type::TEXT),
                        (&session, Type::TEXT),
                        (&agent, Type::TEXT),
                        (&when, Type::TIMESTAMPTZ),
                    ],
                )
                .await?;
            Ok(())
        })
    }

    fn forget(&self, session: &str, agent: Option<&str>) -> Result<()> {
        self.db.run(async |client| {
            client
                .execute_typed(
                    "DELETE FROM devkit.seen
                     WHERE root = $1 AND session = $2 AND ($3::text IS NULL OR agent = $3)",
                    &[
                        (&self.root, Type::TEXT),
                        (&session, Type::TEXT),
                        (&agent, Type::TEXT),
                    ],
                )
                .await?;
            Ok(())
        })
    }

    /// An event whose payload does not parse is skipped.
    fn read(&self, now: DateTime<Utc>) -> Result<Activity> {
        let (events, seen) = self.db.run(async |client| {
            // One snapshot for both queries, so a stop that lands between
            // them cannot leave a run open with its last-seen mark gone.
            let tx = client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await?;
            let events = tx
                .query_typed(
                    "SELECT at, event::text FROM devkit.activity WHERE root = $1 ORDER BY at, id",
                    &[(&self.root, Type::TEXT)],
                )
                .await?;
            let seen = tx
                .query_typed(
                    "SELECT session, agent, at FROM devkit.seen WHERE root = $1",
                    &[(&self.root, Type::TEXT)],
                )
                .await?;
            tx.commit().await?;
            Ok((events, seen))
        })?;
        let events = events
            .iter()
            .filter_map(|row| {
                let what = serde_json::from_str::<What>(row.get(1)).ok()?;
                Some(Event {
                    at: row.get(0),
                    what,
                })
            })
            .collect();
        let seen: HashMap<(String, String), DateTime<Utc>> = seen
            .iter()
            .map(|row| ((row.get(0), row.get(1)), row.get(2)))
            .collect();
        Ok(Activity::of(
            events,
            |session, agent| seen.get(&(session.to_string(), agent.to_string())).copied(),
            now,
        ))
    }
}
