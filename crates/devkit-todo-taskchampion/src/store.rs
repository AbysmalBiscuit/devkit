use std::{
    cell::Cell,
    fmt,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use devkit_common::store::{LockBusy, with_file_lock, with_file_lock_for};
use devkit_todo::{
    Edit, Filter, NewTodo, ORDER_GAP, Status, StatusChange, Todo, TodoStore, by_prefix,
    is_uuid_prefix, one_line, transition,
};
use taskchampion::{
    Operations, Server, ServerConfig, SqliteStorage, Task, Uuid, chrono::Utc, storage::AccessMode,
};
use tokio::runtime::Runtime;

use crate::schema::{Exported, project_of};

type Replica = taskchampion::Replica<SqliteStorage>;

/// Runs `work` on this process's taskchampion runtime, started on first use.
/// It is a current-thread runtime with every driver enabled: sync's HTTP
/// client panics without the timer.
fn block_on<T>(work: impl Future<Output = Result<T>>) -> Result<T> {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    let runtime = match RUNTIME.get() {
        Some(runtime) => runtime,
        None => {
            let built = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("starting the taskchampion runtime")?;
            RUNTIME.get_or_init(|| built)
        }
    };
    runtime.block_on(work)
}

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
    async fn server(&self) -> Result<Box<dyn Server>> {
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
        Ok(config.into_server().await?)
    }
}

/// `text` with the user and password `url` carries removed, since a sync
/// error quotes the URL it was given.
fn redacted(text: &str, url: &str) -> String {
    let authority = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = authority.split(['/', '?', '#']).next().unwrap_or_default();
    let Some((userinfo, _)) = authority.rsplit_once('@') else {
        return text.to_string();
    };
    let password = userinfo.split_once(':').map(|(_, p)| p);
    [Some(userinfo), password]
        .into_iter()
        .flatten()
        .filter(|secret| !secret.is_empty())
        .fold(text.to_string(), |out, secret| {
            out.replace(secret, "<redacted>")
        })
}

/// Debug builds only: with `DEVKIT_TODO_TEST_LOCK_BUSY_AFTER=<n>`, every
/// replica lock acquisition after the first `n` in this process reports
/// [`LockBusy`], so a test can find the lock busy at an exact write.
fn busy_failpoint(lock: &Path) -> Result<()> {
    #[cfg(debug_assertions)]
    {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static TAKEN: AtomicUsize = AtomicUsize::new(0);
        let allowed = std::env::var("DEVKIT_TODO_TEST_LOCK_BUSY_AFTER")
            .ok()
            .and_then(|n| n.parse::<usize>().ok());
        if let Some(allowed) = allowed
            && TAKEN.fetch_add(1, Ordering::SeqCst) >= allowed
        {
            return Err(anyhow::Error::new(LockBusy {
                path: lock.to_path_buf(),
                wait: Duration::ZERO,
            })
            .context("todo store busy"));
        }
    }
    let _ = lock;
    Ok(())
}

/// Todos kept at bare project nodes in the taskchampion replica at `data_dir`.
pub struct TaskchampionStore {
    data_dir: PathBuf,
    lock_wait: Option<Duration>,
    target: Option<SyncTarget>,
    /// Set while [`TaskchampionStore::while_locked`] holds the replica lock.
    held: Cell<bool>,
    /// The target's server, built by the first sync and reused by the rest:
    /// building one loads the certificate store and derives the encryption
    /// key.
    server: Cell<Option<Box<dyn Server>>>,
}

impl TaskchampionStore {
    /// A replica with no sync target, whose callers wait as long as the
    /// replica lock is held.
    pub fn at(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            lock_wait: None,
            target: None,
            held: Cell::new(false),
            server: Cell::new(None),
        }
    }

    /// A write gives up with a "todo store busy" error once another process
    /// has held the replica lock for `wait`. Reads never take the lock.
    pub fn with_lock_wait(self, wait: Duration) -> Self {
        Self {
            lock_wait: Some(wait),
            ..self
        }
    }

    pub fn with_target(self, target: SyncTarget) -> Self {
        Self {
            target: Some(target),
            server: Cell::new(None),
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
        self.locked(async |replica| {
            let mut server = match self.server.take() {
                Some(server) => server,
                None => target.server().await?,
            };
            let synced = replica.sync(&mut server, false).await;
            self.server.set(Some(server));
            Ok(synced?)
        })
        .map_err(|e| match target {
            SyncTarget::Server { url, .. } => anyhow!(redacted(&format!("{e:#}"), url)),
            SyncTarget::Dir(_) => e,
        })
    }

    /// Runs `f` on the replica, opened under `<data_dir>/devkit.lock`.
    /// taskchampion holds a write transaction across a whole sync, longer than
    /// SQLite's busy timeout, so devkit's processes queue on this lock instead
    /// of failing on a busy database.
    fn locked<T>(&self, f: impl AsyncFnOnce(&mut Replica) -> Result<T>) -> Result<T> {
        let run = || block_on(async { f(&mut self.open().await?).await });
        match self.held.get() {
            true => run(),
            false => self.hold(run),
        }
    }

    /// Runs `f` with the replica lock held throughout, so every write `f`
    /// makes through this store lands under that one hold. A busy lock fails
    /// before `f` starts, never between two of its writes.
    pub fn while_locked<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        if self.held.get() {
            return f();
        }
        self.hold(|| {
            self.held.set(true);
            let out = f();
            self.held.set(false);
            out
        })
    }

    fn hold<T>(&self, run: impl FnOnce() -> Result<T>) -> Result<T> {
        std::fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("creating the todo replica at {}", self.data_dir.display()))?;
        let lock = self.data_dir.join("devkit.lock");
        busy_failpoint(&lock)?;
        match self.lock_wait {
            Some(wait) => {
                with_file_lock_for(&lock, wait, run).map_err(|e| match e.is::<LockBusy>() {
                    true => e.context("todo store busy"),
                    false => e,
                })
            }
            None => with_file_lock(&lock, run),
        }
    }

    /// Runs `f` on a read-only view of the replica, without the lock. SQLite's
    /// write-ahead log lets a reader see the last committed state while
    /// another process writes or syncs, so a read never waits behind a sync.
    /// A replica not created yet reads as `empty`.
    fn reading<T>(&self, empty: T, f: impl AsyncFnOnce(&mut Replica) -> Result<T>) -> Result<T> {
        // taskchampion's database file, the name taskwarrior also opens.
        if !self.data_dir.join("taskchampion.sqlite3").exists() {
            return Ok(empty);
        }
        block_on(async {
            let storage = SqliteStorage::new(&self.data_dir, AccessMode::ReadOnly, false)
                .await
                .with_context(|| {
                    format!("opening the todo replica at {}", self.data_dir.display())
                })?;
            f(&mut Replica::new(storage)).await
        })
    }

    async fn open(&self) -> Result<Replica> {
        let storage = SqliteStorage::new(&self.data_dir, AccessMode::ReadWrite, true)
            .await
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
        .into_todo()
    }

    async fn todos(&self, replica: &mut Replica) -> Result<Vec<Todo>> {
        Ok(replica
            .all_tasks()
            .await?
            .values()
            .filter_map(|task| self.todo_of(task))
            .collect())
    }

    async fn find(&self, replica: &mut Replica, id: &str) -> Result<Option<Todo>> {
        if !is_uuid_prefix(id) {
            return Ok(None);
        }
        if let Ok(uuid) = Uuid::try_parse(id) {
            let task = replica.get_task(uuid).await?;
            return Ok(task.and_then(|task| self.todo_of(&task)));
        }
        by_prefix(id, self.todos(replica).await?)
    }

    async fn resolve(&self, replica: &mut Replica, id: &str) -> Result<Todo> {
        self.find(replica, id)
            .await?
            .ok_or_else(|| anyhow!("no todo {id}"))
    }

    /// The full id of the parent `id` names, `None` for the top level.
    async fn resolve_parent(
        &self,
        replica: &mut Replica,
        id: Option<&str>,
    ) -> Result<Option<String>> {
        match id {
            Some(id) => Ok(Some(self.resolve(replica, id).await?.id)),
            None => Ok(None),
        }
    }

    /// The order that places a todo after the last of its siblings: the todos
    /// on `project` under `parent`, other than `skip`.
    async fn after_last(
        &self,
        replica: &mut Replica,
        project: Option<&str>,
        parent: Option<&str>,
        skip: Option<&str>,
    ) -> Result<i64> {
        let node = project.unwrap_or(devkit_todo::node::GLOBAL);
        Ok(self
            .todos(replica)
            .await?
            .iter()
            .filter(|t| t.node() == node)
            .filter(|t| t.parent.as_deref() == parent && Some(t.id.as_str()) != skip)
            .filter_map(|t| t.order)
            .max()
            .unwrap_or(0)
            + ORDER_GAP)
    }

    /// Applies `change` to the task behind `todo` and commits it.
    async fn change(
        replica: &mut Replica,
        todo: &Todo,
        change: impl FnOnce(&mut Task, &mut Operations) -> Result<(), taskchampion::Error>,
    ) -> Result<()> {
        let mut task = replica
            .get_task(Uuid::try_parse(&todo.id)?)
            .await?
            .ok_or_else(|| anyhow!("no todo {}", todo.id))?;
        let mut ops = Operations::new();
        change(&mut task, &mut ops)?;
        replica.commit_operations(ops).await?;
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
        Status::Completed { by } => close(task, taskchampion::Status::Completed, by.as_ref(), ops),
        Status::Cancelled { by } => close(task, taskchampion::Status::Deleted, by.as_ref(), ops),
    }
}

fn stop(task: &mut Task, ops: &mut Operations) -> Result<(), taskchampion::Error> {
    match task.is_active() {
        true => task.stop(ops),
        false => Ok(()),
    }
}

/// Ends `task` as `status`, completed or deleted, recording `by` when known.
fn close(
    task: &mut Task,
    status: taskchampion::Status,
    by: Option<&devkit_todo::Holder>,
    ops: &mut Operations,
) -> Result<(), taskchampion::Error> {
    stop(task, ops)?;
    task.set_status(status, ops)?;
    match by {
        Some(by) => task.set_value("holder", Some(by.to_string()), ops),
        None => Ok(()),
    }
}

impl TodoStore for TaskchampionStore {
    fn list(&self, filter: &Filter) -> Result<Vec<Todo>> {
        self.reading(Vec::new(), async |replica| {
            Ok(self
                .todos(replica)
                .await?
                .into_iter()
                .filter(|t| filter.matches(t.project.as_deref()))
                .collect())
        })
    }

    fn get(&self, id: &str) -> Result<Option<Todo>> {
        self.reading(None, async |replica| self.find(replica, id).await)
    }

    fn add(&self, todo: NewTodo) -> Result<String> {
        self.locked(async |replica| {
            let parent = self.resolve_parent(replica, todo.parent.as_deref()).await?;
            let order = match todo.order {
                Some(order) => order,
                None => {
                    self.after_last(replica, todo.project.as_deref(), parent.as_deref(), None)
                        .await?
                }
            };
            let uuid = Uuid::new_v4();
            let mut ops = Operations::new();
            let mut task = replica.create_task(uuid, &mut ops).await?;
            task.set_description(one_line(&todo.description), &mut ops)?;
            task.set_status(taskchampion::Status::Pending, &mut ops)?;
            task.set_entry(Some(Utc::now()), &mut ops)?;
            let project = project_of(todo.project.as_deref());
            task.set_value("project", Some(project), &mut ops)?;
            task.set_value("order", Some(order.to_string()), &mut ops)?;
            task.set_value("subof", parent, &mut ops)?;
            replica.commit_operations(ops).await?;
            Ok(uuid.to_string())
        })
    }

    fn apply(&self, edit: &Edit) -> Result<Vec<StatusChange>> {
        self.locked(async |replica| match edit {
            Edit::SetStatus { id, to, actor } => {
                let todo = self.resolve(replica, id).await?;
                match transition(&todo.status, *to, actor)? {
                    Some(next) => {
                        let change = StatusChange::of(&todo, Some(next.clone()));
                        Self::change(replica, &todo, |task, ops| write_status(task, &next, ops))
                            .await?;
                        Ok(vec![change])
                    }
                    None => Ok(Vec::new()),
                }
            }
            Edit::Describe { id, description } => {
                let todo = self.resolve(replica, id).await?;
                Self::change(replica, &todo, |task, ops| {
                    task.set_description(one_line(description), ops)
                })
                .await?;
                Ok(Vec::new())
            }
            Edit::Move { id, parent, order } => {
                let todo = self.resolve(replica, id).await?;
                let parent = self.resolve_parent(replica, parent.as_deref()).await?;
                let order = match order {
                    Some(order) => *order,
                    None => {
                        self.after_last(
                            replica,
                            todo.project.as_deref(),
                            parent.as_deref(),
                            Some(&todo.id),
                        )
                        .await?
                    }
                };
                Self::change(replica, &todo, |task, ops| {
                    task.set_value("subof", parent, ops)?;
                    task.set_value("order", Some(order.to_string()), ops)
                })
                .await?;
                Ok(Vec::new())
            }
            Edit::Reorder { id, order } => {
                let todo = self.resolve(replica, id).await?;
                Self::change(replica, &todo, |task, ops| {
                    task.set_value("order", Some(order.to_string()), ops)
                })
                .await?;
                Ok(Vec::new())
            }
            Edit::Relocate { id, project } => {
                let todo = self.resolve(replica, id).await?;
                let project = project_of(project.as_deref());
                Self::change(replica, &todo, |task, ops| {
                    task.set_value("project", Some(project), ops)
                })
                .await?;
                Ok(Vec::new())
            }
            Edit::ReleaseAll { holder } => {
                if holder.is_human() {
                    return Ok(Vec::new());
                }
                let mut ops = Operations::new();
                let mut changes = Vec::new();
                for task in replica.all_tasks().await?.values_mut() {
                    let Some(todo) = self.todo_of(task) else {
                        continue;
                    };
                    if matches!(&todo.status, Status::InProgress { by } if holder.covers(by)) {
                        task.stop(&mut ops)?;
                        task.set_value("holder", None, &mut ops)?;
                        changes.push(StatusChange::of(&todo, Some(Status::Pending)));
                    }
                }
                replica.commit_operations(ops).await?;
                Ok(changes)
            }
            Edit::Purge(id) => {
                let todo = self.resolve(replica, id).await?;
                let mut data = replica
                    .get_task_data(Uuid::try_parse(&todo.id)?)
                    .await?
                    .ok_or_else(|| anyhow!("no todo {id}"))?;
                let mut ops = Operations::new();
                data.delete(&mut ops);
                replica.commit_operations(ops).await?;
                Ok(vec![StatusChange::of(&todo, None)])
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::redacted;

    #[test]
    fn a_urls_user_and_password_are_redacted() {
        let url = "http://alice:hunter2@sync.example/";
        let text = "GET http://alice:hunter2@sync.example/v1: refused; hunter2";
        let out = redacted(text, url);
        assert!(!out.contains("hunter2") && !out.contains("alice:"), "{out}");
        assert_eq!(redacted("plain", "https://sync.example/"), "plain");
    }
}
