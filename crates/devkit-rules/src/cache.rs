//! The local SQLite copy of a remote source's rules, which hooks read
//! instead of the network.
//!
//! Every reader loads the whole [`RuleIndex`] and filters it in memory, so
//! the file keeps each file and rule as JSON in index order, beside a `meta`
//! row naming the source kind, repository, remote and revision it holds. A
//! refresh replaces everything in one transaction, so a reader sees the
//! whole old index or the whole new one. A file whose format, kind,
//! repository or remote is not this cache's reads as no cache, and the next
//! refresh replaces it.

use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior, params};

use crate::model::{Rule, RuleFile, RuleIndex};

/// The layout this version of devkit reads and writes.
const FORMAT: i64 = 1;

/// How long a connection waits on another's lock: a reader on a refresh,
/// or one refresh on another.
const BUSY: Duration = Duration::from_secs(2);

const LAYOUT: &str = "
DROP TABLE IF EXISTS meta;
DROP TABLE IF EXISTS files;
DROP TABLE IF EXISTS rules;
CREATE TABLE meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    format INTEGER NOT NULL,
    kind TEXT NOT NULL,
    repository TEXT NOT NULL,
    source TEXT NOT NULL,
    repo TEXT NOT NULL,
    revision INTEGER NOT NULL,
    pulled_at INTEGER NOT NULL
);
CREATE TABLE files (position INTEGER PRIMARY KEY, json TEXT NOT NULL);
CREATE TABLE rules (position INTEGER PRIMARY KEY, json TEXT NOT NULL);
";

/// Whose rules a cache holds: the source kind, the repository's UUID, and
/// which remote they came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheKey {
    pub kind: &'static str,
    pub repository: String,
    /// The remote's identity with no credential in it, such as a Postgres
    /// target or a Supabase project URL.
    pub source: String,
}

/// What a cache records of its last refresh.
#[derive(Clone, Debug)]
pub struct Meta {
    /// The remote's revision the rules were read at.
    pub revision: i64,
    /// When they were read, in Unix seconds.
    pub pulled_at: i64,
}

/// One cache file, holding one repository's rules from one source kind.
#[derive(Clone, Debug)]
pub struct RuleCache {
    path: PathBuf,
    key: CacheKey,
}

impl RuleCache {
    pub fn new(path: PathBuf, key: CacheKey) -> RuleCache {
        RuleCache { path, key }
    }

    /// The cache for `key` under devkit's state directory, shared by every
    /// worktree and session on the machine.
    pub fn at_state_dir(state_dir: &Path, key: CacheKey) -> RuleCache {
        let name = format!("{}-{}.sqlite", key.kind, key.repository);
        RuleCache::new(state_dir.join("rules-cache").join(name), key)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The last refresh's record, `None` when there is no cache for this
    /// key.
    pub fn meta(&self) -> Option<Meta> {
        let conn = self.open_read().ok()??;
        self.read_meta(&conn).ok()?.map(|(meta, _)| meta)
    }

    /// The cached rules, `None` when there is no cache for this key. A row
    /// that does not parse is an error naming the file.
    pub fn read(&self) -> Result<Option<RuleIndex>> {
        let Some(mut conn) = self.open_read()? else {
            return Ok(None);
        };
        let tx = conn.transaction().context(self.reading())?;
        let Some((_, repo)) = self.read_meta(&tx)? else {
            return Ok(None);
        };
        let files = rows::<RuleFile>(&tx, "files").with_context(|| self.reading())?;
        let rules = rows::<Rule>(&tx, "rules").with_context(|| self.reading())?;
        Ok(Some(RuleIndex { repo, files, rules }))
    }

    /// Replaces the cache with `index`, read at `revision`, unless the cache
    /// already holds a later revision.
    pub fn write(&self, revision: i64, index: &RuleIndex) -> Result<()> {
        let writing = || format!("writing the rules cache {}", self.path.display());
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).with_context(writing)?;
        }
        let mut conn = Connection::open(&self.path).with_context(writing)?;
        conn.busy_timeout(BUSY).with_context(writing)?;
        conn.pragma_update(None, "journal_mode", "DELETE")
            .with_context(writing)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .with_context(writing)?;
        // Two sessions can refresh at once; the slower may hold an older
        // pull, which must not replace the newer one already written.
        if self
            .read_meta(&tx)?
            .is_some_and(|(meta, _)| meta.revision > revision)
        {
            return Ok(());
        }
        tx.execute_batch(LAYOUT).with_context(writing)?;
        tx.execute(
            "INSERT INTO meta
                 (singleton, format, kind, repository, source, repo, revision, pulled_at)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                FORMAT,
                self.key.kind,
                self.key.repository,
                self.key.source,
                index.repo,
                revision,
                now()
            ],
        )
        .with_context(writing)?;
        for (table, json) in [
            ("files", to_json(&index.files)?),
            ("rules", to_json(&index.rules)?),
        ] {
            let mut insert = tx
                .prepare(&format!(
                    "INSERT INTO {table} (position, json) VALUES (?1, ?2)"
                ))
                .with_context(writing)?;
            for (position, json) in json.iter().enumerate() {
                insert
                    .execute(params![position as i64, json])
                    .with_context(writing)?;
            }
        }
        tx.commit().with_context(writing)
    }

    /// A connection to read the file, `None` when there is no file.
    fn open_read(&self) -> Result<Option<Connection>> {
        if !self.path.is_file() {
            return Ok(None);
        }
        let conn = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| self.reading())?;
        conn.busy_timeout(BUSY).with_context(|| self.reading())?;
        Ok(Some(conn))
    }

    /// The record and the checkout path the cached index describes, `None`
    /// when the file is not this key's cache in this format.
    fn read_meta(&self, conn: &Connection) -> Result<Option<(Meta, String)>> {
        let row = conn
            .query_row(
                "SELECT format, kind, repository, source, repo, revision, pulled_at
                 FROM meta WHERE singleton = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional();
        let row = match row {
            Ok(row) => row,
            // A file of another layout, or no layout at all, is not a cache.
            Err(rusqlite::Error::SqliteFailure(e, _))
                if matches!(e.code, ErrorCode::Unknown | ErrorCode::NotADatabase) =>
            {
                return Ok(None);
            }
            Err(rusqlite::Error::InvalidColumnType(..)) => return Ok(None),
            Err(e) => return Err(e).with_context(|| self.reading()),
        };
        Ok(match row {
            Some((FORMAT, kind, repository, source, repo, revision, pulled_at))
                if kind == self.key.kind
                    && repository == self.key.repository
                    && source == self.key.source =>
            {
                Some((
                    Meta {
                        revision,
                        pulled_at,
                    },
                    repo,
                ))
            }
            _ => None,
        })
    }

    fn reading(&self) -> String {
        format!("reading the rules cache {}", self.path.display())
    }
}

fn rows<T: serde::de::DeserializeOwned>(conn: &Connection, table: &str) -> Result<Vec<T>> {
    let mut select = conn.prepare(&format!("SELECT json FROM {table} ORDER BY position"))?;
    let texts = select
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    texts
        .iter()
        .map(|text| serde_json::from_str(text).with_context(|| format!("a row of {table}")))
        .collect()
}

fn to_json<T: serde::Serialize>(items: &[T]) -> Result<Vec<String>> {
    items
        .iter()
        .map(|item| serde_json::to_string(item).context("serializing a cached rule"))
        .collect()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
