//! The SQLite store `repo-rules-agent` writes (schema version 1), read and
//! edited under the extractor's editing contract so a rebuild keeps every edit.
//!
//! An edit never writes the active generation or a rule's extraction
//! fingerprint: the rebuild matches a pin to its extracted original by them.

use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{Map, Value};

use crate::{
    edit::{Fields, rule_id},
    index::resolved,
    model::{Rule, RuleFile, RuleIndex},
    source::RuleSource,
};

/// The storage version this reader and writer understand.
const SCHEMA_VERSION: i64 = 1;
/// How long a write waits for another writer before failing.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// The extractor's synthetic source for rules a person added.
const MANUAL_SOURCE: &str = "<manual>";
/// Rule fields stored in their own columns or tables rather than in `extra`.
const COLUMNS: [&str; 12] = [
    "id",
    "title",
    "description",
    "category",
    "scope",
    "severity",
    "directory",
    "tasks",
    "languages",
    "source_file",
    "pinned",
    "removed",
];

/// A rule list kept in its own table, one row per value in stored order.
struct Membership {
    table: &'static str,
    column: &'static str,
    field: &'static str,
    list: fn(&mut Rule) -> &mut Vec<String>,
}

const MEMBERSHIPS: [Membership; 2] = [
    Membership {
        table: "rule_tasks",
        column: "task",
        field: "tasks",
        list: |rule| &mut rule.tasks,
    },
    Membership {
        table: "rule_languages",
        column: "language",
        field: "languages",
        list: |rule| &mut rule.languages,
    },
];

/// Whether `path` holds a SQLite database, by its header. A missing file
/// counts when its extension is one SQLite files carry, so a configured store
/// that has gone missing is reported as one.
pub fn is_store(path: &Path) -> bool {
    match std::fs::File::open(path) {
        Ok(mut file) => {
            let mut header = [0u8; 16];
            file.read_exact(&mut header).is_ok() && &header == b"SQLite format 3\0"
        }
        Err(_) => path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| matches!(ext, "sqlite" | "sqlite3" | "db")),
    }
}

/// Whether the database header's file format versions (offsets 18 and 19)
/// say WAL.
fn is_wal(path: &Path) -> Result<bool> {
    let mut header = [0u8; 20];
    std::fs::File::open(path)?.read_exact(&mut header)?;
    Ok(header[18] == 2 || header[19] == 2)
}

/// The rule source over one SQLite store.
pub struct SqliteSource {
    path: PathBuf,
    /// The main worktree, which picks the repository in a store holding more
    /// than one.
    repo: PathBuf,
}

/// The repository a store describes.
struct Repository {
    id: String,
    path: String,
}

impl SqliteSource {
    pub fn at(path: PathBuf, repo: &Path) -> SqliteSource {
        SqliteSource {
            path,
            repo: repo.to_path_buf(),
        }
    }

    /// A connection that never creates the file. A WAL store is refused
    /// before opening, since opening one creates its `-shm` and `-wal` files.
    fn open(&self, flags: OpenFlags) -> Result<Connection> {
        if !self.path.is_file() {
            bail!("no such file");
        }
        if is_wal(&self.path)? {
            bail!("the store is in WAL journal mode, which devkit does not read or change");
        }
        let db = Connection::open_with_flags(&self.path, flags | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        db.busy_timeout(BUSY_TIMEOUT)?;
        db.pragma_update(None, "foreign_keys", true)?;
        Ok(db)
    }

    /// Every live rule of the store's repository, in stored order, inside one
    /// read transaction so the rules and their lists come from one snapshot.
    fn snapshot(&self) -> Result<RuleIndex> {
        let mut db = self.open(OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Deferred)?;
        check_version(&tx)?;
        let repo = repository(&tx, &self.repo)?;
        Ok(RuleIndex {
            files: files(&tx, &repo)?,
            rules: live_rules(&tx, &repo)?,
            repo: repo.path,
        })
    }

    /// Run `f` in one write transaction taken before anything is read, and
    /// bump the repository revision with it. Any failure rolls back the whole.
    fn write<T>(&self, f: impl FnOnce(&Transaction, &Repository) -> Result<T>) -> Result<T> {
        let run = || -> Result<T> {
            let mut db = self.open(OpenFlags::SQLITE_OPEN_READ_WRITE)?;
            let mode: String = db.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
            if !matches!(mode.as_str(), "delete" | "truncate" | "persist") {
                bail!(
                    "journal mode is {mode}; devkit edits a store only under rollback journaling \
                     (delete, truncate or persist) and does not change it"
                );
            }
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            check_version(&tx)?;
            let repo = repository(&tx, &self.repo)?;
            let out = f(&tx, &repo)?;
            tx.execute(
                "UPDATE repositories SET revision = revision + 1 WHERE repo_id = ?1",
                [&repo.id],
            )?;
            tx.commit()?;
            Ok(out)
        };
        run().with_context(|| format!("rules store at {}", self.path.display()))
    }
}

impl RuleSource for SqliteSource {
    fn read(&self) -> Result<Option<RuleIndex>> {
        self.snapshot()
            .map(Some)
            .with_context(|| format!("rules store at {} could not be read", self.path.display()))
    }

    fn add(&self, _repo: &str, fields: Fields) -> Result<String> {
        self.write(|tx, repo| {
            let id = rule_id(MANUAL_SOURCE, fields.title.as_deref().unwrap_or_default());
            if !matches(tx, repo, &id)?.is_empty() {
                bail!("rule {id} already exists; change it with `devkit rules edit {id}`");
            }
            let defaults: Rule = serde_json::from_value(Value::Object(Map::new()))?;
            let Value::Object(mut rule) = serde_json::to_value(defaults)? else {
                unreachable!("a struct serializes to an object");
            };
            fields.apply(&mut rule);
            let source = manual_source(tx, repo)?;
            let key = uuid::Uuid::new_v4().simple().to_string();
            tx.execute(
                "INSERT INTO rules (repo_id, rule_key, external_id, source_key, title, description,
                     category, scope, severity, directory, position, pinned, removed,
                     extraction_fingerprint, extra)
                 SELECT ?1, ?2, ?3, ?4, '', '', '', 'repo', 'should', NULL,
                     COALESCE(MAX(position) + 1, 0), 1, 0, NULL, '{}'
                 FROM rules WHERE repo_id = ?1",
                [&repo.id, &key, &id, &source],
            )?;
            store_rule(tx, repo, &key, &rule)?;
            Ok(id)
        })
    }

    fn edit(&self, id: &str, fields: Fields) -> Result<()> {
        if fields.is_empty() {
            bail!("nothing to change: pass at least one field to set");
        }
        self.write(|tx, repo| {
            let key = one_live(tx, repo, id)?.key;
            let mut rule = stored_fields(tx, repo, &key)?;
            fields.apply(&mut rule);
            store_rule(tx, repo, &key, &rule)
        })
    }

    /// An extracted rule stays as a pinned tombstone, so a rebuild suppresses
    /// its extracted original. A rule a person added has no original to
    /// suppress and is deleted with its lists.
    fn remove(&self, id: &str) -> Result<()> {
        self.write(|tx, repo| {
            let rule = one_live(tx, repo, id)?;
            let added = rule.fingerprint.is_none()
                && (rule.source == MANUAL_SOURCE || rule.source.is_empty());
            let params = [&repo.id, &rule.key];
            if added {
                tx.execute(
                    "DELETE FROM rules WHERE repo_id = ?1 AND rule_key = ?2",
                    params,
                )?;
            } else {
                tx.execute(
                    "UPDATE rules SET pinned = 1, removed = 1 WHERE repo_id = ?1 AND rule_key = ?2",
                    params,
                )?;
            }
            Ok(())
        })
    }

    fn location(&self) -> String {
        self.path.display().to_string()
    }

    fn kind(&self) -> &'static str {
        "sqlite"
    }
}

fn check_version(tx: &Transaction) -> Result<()> {
    let version: Option<i64> = tx
        .query_row(
            "SELECT version FROM storage_version WHERE singleton = 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    match version {
        Some(SCHEMA_VERSION) => Ok(()),
        Some(other) => bail!("unsupported storage version {other}; devkit reads {SCHEMA_VERSION}"),
        None => bail!("no storage version"),
    }
}

/// The repository `local:` plus the main worktree's path names, else the only
/// repository the store holds.
fn repository(tx: &Transaction, main: &Path) -> Result<Repository> {
    let locator = format!("local:{}", resolved(main).display());
    let mut stmt = tx.prepare("SELECT repo_id, locator, local_path FROM repositories")?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let count = rows.len();
    let chosen = match rows.iter().position(|row| row.1 == locator) {
        Some(at) => rows.into_iter().nth(at),
        None if count == 1 => rows.into_iter().next(),
        None => None,
    };
    match chosen {
        Some((id, locator, local_path)) => Ok(Repository {
            id,
            path: local_path.unwrap_or(locator),
        }),
        None => bail!("holds {count} repositories and none is {locator}"),
    }
}

/// The discovered sources, in the order extraction found them.
fn files(tx: &Transaction, repo: &Repository) -> Result<Vec<RuleFile>> {
    let mut stmt = tx.prepare(
        "SELECT path, tier, extra FROM sources WHERE repo_id = ?1 AND discovered ORDER BY path",
    )?;
    let mut files = stmt
        .query_map([&repo.id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .map(|row| {
            let (path, tier, extra) = row?;
            let extra = object(&extra).with_context(|| format!("source {path}"))?;
            let position = extra
                .get("file_position")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            let errors = match extra.get("errors") {
                None => Vec::new(),
                Some(errors) => serde_json::from_value(errors.clone())
                    .with_context(|| format!("source {path}: errors"))?,
            };
            let file = RuleFile {
                path,
                tier,
                applies_to: String::new(),
                errors,
                content: None,
            };
            Ok((position, file))
        })
        .collect::<Result<Vec<_>>>()?;
    files.sort_by_key(|(position, _)| *position);
    Ok(files.into_iter().map(|(_, file)| file).collect())
}

/// The rules that are not tombstones, ordered by position, each with its
/// lists in stored order.
fn live_rules(tx: &Transaction, repo: &Repository) -> Result<Vec<Rule>> {
    let memberships = MEMBERSHIPS
        .iter()
        .map(|m| Ok((m, lists(tx, repo, m.table, m.column)?)))
        .collect::<Result<Vec<_>>>()?;
    let mut stmt = tx.prepare(
        "SELECT r.rule_key, r.external_id, r.title, r.description, r.category, r.scope,
             r.severity, r.directory, r.pinned, r.extra, s.path
         FROM rules r JOIN sources s ON s.repo_id = r.repo_id AND s.source_key = r.source_key
         WHERE r.repo_id = ?1 AND NOT r.removed
         ORDER BY r.position, r.rule_key",
    )?;
    stmt.query_map([&repo.id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Rule {
                id: r.get(1)?,
                title: r.get(2)?,
                description: r.get(3)?,
                category: r.get(4)?,
                scope_raw: r.get(5)?,
                severity_raw: r.get(6)?,
                directory: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
                pinned: r.get(8)?,
                source_file: r.get(10)?,
                tasks: Vec::new(),
                languages: Vec::new(),
                topics: Vec::new(),
                removed: false,
            },
            r.get::<_, String>(9)?,
        ))
    })?
    .map(|row| {
        let (key, mut rule, extra) = row?;
        let extra = object(&extra).with_context(|| format!("rule {}", rule.id))?;
        if let Some(topics) = extra.get("topics") {
            rule.topics = serde_json::from_value(topics.clone())
                .with_context(|| format!("rule {}: topics", rule.id))?;
        }
        for (membership, lists) in &memberships {
            *(membership.list)(&mut rule) = lists.get(&key).cloned().unwrap_or_default();
        }
        Ok(rule)
    })
    .collect()
}

/// One membership table's values per rule key, each list in stored order.
fn lists(
    tx: &Transaction,
    repo: &Repository,
    table: &str,
    column: &str,
) -> Result<HashMap<String, Vec<String>>> {
    let mut stmt = tx.prepare(&format!(
        "SELECT rule_key, {column} FROM {table} WHERE repo_id = ?1 ORDER BY rule_key, position"
    ))?;
    let mut lists: HashMap<String, Vec<String>> = HashMap::new();
    for row in stmt.query_map([&repo.id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        let (key, value) = row?;
        lists.entry(key).or_default().push(value);
    }
    Ok(lists)
}

fn object(raw: &str) -> Result<Map<String, Value>> {
    match serde_json::from_str(raw).context("extra is not JSON")? {
        Value::Object(map) => Ok(map),
        _ => bail!("extra is not a JSON object"),
    }
}

/// A live rule an id names.
struct Match {
    key: String,
    title: String,
    source: String,
    fingerprint: Option<String>,
}

/// Every live rule whose id is `id`. SQL stores allow an id to repeat.
fn matches(tx: &Transaction, repo: &Repository, id: &str) -> Result<Vec<Match>> {
    let mut stmt = tx.prepare(
        "SELECT r.rule_key, r.title, s.path, r.extraction_fingerprint
         FROM rules r JOIN sources s ON s.repo_id = r.repo_id AND s.source_key = r.source_key
         WHERE r.repo_id = ?1 AND r.external_id = ?2 AND NOT r.removed
         ORDER BY r.position, r.rule_key",
    )?;
    let found = stmt
        .query_map([&repo.id, id], |r| {
            Ok(Match {
                key: r.get(0)?,
                title: r.get(1)?,
                source: r.get(2)?,
                fingerprint: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(found)
}

/// The one live rule `id` names. Several are refused by name, since editing
/// any one of them would be a guess.
fn one_live(tx: &Transaction, repo: &Repository, id: &str) -> Result<Match> {
    let mut found = matches(tx, repo, id)?;
    match found.len() {
        0 => bail!("no rule {id} in the index"),
        1 => Ok(found.remove(0)),
        n => {
            let listed: Vec<String> = found
                .iter()
                .map(|m| format!("  {}: {}", m.source, m.title))
                .collect();
            bail!(
                "rule id {id} names {n} rules, and devkit cannot tell which one you mean:\n{}",
                listed.join("\n")
            )
        }
    }
}

/// The rule `key` as the JSON index spells a rule: its columns, its lists and
/// its preserved metadata in one object, so `Fields::apply` edits both kinds
/// of store the same way.
fn stored_fields(tx: &Transaction, repo: &Repository, key: &str) -> Result<Map<String, Value>> {
    let (scalars, extra) = tx.query_row(
        "SELECT title, description, category, scope, severity, directory, extra
         FROM rules WHERE repo_id = ?1 AND rule_key = ?2",
        [&repo.id, key],
        |r| {
            let mut scalars = Map::new();
            for (at, name) in ["title", "description", "category", "scope", "severity"]
                .into_iter()
                .enumerate()
            {
                scalars.insert(name.to_string(), r.get::<_, String>(at)?.into());
            }
            let directory: Option<String> = r.get(5)?;
            scalars.insert("directory".to_string(), directory.into());
            Ok((scalars, r.get::<_, String>(6)?))
        },
    )?;
    let mut rule = object(&extra)?;
    rule.extend(scalars);
    for Membership {
        table,
        column,
        field,
        ..
    } in &MEMBERSHIPS
    {
        let mut stmt = tx.prepare(&format!(
            "SELECT {column} FROM {table} WHERE repo_id = ?1 AND rule_key = ?2 ORDER BY position"
        ))?;
        let values = stmt
            .query_map([&repo.id, key], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rule.insert(field.to_string(), values.into());
    }
    Ok(rule)
}

/// Write `rule` over the row `key` and pin it: its columns, its preserved
/// metadata, and its lists replaced whole.
fn store_rule(
    tx: &Transaction,
    repo: &Repository,
    key: &str,
    rule: &Map<String, Value>,
) -> Result<()> {
    let text = |name: &str| rule.get(name).and_then(Value::as_str).unwrap_or_default();
    let extra: Map<String, Value> = rule
        .iter()
        .filter(|(name, _)| !COLUMNS.contains(&name.as_str()))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    tx.execute(
        "UPDATE rules SET title = ?3, description = ?4, category = ?5, scope = ?6,
             severity = ?7, directory = ?8, extra = ?9, pinned = 1
         WHERE repo_id = ?1 AND rule_key = ?2",
        rusqlite::params![
            repo.id,
            key,
            text("title"),
            text("description"),
            text("category"),
            text("scope"),
            text("severity"),
            rule.get("directory").and_then(Value::as_str),
            Value::Object(extra).to_string(),
        ],
    )?;
    for Membership {
        table,
        column,
        field,
        ..
    } in &MEMBERSHIPS
    {
        tx.execute(
            &format!("DELETE FROM {table} WHERE repo_id = ?1 AND rule_key = ?2"),
            [&repo.id, key],
        )?;
        let values = rule.get(*field).and_then(Value::as_array);
        for (position, value) in values.into_iter().flatten().enumerate() {
            tx.execute(
                &format!(
                    "INSERT INTO {table} (repo_id, rule_key, position, {column})
                     VALUES (?1, ?2, ?3, ?4)"
                ),
                rusqlite::params![repo.id, key, position as i64, value.as_str()],
            )?;
        }
    }
    Ok(())
}

/// The `<manual>` source's key, registering the source on first use the way
/// the extractor does: tier zero and not discovered.
fn manual_source(tx: &Transaction, repo: &Repository) -> Result<String> {
    let existing: Option<String> = tx
        .query_row(
            "SELECT source_key FROM sources WHERE repo_id = ?1 AND path = ?2",
            [&repo.id, MANUAL_SOURCE],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(key) = existing {
        return Ok(key);
    }
    let key = uuid::Uuid::new_v4().simple().to_string();
    tx.execute(
        "INSERT INTO sources (repo_id, source_key, path, tier, discovered, extra)
         VALUES (?1, ?2, ?3, 0, 0, '{}')",
        [&repo.id, &key, MANUAL_SOURCE],
    )?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture store, built by `repo-rules-agent import-json` and dumped
    /// to SQL, for the repository `/repo`.
    fn store(dir: &Path) -> PathBuf {
        let path = dir.join("index.sqlite");
        let db = Connection::open(&path).unwrap();
        // The dump creates tables alphabetically, so a membership row arrives
        // before the `rules` table its foreign key names.
        db.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        db.execute_batch(include_str!("../tests/fixtures/sqlite/store.sql"))
            .unwrap();
        path
    }

    #[test]
    fn a_load_keeps_stored_rule_and_membership_order_and_skips_tombstones() {
        let dir = tempfile::tempdir().unwrap();
        let index = SqliteSource::at(store(dir.path()), Path::new("/repo"))
            .load()
            .unwrap();
        let ids: Vec<&str> = index.rules.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, [
            "r-root-must",
            "r-foo-should",
            "r-foo-can",
            "r-untagged",
            "r-review-only",
            "r-pinned-manual",
        ]);
        let foo = &index.rules[1];
        assert_eq!(foo.tasks, ["code-review", "code-generation"]);
        assert_eq!(foo.languages, ["rust", "toml"]);
        assert_eq!(foo.topics, ["errors"]);
        assert!(index.rules[3].tasks.is_empty());
        let files: Vec<(&str, i64)> = index
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.tier))
            .collect();
        assert_eq!(files, [("AGENTS.md", 1), ("crates/foo/AGENTS.md", 2)]);
        assert_eq!(index.files[1].errors, ["chunk 3: timed out"]);
        assert_eq!(index.repo, "/repo");
    }

    #[test]
    fn a_store_of_several_repositories_reads_the_one_named_for_the_main_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let path = store(dir.path());
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "INSERT INTO repositories VALUES
                 ('00000000000000000000000000000001', 'local:/other', '/other', '', NULL, 0, 0);",
            )
            .unwrap();
        let found = SqliteSource::at(path.clone(), Path::new("/repo")).snapshot();
        assert_eq!(found.unwrap().rules.len(), 6);
        let err = SqliteSource::at(path, Path::new("/elsewhere"))
            .snapshot()
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("2 repositories and none is local:/elsewhere"),
            "{err}"
        );
    }
}
