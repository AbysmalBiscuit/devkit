use std::sync::Arc;

use anyhow::{Result, anyhow};
use chrono::{DateTime, SecondsFormat, Utc};
use devkit_todo::{
    Edit, Filter, Holder, NewTodo, NodeMatch, ORDER_GAP, Status, StatusChange, Todo, TodoStore,
    by_prefix, is_uuid_prefix, node::GLOBAL, one_line, transition,
};
use tokio_postgres::{GenericClient, Row, types::Type};

use crate::{Database, PostgresActivity};

const COLUMNS: &str =
    "id::text, node, description, status, holder, parent::text, ord, entry, modified";

/// Todos kept in a Postgres database under one root, the `[todo] project`.
/// A todo under any other root is never listed, claimed or released.
///
/// A status change locks the todo's row, applies [`transition`] to what it
/// reads there, and writes the result in the same transaction, so the claim
/// rule holds for any number of processes on any number of machines. Two
/// todos added at once under the same parent without an order may share
/// one.
pub struct PostgresStore {
    db: Arc<Database>,
    root: String,
}

impl PostgresStore {
    pub fn new(db: Arc<Database>, root: impl Into<String>) -> Self {
        Self {
            db,
            root: root.into(),
        }
    }

    pub fn database(&self) -> &Arc<Database> {
        &self.db
    }

    /// The activity log kept beside these todos, under the same root.
    pub fn activity(&self) -> PostgresActivity {
        PostgresActivity::new(self.db.clone(), &self.root)
    }
}

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn todo_of(row: &Row) -> Result<Todo> {
    let holder = row.get::<_, Option<String>>(4).map(Holder::new);
    let status = match (row.get::<_, &str>(3), holder) {
        ("pending", _) => Status::Pending,
        ("in_progress", Some(by)) => Status::InProgress { by },
        ("completed", by) => Status::Completed { by },
        ("cancelled", by) => Status::Cancelled { by },
        (other, _) => anyhow::bail!(
            "todo {} has an unknown status {other:?}",
            row.get::<_, &str>(0)
        ),
    };
    Ok(Todo {
        id: row.get(0),
        project: row.get(1),
        description: row.get(2),
        status,
        parent: row.get(5),
        order: Some(row.get(6)),
        entry: Some(stamp(row.get(7))),
        modified: Some(stamp(row.get(8))),
    })
}

/// The status column and holder `status` is stored as.
fn columns_of(status: &Status) -> (&'static str, Option<&str>) {
    match status {
        Status::Pending => ("pending", None),
        Status::InProgress { by } => ("in_progress", Some(by)),
        Status::Completed { by } => ("completed", by.as_deref()),
        Status::Cancelled { by } => ("cancelled", by.as_deref()),
    }
}

/// The todos under `root` whose id starts with `id`, locked for the rest of
/// the transaction when `lock` is set.
async fn find(db: &impl GenericClient, root: &str, id: &str, lock: bool) -> Result<Option<Todo>> {
    if !is_uuid_prefix(id) {
        return Ok(None);
    }
    let id = id.to_ascii_lowercase();
    let lock = if lock { " FOR UPDATE" } else { "" };
    let sql = format!(
        "SELECT {COLUMNS} FROM devkit.todos WHERE root = $1 AND starts_with(id::text, $2){lock}"
    );
    let rows = db
        .query_typed(&sql, &[(&root, Type::TEXT), (&id, Type::TEXT)])
        .await?;
    by_prefix(&id, rows.iter().map(todo_of).collect::<Result<Vec<_>>>()?)
}

async fn resolve(db: &impl GenericClient, root: &str, id: &str, lock: bool) -> Result<Todo> {
    find(db, root, id, lock)
        .await?
        .ok_or_else(|| anyhow!("no todo {id}"))
}

/// The full id of the parent `id` names, `None` for the top level.
async fn resolve_parent(
    db: &impl GenericClient,
    root: &str,
    id: Option<&str>,
) -> Result<Option<String>> {
    match id {
        Some(id) => Ok(Some(resolve(db, root, id, false).await?.id)),
        None => Ok(None),
    }
}

/// The order that places a todo after the last of its siblings: the todos
/// on `node` under `parent`, other than `skip`.
async fn after_last(
    db: &impl GenericClient,
    root: &str,
    node: Option<&str>,
    parent: Option<&str>,
    skip: Option<&str>,
) -> Result<i64> {
    let row = db
        .query_typed_one(
            "SELECT coalesce(max(ord), 0) + $5 FROM devkit.todos
             WHERE root = $1 AND node IS NOT DISTINCT FROM $2
               AND parent IS NOT DISTINCT FROM $3::uuid AND id::text IS DISTINCT FROM $4",
            &[
                (&root, Type::TEXT),
                (&node, Type::TEXT),
                (&parent, Type::TEXT),
                (&skip, Type::TEXT),
                (&ORDER_GAP, Type::INT8),
            ],
        )
        .await?;
    Ok(row.get(0))
}

/// Writes one todo's column and stamps it modified. Returns when the
/// database made the change.
async fn update(
    db: &impl GenericClient,
    id: &str,
    set: &str,
    value: &(dyn tokio_postgres::types::ToSql + Sync),
    kind: Type,
) -> Result<DateTime<Utc>> {
    let sql = format!(
        "UPDATE devkit.todos SET {set} = $2, modified = clock_timestamp()
         WHERE id = $1::uuid RETURNING modified"
    );
    let row = db
        .query_typed_one(&sql, &[(&id, Type::TEXT), (value, kind)])
        .await?;
    Ok(row.get(0))
}

async fn write_status(db: &impl GenericClient, id: &str, status: &Status) -> Result<DateTime<Utc>> {
    let (status, holder) = columns_of(status);
    let row = db
        .query_typed_one(
            "UPDATE devkit.todos SET status = $2, holder = $3, modified = clock_timestamp()
             WHERE id = $1::uuid RETURNING modified",
            &[
                (&id, Type::TEXT),
                (&status, Type::TEXT),
                (&holder, Type::TEXT),
            ],
        )
        .await?;
    Ok(row.get(0))
}

fn change(todo: &Todo, to: Option<Status>, at: DateTime<Utc>) -> StatusChange {
    StatusChange {
        todo: todo.id.clone(),
        node: todo.node().to_string(),
        from: todo.status.clone(),
        to,
        at,
    }
}

impl TodoStore for PostgresStore {
    fn list(&self, filter: &Filter) -> Result<Vec<Todo>> {
        let (mut exact, mut subtrees) = (Vec::new(), Vec::new());
        for m in &filter.nodes {
            match m {
                NodeMatch::Exact(node) => exact.push(node.as_str()),
                NodeMatch::Subtree(node) => subtrees.push(node.as_str()),
            }
        }
        let sql = format!(
            "SELECT {COLUMNS} FROM devkit.todos AS t
             CROSS JOIN LATERAL (SELECT coalesce(t.node, $4) AS name) AS n
             WHERE root = $1 AND (n.name = ANY($2) OR EXISTS (
                 SELECT FROM unnest($3::text[]) AS s(node)
                 WHERE s.node = '' OR n.name = s.node OR starts_with(n.name, s.node || '.')))
             ORDER BY node, ord"
        );
        self.db.run(async |client| {
            client
                .query_typed(&sql, &[
                    (&self.root, Type::TEXT),
                    (&exact, Type::TEXT_ARRAY),
                    (&subtrees, Type::TEXT_ARRAY),
                    (&GLOBAL, Type::TEXT),
                ])
                .await?
                .iter()
                .map(todo_of)
                .collect()
        })
    }

    fn get(&self, id: &str) -> Result<Option<Todo>> {
        if !is_uuid_prefix(id) {
            return Ok(None);
        }
        self.db
            .run(async |client| find(client, &self.root, id, false).await)
    }

    fn add(&self, todo: NewTodo) -> Result<String> {
        let description = one_line(&todo.description);
        self.db.run(async |client| {
            let tx = client.transaction().await?;
            let parent = resolve_parent(&tx, &self.root, todo.parent.as_deref()).await?;
            let order = match todo.order {
                Some(order) => order,
                None => {
                    after_last(
                        &tx,
                        &self.root,
                        todo.project.as_deref(),
                        parent.as_deref(),
                        None,
                    )
                    .await?
                }
            };
            let row = tx
                .query_typed_one(
                    "INSERT INTO devkit.todos (root, node, description, status, parent, ord)
                     VALUES ($1, $2, $3, 'pending', $4::uuid, $5) RETURNING id::text",
                    &[
                        (&self.root, Type::TEXT),
                        (&todo.project, Type::TEXT),
                        (&description, Type::TEXT),
                        (&parent, Type::TEXT),
                        (&order, Type::INT8),
                    ],
                )
                .await?;
            tx.commit().await?;
            Ok(row.get(0))
        })
    }

    fn apply(&self, edit: &Edit) -> Result<Vec<StatusChange>> {
        let root = self.root.as_str();
        self.db.run(async |client| {
            let tx = client.transaction().await?;
            let changes = match edit {
                Edit::SetStatus { id, to, actor } => {
                    let todo = resolve(&tx, root, id, true).await?;
                    match transition(&todo.status, *to, actor)? {
                        Some(next) => {
                            let at = write_status(&tx, &todo.id, &next).await?;
                            vec![change(&todo, Some(next), at)]
                        }
                        None => Vec::new(),
                    }
                }
                Edit::Describe { id, description } => {
                    let todo = resolve(&tx, root, id, true).await?;
                    let description = one_line(description);
                    update(&tx, &todo.id, "description", &description, Type::TEXT).await?;
                    Vec::new()
                }
                Edit::Move { id, parent, order } => {
                    let todo = resolve(&tx, root, id, true).await?;
                    let parent = resolve_parent(&tx, root, parent.as_deref()).await?;
                    let order = match order {
                        Some(order) => *order,
                        None => {
                            after_last(
                                &tx,
                                root,
                                todo.project.as_deref(),
                                parent.as_deref(),
                                Some(&todo.id),
                            )
                            .await?
                        }
                    };
                    tx.query_typed(
                        "UPDATE devkit.todos SET parent = $2::uuid, ord = $3,
                         modified = clock_timestamp() WHERE id = $1::uuid",
                        &[
                            (&todo.id, Type::TEXT),
                            (&parent, Type::TEXT),
                            (&order, Type::INT8),
                        ],
                    )
                    .await?;
                    Vec::new()
                }
                Edit::Reorder { id, order } => {
                    let todo = resolve(&tx, root, id, true).await?;
                    update(&tx, &todo.id, "ord", order, Type::INT8).await?;
                    Vec::new()
                }
                Edit::Relocate { id, project } => {
                    let todo = resolve(&tx, root, id, true).await?;
                    update(&tx, &todo.id, "node", project, Type::TEXT).await?;
                    Vec::new()
                }
                Edit::ReleaseAll { holder } if holder.is_human() => Vec::new(),
                Edit::ReleaseAll { holder } => {
                    let sql = format!(
                        "SELECT {COLUMNS} FROM devkit.todos
                         WHERE root = $1 AND status = 'in_progress'
                           AND (holder = $2 OR starts_with(holder, $2 || '/'))
                         FOR UPDATE"
                    );
                    let holder_text: &str = holder;
                    let held = tx
                        .query_typed(&sql, &[(&root, Type::TEXT), (&holder_text, Type::TEXT)])
                        .await?;
                    let mut changes = Vec::new();
                    for todo in held.iter().map(todo_of) {
                        let todo = todo?;
                        if matches!(&todo.status, Status::InProgress { by } if holder.covers(by)) {
                            let at = write_status(&tx, &todo.id, &Status::Pending).await?;
                            changes.push(change(&todo, Some(Status::Pending), at));
                        }
                    }
                    changes
                }
                Edit::Purge(id) => {
                    let todo = resolve(&tx, root, id, true).await?;
                    let row = tx
                        .query_typed_one(
                            "DELETE FROM devkit.todos WHERE id = $1::uuid
                             RETURNING clock_timestamp()",
                            &[(&todo.id, Type::TEXT)],
                        )
                        .await?;
                    vec![change(&todo, None, row.get(0))]
                }
            };
            tx.commit().await?;
            Ok(changes)
        })
    }
}
