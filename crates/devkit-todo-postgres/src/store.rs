use std::sync::Arc;

use anyhow::{Result, anyhow};
use devkit_todo::{
    Claimed, Edit, Filter, Holder, NewTodo, NodeMatch, Status, StatusChange, StatusKind, Todo,
    TodoStore, activity::stamp, by_prefix, is_uuid_prefix, node::GLOBAL, one_line,
};
use tokio_postgres::{
    Client, Row,
    types::{ToSql, Type},
};

use crate::{
    Database, PostgresActivity,
    database::{CLAIMED, UNKNOWN_TODO},
};

const COLUMNS: &str =
    "id::text, node, description, status, holder, parent::text, ord, entry, modified";

/// The columns of a row a todo function returns for each status change.
const CHANGE: &str = "todo::text, node, from_status, from_holder, to_status, to_holder, at";

/// Todos kept in a Postgres database under one `[todo.postgres] root`.
/// A todo under any other root is never listed, claimed or released.
///
/// Every write is one call to a function in the `devkit` schema. A status
/// change there locks the todo's row, applies the claim rule
/// [`devkit_todo::transition`] states, and writes the result, so the rule
/// holds for any number of processes on any number of machines, and for any
/// client that can call a function. Two todos added at once under the same
/// parent without an order may share one.
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

    /// The activity log kept beside these todos, under the same root.
    pub fn activity(&self) -> PostgresActivity {
        PostgresActivity::new(self.db.clone(), &self.root)
    }
}

/// The status a todo's `status` and `holder` columns store.
fn status_of(id: &str, status: &str, holder: Option<String>) -> Result<Status> {
    Ok(match (status, holder.map(Holder::new)) {
        ("pending", _) => Status::Pending,
        ("in_progress", Some(by)) => Status::InProgress { by },
        ("completed", by) => Status::Completed { by },
        ("cancelled", by) => Status::Cancelled { by },
        (other, _) => anyhow::bail!("todo {id} has an unknown status {other:?}"),
    })
}

fn todo_of(row: &Row) -> Result<Todo> {
    Ok(Todo {
        id: row.get(0),
        project: row.get(1),
        description: row.get(2),
        status: status_of(row.get(0), row.get(3), row.get(4))?,
        parent: row.get(5),
        order: Some(row.get(6)),
        entry: Some(stamp(row.get(7))),
        modified: Some(stamp(row.get(8))),
    })
}

fn change_of(row: &Row) -> Result<StatusChange> {
    let todo: String = row.get(0);
    let to = match row.get::<_, Option<&str>>(4) {
        Some(status) => Some(status_of(&todo, status, row.get(5))?),
        None => None,
    };
    Ok(StatusChange {
        from: status_of(&todo, row.get(2), row.get(3))?,
        node: row
            .get::<_, Option<String>>(1)
            .unwrap_or_else(|| GLOBAL.to_string()),
        todo,
        to,
        at: row.get(6),
    })
}

fn kind_name(kind: StatusKind) -> &'static str {
    match kind {
        StatusKind::Pending => "pending",
        StatusKind::InProgress => "in_progress",
        StatusKind::Completed => "completed",
        StatusKind::Cancelled => "cancelled",
    }
}

/// The todo under `root` whose id starts with `id`.
async fn lookup(db: &Client, root: &str, id: &str) -> Result<Option<Todo>> {
    let id = id.to_ascii_lowercase();
    let sql =
        format!("SELECT {COLUMNS} FROM devkit.todos WHERE root = $1 AND starts_with(id::text, $2)");
    let rows = db
        .query_typed(&sql, &[(&root, Type::TEXT), (&id, Type::TEXT)])
        .await?;
    by_prefix(&id, rows.iter().map(todo_of).collect::<Result<Vec<_>>>()?)
}

type Params<'a> = [(&'a (dyn ToSql + Sync), Type)];

/// Runs one call to a todo function, its refusals read as the errors every
/// store returns.
async fn call(db: &Client, sql: &str, params: &Params<'_>) -> Result<Vec<Row>> {
    db.query_typed(sql, params).await.map_err(|e| {
        match e.as_db_error().map(|db| (db.code().code(), db)) {
            Some((CLAIMED, db)) => Claimed {
                by: Holder::new(db.detail().unwrap_or_default()),
            }
            .into(),
            Some((UNKNOWN_TODO, db)) => anyhow!("{}", db.message()),
            _ => e.into(),
        }
    })
}

/// The status changes the todo function `function` reports making.
async fn changes(db: &Client, function: &str, params: &Params<'_>) -> Result<Vec<StatusChange>> {
    let args = (1..=params.len())
        .map(|n| format!("${n}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("SELECT {CHANGE} FROM devkit.{function}({args})");
    call(db, &sql, params)
        .await?
        .iter()
        .map(change_of)
        .collect()
}

impl TodoStore for PostgresStore {
    fn list(&self, filter: &Filter) -> Result<Vec<Todo>> {
        let (mut exact, mut subtrees) = (Vec::new(), Vec::new());
        for m in &filter.nodes {
            match m {
                NodeMatch::Exact(node) => exact.push(node.as_str()),
                NodeMatch::Subtree(node) => subtrees.push(node.as_str()),
                NodeMatch::SessionDescendants { node, .. } => subtrees.push(node.as_str()),
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
        self.db
            .run(async |client| {
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
            .map(|todos: Vec<Todo>| {
                todos
                    .into_iter()
                    .filter(|todo| filter.matches(todo.project.as_deref()))
                    .collect()
            })
    }

    fn get(&self, id: &str) -> Result<Option<Todo>> {
        if !is_uuid_prefix(id) {
            return Ok(None);
        }
        self.db
            .run(async |client| lookup(client, &self.root, id).await)
    }

    fn add(&self, todo: NewTodo) -> Result<String> {
        let description = one_line(&todo.description);
        self.db.run(async |client| {
            let rows = call(
                client,
                "SELECT devkit.todo_add($1, $2, $3, $4, $5)::text",
                &[
                    (&self.root, Type::TEXT),
                    (&todo.project, Type::TEXT),
                    (&description, Type::TEXT),
                    (&todo.parent, Type::TEXT),
                    (&todo.order, Type::INT8),
                ],
            )
            .await?;
            Ok(rows[0].get(0))
        })
    }

    fn apply(&self, edit: &Edit) -> Result<Vec<StatusChange>> {
        let root = self.root.as_str();
        self.db.run(async |client| match edit {
            Edit::SetStatus { id, to, actor } => {
                let actor: &str = actor;
                changes(client, "todo_set_status", &[
                    (&root, Type::TEXT),
                    (id, Type::TEXT),
                    (&kind_name(*to), Type::TEXT),
                    (&actor, Type::TEXT),
                ])
                .await
            }
            Edit::TakeOver { id, from, actor } => {
                let (from, actor): (&str, &str) = (from, actor);
                changes(client, "todo_take_over", &[
                    (&root, Type::TEXT),
                    (id, Type::TEXT),
                    (&from, Type::TEXT),
                    (&actor, Type::TEXT),
                ])
                .await
            }
            Edit::Describe { id, description } => {
                let description = one_line(description);
                call(client, "SELECT devkit.todo_describe($1, $2, $3)", &[
                    (&root, Type::TEXT),
                    (id, Type::TEXT),
                    (&description, Type::TEXT),
                ])
                .await?;
                Ok(Vec::new())
            }
            Edit::Move { id, parent, order } => {
                call(client, "SELECT devkit.todo_move($1, $2, $3, $4)", &[
                    (&root, Type::TEXT),
                    (id, Type::TEXT),
                    (parent, Type::TEXT),
                    (order, Type::INT8),
                ])
                .await?;
                Ok(Vec::new())
            }
            Edit::Reorder { id, order } => {
                call(client, "SELECT devkit.todo_reorder($1, $2, $3)", &[
                    (&root, Type::TEXT),
                    (id, Type::TEXT),
                    (order, Type::INT8),
                ])
                .await?;
                Ok(Vec::new())
            }
            Edit::Relocate { id, project } => {
                call(client, "SELECT devkit.todo_relocate($1, $2, $3)", &[
                    (&root, Type::TEXT),
                    (id, Type::TEXT),
                    (project, Type::TEXT),
                ])
                .await?;
                Ok(Vec::new())
            }
            Edit::ReleaseAll { holder } => {
                let holder: &str = holder;
                changes(client, "todo_release_all", &[
                    (&root, Type::TEXT),
                    (&holder, Type::TEXT),
                ])
                .await
            }
            Edit::Purge(id) => {
                changes(client, "todo_purge", &[
                    (&root, Type::TEXT),
                    (id, Type::TEXT),
                ])
                .await
            }
        })
    }
}
