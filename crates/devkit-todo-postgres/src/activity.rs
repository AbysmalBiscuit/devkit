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

    /// One statement for the whole batch, each event keeping its own time:
    /// a claim event's is the database's, taken when its change was made.
    fn record_all(&self, events: &[Event]) -> Result<()> {
        let at: Vec<DateTime<Utc>> = events.iter().map(|e| e.at).collect();
        let what = events
            .iter()
            .map(|e| serde_json::to_string(&e.what))
            .collect::<Result<Vec<String>, _>>()?;
        self.db.run(async |client| {
            client
                .execute_typed(
                    "INSERT INTO devkit.activity (root, at, event)
                     SELECT $1, e.at, e.event::jsonb
                     FROM unnest($2::timestamptz[], $3::text[]) WITH ORDINALITY
                         AS e(at, event, n)
                     ORDER BY e.n",
                    &[
                        (&self.root, Type::TEXT),
                        (&at, Type::TIMESTAMPTZ_ARRAY),
                        (&what, Type::TEXT_ARRAY),
                    ],
                )
                .await?;
            Ok(())
        })
    }

    fn record_now(&self, what: &What) -> Result<()> {
        let what = serde_json::to_string(what)?;
        self.db.run(async |client| {
            client
                .execute_typed(
                    "INSERT INTO devkit.activity (root, at, event)
                     VALUES ($1, clock_timestamp(), $2::jsonb)",
                    &[(&self.root, Type::TEXT), (&what, Type::TEXT)],
                )
                .await?;
            Ok(())
        })
    }

    fn seen(&self, session: &str, agent: &str) -> Result<()> {
        self.db.run(async |client| {
            client
                .execute_typed(
                    "INSERT INTO devkit.seen (root, session, agent, at)
                     VALUES ($1, $2, $3, clock_timestamp())
                     ON CONFLICT (root, session, agent) DO UPDATE SET at = excluded.at",
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

    fn read_now(&self) -> Result<Activity> {
        self.read_as_of(None)
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
        self.read_as_of(Some(now))
    }
}

impl PostgresActivity {
    /// Every run and interval as of `now`, or as of the database's clock when
    /// `None`, read from one snapshot. An event whose payload does not parse
    /// is skipped.
    fn read_as_of(&self, now: Option<DateTime<Utc>>) -> Result<Activity> {
        let (events, seen, now) = self.db.run(async |client| {
            // One snapshot for both queries, so a stop that lands between
            // them cannot leave a run open with its last-seen mark gone.
            let tx = client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await?;
            // The snapshot is taken at the transaction's first statement, so
            // reading the clock first makes `now` the moment the records
            // are read as of.
            let now = match now {
                Some(now) => now,
                None => tx
                    .query_typed_one("SELECT clock_timestamp()", &[])
                    .await?
                    .get(0),
            };
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
            Ok((events, seen, now))
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
