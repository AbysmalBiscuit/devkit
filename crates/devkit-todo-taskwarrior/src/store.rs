use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use devkit_common::store::with_file_lock;
use devkit_todo::{
    Edit, Filter, NewTodo, NodeMatch, ORDER_GAP, Status, Todo, TodoStore, one_line, state_dir,
    transition,
};

use crate::{
    cli::Cli,
    schema::{Exported, GLOBAL_PROJECT, escaped, project_arg, status_args},
};

const STATUSES: &str = "(status:pending or status:completed or status:deleted)";

/// The shortest uuid prefix an id may be, taskwarrior's `uuid.short`.
const SHORT_ID: usize = 8;

/// Todos kept in the local taskwarrior, run as `program`. See the crate docs
/// for the race a person's own `task` call can lose.
pub struct TaskwarriorStore {
    program: String,
    env: Vec<(String, String)>,
    lock: PathBuf,
}

impl TaskwarriorStore {
    /// Serializes devkit's own read-then-write edits on a lock in
    /// [`state_dir`].
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            env: Vec::new(),
            lock: state_dir().join("taskwarrior.lock"),
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
            .filter_map(Exported::into_todo)
            .collect())
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
        let node = format!("project.is:{}", quoted(project.unwrap_or(GLOBAL_PROJECT)));
        let siblings = self.todos(&[node, STATUSES.into()])?;
        Ok(siblings
            .iter()
            .filter(|t| t.parent.as_deref() == parent && Some(t.id.as_str()) != skip)
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

/// The filter words for `filter`, or `None` when it covers no node. A bare
/// `project:r` is a left match that would also return a repository named
/// `r-web`, so a subtree matches `r` itself and whatever sits below `r.`.
fn node_filter(filter: &Filter) -> Option<String> {
    let nodes: Vec<String> = filter
        .nodes
        .iter()
        .map(|m| match m {
            // The space keeps `)` from being read as the value.
            NodeMatch::Subtree(node) if node.is_empty() => "project.any: ".to_string(),
            NodeMatch::Subtree(node) => format!(
                "project.is:{} or project:{}",
                quoted(node),
                quoted(&format!("{node}."))
            ),
            NodeMatch::Exact(node) => format!("project.is:{}", quoted(node)),
        })
        .collect();
    (!nodes.is_empty()).then(|| format!("({})", nodes.join(" or ")))
}

/// A node as a filter value. Unquoted, taskwarrior splits a name on a space
/// or reads `'` and `(` as syntax; a backslash escape does not stop it.
fn quoted(node: &str) -> String {
    format!("\"{node}\"")
}

/// Whether `id` can name a task by uuid. Anything else would reach `task` as
/// filter syntax.
fn is_uuid_prefix(id: &str) -> bool {
    (SHORT_ID..=36).contains(&id.len()) && id.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
}

impl TodoStore for TaskwarriorStore {
    fn list(&self, filter: &Filter) -> Result<Vec<Todo>> {
        match node_filter(filter) {
            Some(nodes) => self.todos(&[nodes, STATUSES.into()]),
            None => Ok(Vec::new()),
        }
    }

    fn get(&self, id: &str) -> Result<Option<Todo>> {
        if !is_uuid_prefix(id) {
            return Ok(None);
        }
        let mut matches: Vec<Todo> = self
            .todos(&[id.to_string()])?
            .into_iter()
            .filter(|t| t.id.starts_with(id))
            .collect();
        if matches.len() > 1 {
            matches.sort_by(|a, b| a.id.cmp(&b.id));
            let uuids: Vec<&str> = matches.iter().map(|t| t.id.as_str()).collect();
            bail!("todo id {id} is ambiguous: {}", uuids.join(", "));
        }
        Ok(matches.pop())
    }

    fn add(&self, todo: NewTodo) -> Result<String> {
        let parent = match &todo.parent {
            Some(parent) => Some(self.resolve(parent)?.id),
            None => None,
        };
        let add = |order: i64| {
            let mut words = vec![
                project_arg(todo.project.as_deref()),
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
                self.modify(&todo.id, &[project_arg(project.as_deref())])
            }
            Edit::ReleaseAll { holder } => {
                if holder.is_human() {
                    return Ok(());
                }
                self.locked(|| {
                    let active = self.todos(&["project.any:".into(), "+ACTIVE".into()])?;
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
    fn a_subtree_keeps_other_repos_out_and_all_needs_a_project() {
        let filter = Filter {
            nodes: vec![
                NodeMatch::Subtree("r".into()),
                NodeMatch::Exact("global".into()),
            ],
        };
        assert_eq!(
            node_filter(&filter).as_deref(),
            Some(r#"(project.is:"r" or project:"r." or project.is:"global")"#)
        );
        assert_eq!(
            node_filter(&Filter::all()).as_deref(),
            Some("(project.any: )")
        );
        assert_eq!(node_filter(&Filter { nodes: Vec::new() }), None);
    }

    #[test]
    fn only_hex_of_a_short_ids_length_names_a_uuid() {
        assert!(is_uuid_prefix("abcdef12"));
        assert!(is_uuid_prefix("96432cd6-082a-4d8c-a9cb-adef8823ff92"));
        assert!(!is_uuid_prefix("abcdef1"));
        assert!(!is_uuid_prefix("status:pending"));
        assert!(!is_uuid_prefix("abcdef12 or project:x"));
    }
}
