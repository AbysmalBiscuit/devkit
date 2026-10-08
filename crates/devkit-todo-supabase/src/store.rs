use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use devkit_todo::{
    Edit, Filter, Holder, NewTodo, NodeMatch, Status, StatusChange, Todo, TodoStore,
    activity::stamp, by_prefix, is_uuid_prefix, node::GLOBAL, one_line,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{Api, SupabaseActivity};

const COLUMNS: &str = "id,node,description,status,holder,parent,ord,entry,modified";

/// How many todos one list request asks for. A project whose API answers
/// fewer per request is read in more requests.
const PAGE: usize = 1000;

/// Todos kept in the postgres backend's tables under one root, reached over
/// a Supabase project's Data API. A todo under any other root is never
/// listed, claimed or released.
///
/// Every write is one call to the same database function the postgres
/// backend calls, so a claim is as atomic here as there, and machines on
/// either backend share one list.
pub struct SupabaseStore {
    api: Arc<Api>,
    root: String,
}

impl SupabaseStore {
    pub fn new(api: Arc<Api>, root: impl Into<String>) -> Self {
        Self {
            api,
            root: root.into(),
        }
    }

    /// The activity log kept beside these todos, under the same root.
    pub fn activity(&self) -> SupabaseActivity {
        SupabaseActivity::new(self.api.clone(), &self.root)
    }
}

#[derive(Deserialize)]
struct Row {
    id: String,
    node: Option<String>,
    description: String,
    status: String,
    holder: Option<String>,
    parent: Option<String>,
    ord: i64,
    entry: String,
    modified: String,
}

/// A row a todo function returns for each status change it made.
#[derive(Deserialize)]
struct Change {
    todo: String,
    node: Option<String>,
    from_status: String,
    from_holder: Option<String>,
    to_status: Option<String>,
    to_holder: Option<String>,
    at: String,
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

fn time(id: &str, at: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(at)
        .map(|at| at.to_utc())
        .with_context(|| format!("todo {id} has an unreadable time {at:?}"))
}

fn todo_of(row: Row) -> Result<Todo> {
    Ok(Todo {
        status: status_of(&row.id, &row.status, row.holder)?,
        entry: Some(stamp(time(&row.id, &row.entry)?)),
        modified: Some(stamp(time(&row.id, &row.modified)?)),
        id: row.id,
        project: row.node,
        description: row.description,
        parent: row.parent,
        order: Some(row.ord),
    })
}

fn change_of(row: Change) -> Result<StatusChange> {
    let to = match row.to_status.as_deref() {
        Some(status) => Some(status_of(&row.todo, status, row.to_holder)?),
        None => None,
    };
    Ok(StatusChange {
        from: status_of(&row.todo, &row.from_status, row.from_holder)?,
        node: row.node.unwrap_or_else(|| GLOBAL.to_string()),
        at: time(&row.todo, &row.at)?,
        todo: row.todo,
        to,
    })
}

/// `value` as a PostgREST filter value, quoted so no character in it reads
/// as syntax.
fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A PostgREST `or` filter selecting at least the nodes `filter` covers,
/// `None` when it covers every node. `filter` itself decides each todo.
fn nodes_filter(filter: &Filter) -> Option<String> {
    let mut terms = Vec::new();
    for m in &filter.nodes {
        let (node, subtree) = match m {
            NodeMatch::Exact(node) => (node, false),
            NodeMatch::Subtree(node) if node.is_empty() => return None,
            NodeMatch::Subtree(node) | NodeMatch::SessionDescendants { node, .. } => (node, true),
        };
        terms.push(format!("node.eq.{}", quoted(node)));
        if subtree {
            terms.push(format!("node.like.{}", quoted(&format!("{node}.*"))));
        }
        if node == GLOBAL {
            terms.push("node.is.null".to_string());
        }
    }
    Some(format!("({})", terms.join(",")))
}

/// The lowest and highest uuids that start with `prefix`, `None` when no
/// uuid can.
fn uuid_bounds(prefix: &str) -> Option<(String, String)> {
    const ZERO: &str = "00000000-0000-0000-0000-000000000000";
    let fits = prefix.len() <= ZERO.len()
        && prefix
            .bytes()
            .zip(ZERO.bytes())
            .all(|(c, slot)| (c == b'-') == (slot == b'-'));
    fits.then(|| {
        let rest = &ZERO[prefix.len()..];
        (
            format!("{prefix}{rest}"),
            format!("{prefix}{}", rest.replace('0', "f")),
        )
    })
}

impl SupabaseStore {
    fn rows(&self, mut query: Vec<(&str, String)>) -> Result<Vec<Todo>> {
        query.extend([
            ("select", COLUMNS.to_string()),
            ("root", format!("eq.{}", self.root)),
            ("order", "node,ord,id".to_string()),
            ("limit", PAGE.to_string()),
        ]);
        let mut todos = Vec::new();
        loop {
            let mut page = query.clone();
            page.push(("offset", todos.len().to_string()));
            let (rows, total) = self.api.select_page::<Row>("todos", &page)?;
            let fetched = rows.len();
            for row in rows {
                todos.push(todo_of(row)?);
            }
            if fetched == 0 || total.is_none_or(|total| todos.len() >= total) {
                return Ok(todos);
            }
        }
    }

    /// Calls the todo function `function`, which returns nothing.
    fn call(&self, function: &str, args: Value) -> Result<Vec<StatusChange>> {
        self.api.call(function, &args)?;
        Ok(Vec::new())
    }

    /// The status changes the todo function `function` reports making.
    fn changes(&self, function: &str, args: Value) -> Result<Vec<StatusChange>> {
        let resp = self.api.call(function, &args)?;
        self.api
            .body::<Vec<Change>>(resp)?
            .into_iter()
            .map(change_of)
            .collect()
    }
}

impl TodoStore for SupabaseStore {
    fn list(&self, filter: &Filter) -> Result<Vec<Todo>> {
        if filter.nodes.is_empty() {
            return Ok(Vec::new());
        }
        let query = nodes_filter(filter)
            .map(|or| vec![("or", or)])
            .unwrap_or_default();
        Ok(self
            .rows(query)?
            .into_iter()
            .filter(|todo| filter.matches(todo.project.as_deref()))
            .collect())
    }

    fn get(&self, id: &str) -> Result<Option<Todo>> {
        if !is_uuid_prefix(id) {
            return Ok(None);
        }
        let id = id.to_ascii_lowercase();
        let Some((low, high)) = uuid_bounds(&id) else {
            return Ok(None);
        };
        by_prefix(
            &id,
            self.rows(vec![
                ("id", format!("gte.{low}")),
                ("id", format!("lte.{high}")),
            ])?,
        )
    }

    fn add(&self, todo: NewTodo) -> Result<String> {
        let resp = self.api.call(
            "todo_add",
            &json!({
                "p_root": self.root,
                "p_node": todo.project,
                "p_description": one_line(&todo.description),
                "p_parent": todo.parent,
                "p_ord": todo.order,
            }),
        )?;
        self.api.body(resp)
    }

    fn apply(&self, edit: &Edit) -> Result<Vec<StatusChange>> {
        let root = &self.root;
        match edit {
            Edit::SetStatus { id, to, actor } => self.changes(
                "todo_set_status",
                json!({ "p_root": root, "p_id": id, "p_to": to, "p_actor": actor }),
            ),
            Edit::TakeOver { id, from, actor } => self.changes(
                "todo_take_over",
                json!({ "p_root": root, "p_id": id, "p_from": from, "p_actor": actor }),
            ),
            Edit::Describe { id, description } => self.call(
                "todo_describe",
                json!({ "p_root": root, "p_id": id, "p_description": one_line(description) }),
            ),
            Edit::Move { id, parent, order } => self.call(
                "todo_move",
                json!({ "p_root": root, "p_id": id, "p_parent": parent, "p_ord": order }),
            ),
            Edit::Reorder { id, order } => self.call(
                "todo_reorder",
                json!({ "p_root": root, "p_id": id, "p_ord": order }),
            ),
            Edit::Relocate { id, project } => self.call(
                "todo_relocate",
                json!({ "p_root": root, "p_id": id, "p_node": project }),
            ),
            Edit::ReleaseAll { holder } => self.changes(
                "todo_release_all",
                json!({ "p_root": root, "p_holder": holder }),
            ),
            Edit::Purge(id) => self.changes("todo_purge", json!({ "p_root": root, "p_id": id })),
        }
    }
}
