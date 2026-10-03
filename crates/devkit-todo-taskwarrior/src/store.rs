use std::path::PathBuf;

use anyhow::{Result, anyhow};
use devkit_common::store::with_file_lock;
use devkit_todo::{
    Edit, Filter, NewTodo, ORDER_GAP, Status, Todo, TodoStore, by_prefix, is_uuid_prefix, one_line,
    state_dir, transition,
};

use crate::{
    cli::Cli,
    schema::{DEFAULT_ROOT, escaped, project_arg, status_args},
};

const STATUSES: &str = "(status:pending or status:completed or status:deleted)";

/// Todos kept in the local taskwarrior, run as `program`, under one root
/// project. See the crate docs for the race a person's own `task` call can
/// lose.
pub struct TaskwarriorStore {
    program: String,
    env: Vec<(String, String)>,
    lock: PathBuf,
    root: String,
}

impl TaskwarriorStore {
    /// Serializes devkit's own read-then-write edits on a lock in
    /// [`state_dir`].
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            env: Vec::new(),
            lock: state_dir().join("taskwarrior.lock"),
            root: DEFAULT_ROOT.to_string(),
        }
    }

    /// The project every todo is filed under. A task outside it is never a
    /// todo.
    pub fn with_root(self, root: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            ..self
        }
    }

    /// Variables set on every `task` call, such as `TASKRC` and `TASKDATA`.
    pub fn with_env(self, env: Vec<(String, String)>) -> Self {
        Self { env, ..self }
    }

    pub fn with_lock_at(self, lock: PathBuf) -> Self {
        Self { lock, ..self }
    }

    fn cli(&self) -> Cli<'_> {
        Cli {
            program: &self.program,
            env: &self.env,
        }
    }

    fn locked<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        with_file_lock(&self.lock, f)
    }

    fn todos(&self, filter: &[String]) -> Result<Vec<Todo>> {
        Ok(self
            .cli()
            .export(filter)?
            .into_iter()
            .filter_map(|task| task.into_todo(&self.root))
            .collect())
    }

    /// Every todo under the root. A node never reaches a `task` filter: no
    /// quoting survives a name holding both `'` and `"`, so nodes are matched
    /// here instead.
    fn every_todo(&self) -> Result<Vec<Todo>> {
        self.todos(&[in_root(&self.root), STATUSES.into()])
    }

    fn resolve(&self, id: &str) -> Result<Todo> {
        self.get(id)?.ok_or_else(|| anyhow!("no todo {id}"))
    }

    /// The order that places a todo after the last of its siblings: the todos
    /// on `project` under `parent`, other than `skip`.
    fn after_last(
        &self,
        project: Option<&str>,
        parent: Option<&str>,
        skip: Option<&str>,
    ) -> Result<i64> {
        let siblings = self.every_todo()?;
        Ok(siblings
            .iter()
            .filter(|t| {
                t.project.as_deref() == project
                    && t.parent.as_deref() == parent
                    && Some(t.id.as_str()) != skip
            })
            .filter_map(|t| t.order)
            .max()
            .unwrap_or(0)
            + ORDER_GAP)
    }

    fn modify(&self, uuid: &str, words: &[String]) -> Result<()> {
        let words: Vec<String> = std::iter::once("modify".to_string())
            .chain(words.iter().cloned())
            .collect();
        self.cli().write(&[uuid], &words)
    }

    fn set_status(&self, todo: &Todo, next: &Status) -> Result<()> {
        // `done` refuses a deleted task, so a cancelled todo reopens first.
        if let (Status::Cancelled { .. }, Status::Completed { .. }) = (&todo.status, next) {
            self.modify(&todo.id, &["status:pending".into()])?;
        }
        let started = matches!(todo.status, Status::InProgress { .. });
        self.cli().write(&[&todo.id], &status_args(next, started))
    }
}

/// The filter for every task under `root`: the root itself and whatever sits
/// below `root.`. A bare `project:root` would also match a sibling project
/// such as `root-web`. The root is a config value that holds no quote, so
/// double quotes keep a space in it one value.
fn in_root(root: &str) -> String {
    format!(r#"(project.is:"{root}" or project:"{root}.")"#)
}

impl TodoStore for TaskwarriorStore {
    fn list(&self, filter: &Filter) -> Result<Vec<Todo>> {
        if filter.nodes.is_empty() {
            return Ok(Vec::new());
        }
        let mut todos = self.every_todo()?;
        todos.retain(|t| filter.matches(t.project.as_deref()));
        Ok(todos)
    }

    fn get(&self, id: &str) -> Result<Option<Todo>> {
        if !is_uuid_prefix(id) {
            return Ok(None);
        }
        by_prefix(id, self.todos(&[id.to_string()])?)
    }

    fn add(&self, todo: NewTodo) -> Result<String> {
        let parent = match &todo.parent {
            Some(parent) => Some(self.resolve(parent)?.id),
            None => None,
        };
        let add = |order: i64| {
            let mut words = vec![
                project_arg(&self.root, todo.project.as_deref()),
                format!("order:{order}"),
            ];
            words.extend(parent.as_ref().map(|p| format!("subof:{p}")));
            words.push("--".into());
            words.push(escaped(&one_line(&todo.description)));
            self.cli().add(&words)
        };
        match todo.order {
            Some(order) => add(order),
            None => self.locked(|| {
                add(self.after_last(todo.project.as_deref(), parent.as_deref(), None)?)
            }),
        }
    }

    fn apply(&self, edit: &Edit) -> Result<()> {
        match edit {
            Edit::SetStatus { id, to, actor } => self.locked(|| {
                let todo = self.resolve(id)?;
                match transition(&todo.status, *to, actor)? {
                    Some(next) => self.set_status(&todo, &next),
                    None => Ok(()),
                }
            }),
            Edit::Describe { id, description } => {
                let todo = self.resolve(id)?;
                self.modify(&todo.id, &["--".into(), escaped(&one_line(description))])
            }
            Edit::Move { id, parent, order } => {
                let todo = self.resolve(id)?;
                let parent = match parent {
                    Some(parent) => Some(self.resolve(parent)?.id),
                    None => None,
                };
                let place = |order: i64| {
                    self.modify(&todo.id, &[
                        format!("subof:{}", parent.as_deref().unwrap_or_default()),
                        format!("order:{order}"),
                    ])
                };
                match order {
                    Some(order) => place(*order),
                    None => self.locked(|| {
                        place(self.after_last(
                            todo.project.as_deref(),
                            parent.as_deref(),
                            Some(&todo.id),
                        )?)
                    }),
                }
            }
            Edit::Reorder { id, order } => {
                let todo = self.resolve(id)?;
                self.modify(&todo.id, &[format!("order:{order}")])
            }
            Edit::Relocate { id, project } => {
                let todo = self.resolve(id)?;
                self.modify(&todo.id, &[project_arg(&self.root, project.as_deref())])
            }
            Edit::ReleaseAll { holder } => {
                if holder.is_human() {
                    return Ok(());
                }
                self.locked(|| {
                    let active = self.todos(&[in_root(&self.root), "+ACTIVE".into()])?;
                    let held: Vec<&str> = active
                        .iter()
                        .filter(
                            |t| matches!(&t.status, Status::InProgress { by } if holder.covers(by)),
                        )
                        .map(|t| t.id.as_str())
                        .collect();
                    if held.is_empty() {
                        return Ok(());
                    }
                    let words = ["modify", "start:", "holder:"].map(String::from);
                    self.cli().write(&held, &words)
                })
            }
            Edit::Purge(id) => {
                let todo = self.resolve(id)?;
                if !matches!(todo.status, Status::Cancelled { .. }) {
                    self.cli().write(&[&todo.id], &["delete".into()])?;
                }
                self.cli().write(&[&todo.id], &["purge".into()])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_root_filter_keeps_sibling_projects_out() {
        assert_eq!(
            in_root("devkit"),
            r#"(project.is:"devkit" or project:"devkit.")"#
        );
    }
}
