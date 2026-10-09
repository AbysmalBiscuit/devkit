//! `repo-rules-agent`'s Postgres store as a rule source: one repository's
//! rules in the `repo_rules` schema of a database every machine shares.
//!
//! A read takes one REPEATABLE READ snapshot. An edit locks the repository row
//! in a statement of its own before reading anything, as every writer to the
//! store does, so concurrent edits serialize and each sees the last. No
//! statement is named, so a transaction-mode pooler may sit in between.

use std::{path::Path, sync::Arc};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value};
use tokio_postgres::{GenericClient, IsolationLevel, Row, error::SqlState, types::Type};

use crate::{
    edit::{Fields, apply, rule_id},
    model::{Rule, RuleFile, RuleIndex},
    source::RuleSource,
};

/// The storage version this source reads and writes.
const STORAGE_VERSION: i32 = 1;

/// The source file a rule added by hand belongs to, as the store's own
/// `put_rule` names it.
pub(crate) const MANUAL_SOURCE: &str = "<manual>";

pub use devkit_postgres::{Database, is_unreachable};

/// Whether `e` is the server reporting that a table or schema does not exist.
fn missing_relation(e: &tokio_postgres::Error) -> bool {
    e.code().is_some_and(|code| {
        *code == SqlState::UNDEFINED_TABLE || *code == SqlState::INVALID_SCHEMA_NAME
    })
}

/// The rule source over one repository in the store.
pub struct PostgresSource {
    db: Arc<Database>,
    /// The repository's UUID, lowercased, or why there is none.
    repository: Result<String, String>,
    /// The local checkout the rules describe, reported as the index's `repo`
    /// so a query reads that checkout's `.agents/repo-rules-agent.toml`.
    repo: String,
}

impl PostgresSource {
    /// The repository `repository` names in `db`, describing the checkout at
    /// `repo`.
    pub fn new(db: Arc<Database>, repository: Option<&str>, repo: &Path) -> PostgresSource {
        let repository = match repository.map(str::trim) {
            None | Some("") => Err("[rules.postgres] repository is not set".to_string()),
            Some(id) if is_uuid(id) => Ok(id.to_ascii_lowercase()),
            Some(id) => Err(format!("[rules.postgres] repository {id:?} is not a UUID")),
        };
        PostgresSource {
            db,
            repository,
            repo: repo.display().to_string(),
        }
    }

    /// Connects, checks the store's version and finds the repository, without
    /// reading its rules, and returns how many live rules it holds.
    pub fn check(&self) -> Result<i64> {
        let repo = self.repository()?;
        self.db.run(async |client| {
            let tx = client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await?;
            check_version(&tx).await?;
            find_repository(&tx, repo, false).await?;
            let row = tx
                .query_typed_one(
                    "SELECT count(*) FROM repo_rules.rules WHERE repo_id = $1::uuid AND NOT removed",
                    &[(&repo, Type::TEXT)],
                )
                .await?;
            tx.commit().await?;
            Ok(row.get(0))
        })
    }

    /// The repository's revision, which every edit to its rules increments.
    pub fn revision(&self) -> Result<i64> {
        let repo = self.repository()?;
        self.db.run(async |client| {
            check_version(&*client).await?;
            let rows = client
                .query_typed(
                    "SELECT revision FROM repo_rules.repositories WHERE repo_id = $1::uuid",
                    &[(&repo, Type::TEXT)],
                )
                .await?;
            match rows.first() {
                Some(row) => Ok(row.get(0)),
                None => bail!("no repository {repo} in the rules store"),
            }
        })
    }

    /// The live rules and the revision they are at, read from one snapshot.
    pub fn pull(&self) -> Result<(i64, RuleIndex)> {
        let repo = self.repository()?;
        let (revision, rules, files) = self.db.run(async |client| {
            let tx = client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await?;
            check_version(&tx).await?;
            find_repository(&tx, repo, false).await?;
            let revision: i64 = tx
                .query_typed_one(
                    "SELECT revision FROM repo_rules.repositories WHERE repo_id = $1::uuid",
                    &[(&repo, Type::TEXT)],
                )
                .await?
                .get(0);
            let rules = tx
                .query_typed(&format!("{RULES} AND NOT r.removed {ORDER}"), &[(
                    &repo,
                    Type::TEXT,
                )])
                .await?
                .iter()
                .map(rule_of)
                .collect::<Result<Vec<_>>>()?;
            let files = tx
                .query_typed(
                    "SELECT path, tier, extra::text FROM repo_rules.sources
                     WHERE repo_id = $1::uuid AND discovered
                     ORDER BY coalesce((extra->>'file_position')::bigint, 0), path",
                    &[(&repo, Type::TEXT)],
                )
                .await?
                .iter()
                .map(file_of)
                .collect::<Result<Vec<_>>>()?;
            tx.commit().await?;
            Ok((revision, rules, files))
        })?;
        let index = RuleIndex {
            repo: self.repo.clone(),
            files,
            rules,
        };
        Ok((revision, index))
    }

    /// The repository's UUID, lowercased, when the config names a valid one.
    pub fn repository_id(&self) -> Option<&str> {
        self.repository.as_deref().ok()
    }

    /// The local checkout the rules describe.
    pub fn checkout(&self) -> &str {
        &self.repo
    }

    fn repository(&self) -> Result<&str> {
        self.repository
            .as_deref()
            .map_err(|reason| anyhow!("{reason}"))
    }

    /// Runs `op` in an edit's transaction: the repository row locked first,
    /// in a statement of its own, and its revision incremented last.
    fn edit_with<T>(
        &self,
        op: impl AsyncFnOnce(&tokio_postgres::Transaction<'_>, &str) -> Result<T>,
    ) -> Result<T> {
        let repo = self.repository()?;
        self.db.run(async |client| {
            let tx = client
                .build_transaction()
                .isolation_level(IsolationLevel::ReadCommitted)
                .start()
                .await?;
            check_version(&tx).await?;
            find_repository(&tx, repo, true).await?;
            let out = op(&tx, repo).await?;
            tx.query_typed(
                "UPDATE repo_rules.repositories SET revision = revision + 1
                 WHERE repo_id = $1::uuid",
                &[(&repo, Type::TEXT)],
            )
            .await?;
            tx.commit().await?;
            Ok(out)
        })
    }
}

impl RuleSource for PostgresSource {
    fn read(&self) -> Result<Option<RuleIndex>> {
        self.pull().map(|(_, index)| Some(index))
    }

    fn add(&self, _repo: &str, fields: Fields) -> Result<String> {
        let title = fields.title.clone().unwrap_or_default();
        let id = rule_id(MANUAL_SOURCE, &title);
        let mut rule = Rule::default_extracted();
        rule.id = id.clone();
        rule.source_file = MANUAL_SOURCE.to_string();
        let rule = apply(rule, fields)?;
        self.edit_with(async |tx, repo| {
            if !live(tx, repo, &id).await?.is_empty() {
                bail!("rule {id} already exists; change it with `devkit rules edit {id}`");
            }
            let source = manual_source(tx, repo).await?;
            let key: String = tx
                .query_typed_one(
                    "INSERT INTO repo_rules.rules (repo_id, rule_key, external_id, source_key,
                         title, description, category, scope, severity, directory, position,
                         pinned, removed, extraction_fingerprint, extra)
                     SELECT $1::uuid, gen_random_uuid(), $2, $3::uuid, $4, $5, $6, $7, $8, $9,
                         coalesce(max(position) + 1, 0), true, false, NULL, $10::text::jsonb
                     FROM repo_rules.rules WHERE repo_id = $1::uuid
                     RETURNING rule_key::text",
                    &[
                        (&repo, Type::TEXT),
                        (&rule.id, Type::TEXT),
                        (&source, Type::TEXT),
                        (&rule.title, Type::TEXT),
                        (&rule.description, Type::TEXT),
                        (&rule.category, Type::TEXT),
                        (&rule.scope_raw, Type::TEXT),
                        (&rule.severity_raw, Type::TEXT),
                        (&rule.directory, Type::TEXT),
                        (&extra_of(&Map::new(), &rule), Type::TEXT),
                    ],
                )
                .await?
                .get(0);
            replace_lists(tx, repo, &key, &rule).await?;
            Ok(())
        })?;
        Ok(id)
    }

    fn edit(&self, id: &str, fields: Fields) -> Result<()> {
        if fields.is_empty() {
            bail!("nothing to change: pass at least one field to set");
        }
        self.edit_with(async |tx, repo| {
            let (key, rule, extra) = one_live(tx, repo, id).await?;
            let rule = apply(rule, fields)?;
            tx.query_typed(
                "UPDATE repo_rules.rules SET title = $3, description = $4, category = $5,
                     scope = $6, severity = $7, directory = $8, pinned = true,
                     extra = $9::text::jsonb
                 WHERE repo_id = $1::uuid AND rule_key = $2::uuid",
                &[
                    (&repo, Type::TEXT),
                    (&key, Type::TEXT),
                    (&rule.title, Type::TEXT),
                    (&rule.description, Type::TEXT),
                    (&rule.category, Type::TEXT),
                    (&rule.scope_raw, Type::TEXT),
                    (&rule.severity_raw, Type::TEXT),
                    (&rule.directory, Type::TEXT),
                    (&extra_of(&extra, &rule), Type::TEXT),
                ],
            )
            .await?;
            replace_lists(tx, repo, &key, &rule).await
        })
    }

    fn remove(&self, id: &str) -> Result<()> {
        self.edit_with(async |tx, repo| {
            let (key, ..) = one_live(tx, repo, id).await?;
            tx.query_typed(
                "UPDATE repo_rules.rules SET pinned = true, removed = true
                 WHERE repo_id = $1::uuid AND rule_key = $2::uuid",
                &[(&repo, Type::TEXT), (&key, Type::TEXT)],
            )
            .await?;
            Ok(())
        })
    }

    fn location(&self) -> String {
        match &self.repository {
            Ok(repo) => format!("{}, repository {repo}", self.db.target()),
            Err(_) => format!("{}, no repository", self.db.target()),
        }
    }

    fn kind(&self) -> &'static str {
        "postgres"
    }
}

/// A rule's columns, its source file's path and its lists in their stored
/// order, for one repository. Callers add their own conditions.
const RULES: &str = "SELECT r.rule_key::text, r.external_id, r.title, r.description, r.category,
        r.scope, r.severity, coalesce(r.directory, ''), s.path, r.pinned, r.extra::text,
        ARRAY(SELECT t.task FROM repo_rules.rule_tasks t
              WHERE t.repo_id = r.repo_id AND t.rule_key = r.rule_key ORDER BY t.position),
        ARRAY(SELECT l.language FROM repo_rules.rule_languages l
              WHERE l.repo_id = r.repo_id AND l.rule_key = r.rule_key ORDER BY l.position)
    FROM repo_rules.rules r
    JOIN repo_rules.sources s ON s.repo_id = r.repo_id AND s.source_key = r.source_key
    WHERE r.repo_id = $1::uuid";

/// The store's own order for visible rules.
const ORDER: &str = "ORDER BY r.position, r.rule_key";

fn rule_of(row: &Row) -> Result<Rule> {
    let extra = extra_map(row.get(10))?;
    let topics = extra
        .get("topics")
        .and_then(Value::as_array)
        .map(|topics| {
            topics
                .iter()
                .filter_map(|t| t.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(Rule {
        id: row.get(1),
        title: row.get(2),
        description: row.get(3),
        category: row.get(4),
        tasks: row.get(11),
        languages: row.get(12),
        topics,
        scope_raw: row.get(5),
        severity_raw: row.get(6),
        source_file: row.get(8),
        directory: row.get(7),
        pinned: row.get(9),
        removed: false,
    })
}

fn file_of(row: &Row) -> Result<RuleFile> {
    let extra = extra_map(row.get(2))?;
    let errors = extra
        .get("errors")
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter_map(|e| e.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(RuleFile {
        path: row.get(0),
        tier: i64::from(row.get::<_, i32>(1)),
        applies_to: String::new(),
        errors,
        content: None,
    })
}

fn extra_map(text: &str) -> Result<Map<String, Value>> {
    match serde_json::from_str(text).context("a stored rule's metadata does not parse")? {
        Value::Object(map) => Ok(map),
        _ => bail!("a stored rule's metadata is not a JSON object"),
    }
}

/// `extra` with the rule's topics, the one field the store keeps there that
/// devkit edits.
fn extra_of(extra: &Map<String, Value>, rule: &Rule) -> String {
    let mut extra = extra.clone();
    extra.insert("topics".to_string(), Value::from(rule.topics.clone()));
    Value::Object(extra).to_string()
}

/// Fails unless the store is the version this source speaks.
async fn check_version(db: &impl GenericClient) -> Result<()> {
    let rows = match db
        .query_typed(
            "SELECT version FROM repo_rules.storage_version WHERE singleton = 1",
            &[],
        )
        .await
    {
        Err(e) if missing_relation(&e) => {
            return Err(anyhow::Error::from(e)
                .context("no repo-rules-agent store here: repo_rules.storage_version is missing"));
        }
        out => out?,
    };
    match rows.first().map(|row| row.get::<_, i32>(0)) {
        Some(STORAGE_VERSION) => Ok(()),
        Some(version) => bail!(
            "the rules store is at storage version {version}, and devkit reads version {STORAGE_VERSION}"
        ),
        None => bail!("the rules store records no storage version"),
    }
}

/// Fails unless the repository exists, locking its row for the rest of the
/// transaction when `lock` is set.
async fn find_repository(db: &impl GenericClient, repo: &str, lock: bool) -> Result<()> {
    let lock = if lock { " FOR UPDATE" } else { "" };
    let rows = db
        .query_typed(
            &format!("SELECT 1 FROM repo_rules.repositories WHERE repo_id = $1::uuid{lock}"),
            &[(&repo, Type::TEXT)],
        )
        .await?;
    if rows.is_empty() {
        bail!("no repository {repo} in the rules store");
    }
    Ok(())
}

/// The live rules whose id is `id`: their keys, the rules, and their stored
/// metadata.
async fn live(
    db: &impl GenericClient,
    repo: &str,
    id: &str,
) -> Result<Vec<(String, Rule, Map<String, Value>)>> {
    db.query_typed(
        &format!("{RULES} AND r.external_id = $2 AND NOT r.removed {ORDER}"),
        &[(&repo, Type::TEXT), (&id, Type::TEXT)],
    )
    .await?
    .iter()
    .map(|row| Ok((row.get(0), rule_of(row)?, extra_map(row.get(10))?)))
    .collect()
}

/// The one live rule `id` names. Imported records may share an id, and an
/// edit that cannot tell them apart changes neither.
async fn one_live(
    db: &impl GenericClient,
    repo: &str,
    id: &str,
) -> Result<(String, Rule, Map<String, Value>)> {
    let mut found = live(db, repo, id).await?;
    match found.len() {
        0 => bail!("no rule {id} in repository {repo}"),
        1 => Ok(found.remove(0)),
        n => bail!(
            "{n} rules in repository {repo} share the id {id}; edit them through repo-rules-agent"
        ),
    }
}

/// The key of the source rules added by hand belong to, created on first use
/// as the store's `put_rule` creates it.
async fn manual_source(db: &impl GenericClient, repo: &str) -> Result<String> {
    let found = db
        .query_typed(
            "SELECT source_key::text FROM repo_rules.sources
             WHERE repo_id = $1::uuid AND path = $2",
            &[(&repo, Type::TEXT), (&MANUAL_SOURCE, Type::TEXT)],
        )
        .await?;
    if let Some(row) = found.first() {
        return Ok(row.get(0));
    }
    Ok(db
        .query_typed_one(
            "INSERT INTO repo_rules.sources (repo_id, source_key, path, tier, discovered, extra)
             VALUES ($1::uuid, gen_random_uuid(), $2, 0, false, '{}'::jsonb)
             RETURNING source_key::text",
            &[(&repo, Type::TEXT), (&MANUAL_SOURCE, Type::TEXT)],
        )
        .await?
        .get(0))
}

/// Replaces the rule's task and language lists with `rule`'s, in order.
async fn replace_lists(db: &impl GenericClient, repo: &str, key: &str, rule: &Rule) -> Result<()> {
    for (table, column, values) in [
        ("rule_tasks", "task", &rule.tasks),
        ("rule_languages", "language", &rule.languages),
    ] {
        db.query_typed(
            &format!(
                "DELETE FROM repo_rules.{table} WHERE repo_id = $1::uuid AND rule_key = $2::uuid"
            ),
            &[(&repo, Type::TEXT), (&key, Type::TEXT)],
        )
        .await?;
        db.query_typed(
            &format!(
                "INSERT INTO repo_rules.{table} (repo_id, rule_key, position, {column})
                 SELECT $1::uuid, $2::uuid, (n - 1)::int, v
                 FROM unnest($3::text[]) WITH ORDINALITY AS u(v, n)"
            ),
            &[
                (&repo, Type::TEXT),
                (&key, Type::TEXT),
                (values, Type::TEXT_ARRAY),
            ],
        )
        .await?;
    }
    Ok(())
}

/// Whether `id` spells a UUID in its hyphenated form.
pub(crate) fn is_uuid(id: &str) -> bool {
    const HYPHENS: [usize; 4] = [8, 13, 18, 23];
    id.len() == 36
        && id.char_indices().all(|(i, c)| match HYPHENS.contains(&i) {
            true => c == '-',
            false => c.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_must_be_a_uuid() {
        assert!(is_uuid("0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a10"));
        assert!(is_uuid("0B6F6C1E-8F0E-4A43-9D55-3C0D2B1F9A10"));
        assert!(!is_uuid("0b6f6c1e8f0e4a439d553c0d2b1f9a10"));
        assert!(!is_uuid("0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a1g"));
    }
}
