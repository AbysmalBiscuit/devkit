use std::{
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use devkit_common::store::{with_file_lock, with_file_lock_for};
use devkit_todo::{
    Edit, Filter, NewTodo, ORDER_GAP, Status, Todo, TodoStore, by_prefix, is_uuid_prefix, one_line,
    transition,
};
use devkit_todo_taskwarrior::schema::{DEFAULT_ROOT, Exported, project_of};
use taskchampion::{
    Operations, Replica, Server, ServerConfig, StorageConfig, Task, Uuid, chrono::Utc,
    storage::AccessMode,
};

/// Where a replica syncs.
pub enum SyncTarget {
    /// A directory another replica on this machine also syncs to.
    Dir(PathBuf),
    /// A taskchampion sync server.
    Server {
        url: String,
        client_id: Uuid,
        secret: Vec<u8>,
    },
}

impl fmt::Debug for SyncTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dir(dir) => f.debug_tuple("Dir").field(dir).finish(),
            Self::Server { url, client_id, .. } => f
                .debug_struct("Server")
                .field("url", url)
                .field("client_id", client_id)
                .field("secret", &"<redacted>")
                .finish(),
        }
    }
}

impl SyncTarget {
    fn server(&self) -> Result<Box<dyn Server>> {
        let config = match self {
            Self::Dir(dir) => {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("creating the todo sync dir {}", dir.display()))?;
                ServerConfig::Local {
                    server_dir: dir.clone(),
                }
            }
            Self::Server {
                url,
                client_id,
                secret,
            } => ServerConfig::Remote {
                url: url.clone(),
                client_id: *client_id,
                encryption_secret: secret.clone(),
            },
        };
        Ok(config.into_server()?)
    }
}

/// Todos kept in the taskchampion replica at `data_dir`, under one root
/// project. A task outside the root is never a todo.
pub struct TaskchampionStore {
    data_dir: PathBuf,
    root: String,
    lock_wait: Option<Duration>,
    target: Option<SyncTarget>,
}

impl TaskchampionStore {
    /// A replica with no sync target, whose callers wait as long as the
    /// replica lock is held.
    pub fn at(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            root: DEFAULT_ROOT.to_string(),
            lock_wait: None,
            target: None,
        }
    }

    /// The project every todo is filed under.
    pub fn with_root(self, root: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            ..self
        }
    }

    /// Gives up with a "todo store busy" error once another process has held
    /// the replica lock for `wait`.
    pub fn with_lock_wait(self, wait: Duration) -> Self {
        Self {
            lock_wait: Some(wait),
            ..self
        }
    }

    pub fn with_target(self, target: SyncTarget) -> Self {
        Self {
            target: Some(target),
            ..self
        }
    }

    pub fn target(&self) -> Option<&SyncTarget> {
        self.target.as_ref()
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// One full sync with the target under the replica lock, a no-op with no
    /// target. taskchampion runs a sync as one transaction, so a sync that
    /// stops partway leaves the replica as it was; nothing in devkit stops one
    /// on purpose.
    pub fn sync_once(&self) -> Result<()> {
        let Some(target) = &self.target else {
            return Ok(());
        };
        self.locked(|replica| {
            let mut server = target.server()?;
            replica.sync(&mut server, false)?;
            Ok(())
        })
    }

    /// Runs `f` on the replica, opened under `<data_dir>/devkit.lock`.
    /// taskchampion holds a write transaction across a whole sync, longer than
    /// SQLite's busy timeout, so devkit's processes queue on this lock instead
    /// of failing on a busy database.
    fn locked<T>(&self, f: impl FnOnce(&mut Replica) -> Result<T>) -> Result<T> {
        std::fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("creating the todo replica at {}", self.data_dir.display()))?;
        let lock = self.data_dir.join("devkit.lock");
        let run = || f(&mut self.open()?);
        match self.lock_wait {
            Some(wait) => with_file_lock_for(&lock, wait, run),
            None => with_file_lock(&lock, run),
        }
    }

    fn open(&self) -> Result<Replica> {
        let storage = StorageConfig::OnDisk {
            taskdb_dir: self.data_dir.clone(),
            create_if_missing: true,
            access_mode: AccessMode::ReadWrite,
        }
        .into_storage()
        .with_context(|| format!("opening the todo replica at {}", self.data_dir.display()))?;
        Ok(Replica::new(storage))
    }

    fn todo_of(&self, task: &Task) -> Option<Todo> {
        let value = |key: &str| task.get_value(key).map(str::to_string);
        let stamp = |t: Option<taskchampion::chrono::DateTime<Utc>>| {
            t.map(|t| t.format("%Y%m%dT%H%M%SZ").to_string())
        };
        Exported {
            uuid: task.get_uuid().to_string(),
            description: task.get_description().to_string(),
            status: value("status").unwrap_or_default(),
            start: value("start"),
            holder: value("holder"),
            subof: value("subof"),
            order: task
                .get_value("order")
                .and_then(|o| o.parse::<f64>().ok())
                .map(|o| o.round() as i64),
            project: value("project"),
            entry: stamp(task.get_entry()),
            modified: stamp(task.get_modified()),
        }
        .into_todo(&self.root)
    }

    fn todos(&self, replica: &mut Replica) -> Result<Vec<Todo>> {
        Ok(replica
            .all_tasks()?
            .values()
            .filter_map(|task| self.todo_of(task))
            .collect())
    }

    fn find(&self, replica: &mut Replica, id: &str) -> Result<Option<Todo>> {
        if !is_uuid_prefix(id) {
            return Ok(None);
        }
        if let Ok(uuid) = Uuid::try_parse(id) {
            return Ok(replica.get_task(uuid)?.and_then(|task| self.todo_of(&task)));
        }
        by_prefix(id, self.todos(replica)?)
    }

    fn resolve(&self, replica: &mut Replica, id: &str) -> Result<Todo> {
        self.find(replica, id)?
            .ok_or_else(|| anyhow!("no todo {id}"))
    }

    /// The order that places a todo after the last of its siblings: the todos
    /// on `project` under `parent`, other than `skip`.
    fn after_last(
        &self,
        replica: &mut Replica,
        project: Option<&str>,
        parent: Option<&str>,
        skip: Option<&str>,
    ) -> Result<i64> {
        let node = project_of(&self.root, project);
        Ok(self
            .todos(replica)?
            .iter()
            .filter(|t| project_of(&self.root, t.project.as_deref()) == node)
            .filter(|t| t.parent.as_deref() == parent && Some(t.id.as_str()) != skip)
            .filter_map(|t| t.order)
            .max()
            .unwrap_or(0)
            + ORDER_GAP)
    }

    /// Applies `change` to the task behind `todo` and commits it.
    fn change(
        replica: &mut Replica,
        todo: &Todo,
        change: impl FnOnce(&mut Task, &mut Operations) -> Result<(), taskchampion::Error>,
    ) -> Result<()> {
        let mut task = replica
            .get_task(Uuid::try_parse(&todo.id)?)?
            .ok_or_else(|| anyhow!("no todo {}", todo.id))?;
        let mut ops = Operations::new();
        change(&mut task, &mut ops)?;
        replica.commit_operations(ops)?;
        Ok(())
    }
}

/// Writes `status` the way taskwarrior's `start`, `done` and `delete` do:
/// `set_status` keeps `end` in step, and `start` keeps an existing start time,
/// so a claim handed down to a sub-agent keeps its start.
fn write_status(
    task: &mut Task,
    status: &Status,
    ops: &mut Operations,
) -> Result<(), taskchampion::Error> {
    let holder = |by: &devkit_todo::Holder| Some(by.to_string());
    let stop = |task: &mut Task, ops: &mut Operations| match task.is_active() {
        true => task.stop(ops),
        false => Ok(()),
    };
    match status {
        Status::Pending => {
            task.set_status(taskchampion::Status::Pending, ops)?;
            stop(task, ops)?;
            task.set_value("holder", None, ops)
        }
        Status::InProgress { by } => {
            task.set_status(taskchampion::Status::Pending, ops)?;
            task.start(ops)?;
            task.set_value("holder", holder(by), ops)
        }
        Status::Completed { by } | Status::Cancelled { by } => {
            let next = match status {
                Status::Completed { .. } => taskchampion::Status::Completed,
                _ => taskchampion::Status::Deleted,
            };
            stop(task, ops)?;
            task.set_status(next, ops)?;
            match by {
                Some(by) => task.set_value("holder", holder(by), ops),
                None => Ok(()),
            }
        }
    }
}

impl TodoStore for TaskchampionStore {
    fn list(&self, filter: &Filter) -> Result<Vec<Todo>> {
        self.locked(|replica| {
            Ok(self
                .todos(replica)?
                .into_iter()
                .filter(|t| filter.matches(t.project.as_deref()))
                .collect())
        })
    }

    fn get(&self, id: &str) -> Result<Option<Todo>> {
        self.locked(|replica| self.find(replica, id))
    }

    fn add(&self, todo: NewTodo) -> Result<String> {
        self.locked(|replica| {
            let parent = match &todo.parent {
                Some(parent) => Some(self.resolve(replica, parent)?.id),
                None => None,
            };
            let order = match todo.order {
                Some(order) => order,
                None => {
                    self.after_last(replica, todo.project.as_deref(), parent.as_deref(), None)?
                }
            };
            let uuid = Uuid::new_v4();
            let mut ops = Operations::new();
            let mut task = replica.create_task(uuid, &mut ops)?;
            task.set_description(one_line(&todo.description), &mut ops)?;
            task.set_status(taskchampion::Status::Pending, &mut ops)?;
            task.set_entry(Some(Utc::now()), &mut ops)?;
            let project = project_of(&self.root, todo.project.as_deref());
            task.set_value("project", Some(project), &mut ops)?;
            task.set_value("order", Some(order.to_string()), &mut ops)?;
            task.set_value("subof", parent, &mut ops)?;
            replica.commit_operations(ops)?;
            Ok(uuid.to_string())
        })
    }

    fn apply(&self, edit: &Edit) -> Result<()> {
        self.locked(|replica| match edit {
            Edit::SetStatus { id, to, actor } => {
                let todo = self.resolve(replica, id)?;
                match transition(&todo.status, *to, actor)? {
                    Some(next) => {
                        Self::change(replica, &todo, |task, ops| write_status(task, &next, ops))
                    }
                    None => Ok(()),
                }
            }
            Edit::Describe { id, description } => {
                let todo = self.resolve(replica, id)?;
                Self::change(replica, &todo, |task, ops| {
                    task.set_description(one_line(description), ops)
                })
            }
            Edit::Move { id, parent, order } => {
                let todo = self.resolve(replica, id)?;
                let parent = match parent {
                    Some(parent) => Some(self.resolve(replica, parent)?.id),
                    None => None,
                };
                let order = match order {
                    Some(order) => *order,
                    None => self.after_last(
                        replica,
                        todo.project.as_deref(),
                        parent.as_deref(),
                        Some(&todo.id),
                    )?,
                };
                Self::change(replica, &todo, |task, ops| {
                    task.set_value("subof", parent, ops)?;
                    task.set_value("order", Some(order.to_string()), ops)
                })
            }
            Edit::Reorder { id, order } => {
                let todo = self.resolve(replica, id)?;
                Self::change(replica, &todo, |task, ops| {
                    task.set_value("order", Some(order.to_string()), ops)
                })
            }
            Edit::Relocate { id, project } => {
                let todo = self.resolve(replica, id)?;
                let project = project_of(&self.root, project.as_deref());
                Self::change(replica, &todo, |task, ops| {
                    task.set_value("project", Some(project), ops)
                })
            }
            Edit::ReleaseAll { holder } => {
                if holder.is_human() {
                    return Ok(());
                }
                let mut ops = Operations::new();
                for task in replica.all_tasks()?.values_mut() {
                    let held = matches!(
                        self.todo_of(task).map(|t| t.status),
                        Some(Status::InProgress { by }) if holder.covers(&by)
                    );
                    if held {
                        task.stop(&mut ops)?;
                        task.set_value("holder", None, &mut ops)?;
                    }
                }
                replica.commit_operations(ops)?;
                Ok(())
            }
            Edit::Purge(id) => {
                let todo = self.resolve(replica, id)?;
                let mut data = replica
                    .get_task_data(Uuid::try_parse(&todo.id)?)?
                    .ok_or_else(|| anyhow!("no todo {id}"))?;
                let mut ops = Operations::new();
                data.delete(&mut ops);
                replica.commit_operations(ops)?;
                Ok(())
            }
        })
    }
}
