//! The store devkit ships: one JSON document under the state directory,
//! guarded by a file lock. Lists are not per checkout, so removing a worktree
//! never deletes the record of its work.

use std::{collections::BTreeMap, path::PathBuf, time::SystemTime};

use anyhow::{Result, anyhow};
use chrono::{DateTime, SecondsFormat, Utc};
use devkit_common::{paths, store};
use serde::{Deserialize, Serialize};

use crate::{
    Edit, Filter, Holder, NewTodo, ORDER_GAP, Status, Todo, TodoStore, one_line, render, transition,
};

const VERSION: u32 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Doc {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    next_id: u64,
    /// Numeric keys, so `10` sorts after `9`.
    #[serde(default)]
    todos: BTreeMap<u64, Todo>,
}

impl store::Document for Doc {
    fn stamp_version(&mut self) {
        self.version = VERSION;
    }

    /// Required by the trait, never called: the store loads strictly, so an
    /// unreadable file fails the call instead of losing a list.
    fn salvage(raw: &str) -> Option<Self> {
        let todos: BTreeMap<u64, Todo> = store::salvage_map(raw, "todos", |k| k.parse().ok())?;
        let next_id = todos.keys().next_back().map_or(0, |k| k + 1);
        Some(Self {
            version: 0,
            next_id,
            todos,
        })
    }

    fn label() -> &'static str {
        "todo store"
    }

    fn len(&self) -> usize {
        self.todos.len()
    }
}

impl Doc {
    fn todo_mut(&mut self, id: &str) -> Result<&mut Todo> {
        id.parse::<u64>()
            .ok()
            .and_then(|key| self.todos.get_mut(&key))
            .ok_or_else(|| no_todo(id))
    }
}

fn no_todo(id: &str) -> anyhow::Error {
    anyhow!("no todo {id}")
}

fn now() -> String {
    DateTime::<Utc>::from(SystemTime::now()).to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Where the lists last injected for `holder` are fingerprinted, keyed on a
/// hash of the full holder so a sub-agent's injection never suppresses its
/// session's.
pub fn digest_path(holder: &Holder) -> PathBuf {
    BuiltinStore::default_dir()
        .join("digests")
        .join(render::digest(holder))
}

pub struct BuiltinStore {
    dir: PathBuf,
}

impl BuiltinStore {
    /// Keeps `dir/todos.json`, guarded by `dir/todo.lock`.
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn default_dir() -> PathBuf {
        paths::state_dir().join("todo")
    }

    /// The store in [`BuiltinStore::default_dir`].
    pub fn open() -> Self {
        Self::at(Self::default_dir())
    }

    fn with_doc<T>(&self, f: impl FnOnce(&mut Doc) -> Result<T>) -> Result<T> {
        store::with_lock_strict(&self.dir.join("todo.lock"), &self.dir.join("todos.json"), f)
    }
}

impl TodoStore for BuiltinStore {
    fn list(&self, filter: &Filter) -> Result<Vec<Todo>> {
        self.with_doc(|doc| {
            Ok(doc
                .todos
                .values()
                .filter(|t| filter.matches(t.project.as_deref()))
                .cloned()
                .collect())
        })
    }

    fn add(&self, todo: NewTodo) -> Result<String> {
        self.with_doc(|doc| {
            if let Some(parent) = &todo.parent {
                doc.todo_mut(parent)?;
            }
            let order = todo.order.unwrap_or_else(|| {
                doc.todos
                    .values()
                    .filter(|t| t.project == todo.project && t.parent == todo.parent)
                    .filter_map(|t| t.order)
                    .max()
                    .unwrap_or(0)
                    + ORDER_GAP
            });
            let key = doc.next_id.max(1);
            doc.next_id = key + 1;
            let id = key.to_string();
            let stamp = now();
            doc.todos.insert(key, Todo {
                id: id.clone(),
                description: one_line(&todo.description),
                status: Status::Pending,
                parent: todo.parent,
                order: Some(order),
                project: todo.project,
                entry: Some(stamp.clone()),
                modified: Some(stamp),
            });
            Ok(id)
        })
    }

    fn apply(&self, edit: &Edit) -> Result<()> {
        self.with_doc(|doc| {
            match edit {
                Edit::SetStatus { id, to, actor } => {
                    let todo = doc.todo_mut(id)?;
                    if let Some(next) = transition(&todo.status, *to, actor)? {
                        todo.status = next;
                        todo.modified = Some(now());
                    }
                }
                Edit::Describe { id, description } => {
                    let todo = doc.todo_mut(id)?;
                    todo.description = one_line(description);
                    todo.modified = Some(now());
                }
                Edit::Move { id, parent, order } => {
                    if let Some(parent) = parent {
                        doc.todo_mut(parent)?;
                    }
                    let todo = doc.todo_mut(id)?;
                    todo.parent = parent.clone();
                    todo.order = Some(*order);
                    todo.modified = Some(now());
                }
                Edit::Relocate { id, project } => {
                    let todo = doc.todo_mut(id)?;
                    todo.project = project.clone();
                    todo.modified = Some(now());
                }
                Edit::Reorder { id, order } => {
                    let todo = doc.todo_mut(id)?;
                    todo.order = Some(*order);
                    todo.modified = Some(now());
                }
                Edit::ReleaseAll { holder } => {
                    if holder.is_human() {
                        return Ok(());
                    }
                    let stamp = now();
                    for todo in doc.todos.values_mut() {
                        if let Status::InProgress { by } = &todo.status
                            && holder.covers(by)
                        {
                            todo.status = Status::Pending;
                            todo.modified = Some(stamp.clone());
                        }
                    }
                }
                Edit::Purge(id) => {
                    let key = id.parse::<u64>().map_err(|_| no_todo(id))?;
                    doc.todos.remove(&key).ok_or_else(|| no_todo(id))?;
                }
            }
            Ok(())
        })
    }
}
