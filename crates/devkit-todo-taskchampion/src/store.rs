use std::{
    cell::Cell,
    collections::HashMap,
    fmt,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use devkit_common::store::{LockBusy, with_file_lock, with_file_lock_for};
use devkit_todo::{
    ClosedCounts, Edit, Filter, NewTodo, ORDER_GAP, Status, StatusChange, Todo, TodoStore,
    by_prefix, is_uuid_prefix, one_line, take_over, ticket_todo_uuid, transition,
};
use taskchampion::{
    Operations, Server, ServerConfig, SqliteStorage, TaskData, Uuid, chrono::Utc,
    storage::AccessMode, utc_timestamp,
};
use tokio::runtime::Runtime;

use crate::schema::{Exported, node_of, project_of};

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

    /// The replica's database opened read-only for direct SQL, `None` for a
    /// replica not created yet. taskchampion answers a read only with whole
    /// decoded tasks, so counting closed tasks needs this. The queries read
    /// its private `tasks` table of `uuid` and JSON `data`, and a test pins
    /// them to what taskchampion's own reads return.
    fn database(&self) -> Result<Option<rusqlite::Connection>> {
        let path = self.data_dir.join("taskchampion.sqlite3");
        if !path.exists() {
            return Ok(None);
        }
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map(Some)
            .with_context(|| format!("opening the todo replica at {}", self.data_dir.display()))
    }

    /// Every open todo, read without decoding a closed task. A row whose data
    /// is not JSON, which taskchampion cannot read either, is skipped.
    fn open_todos(&self) -> Result<Vec<Todo>> {
        let Some(db) = self.database()? else {
            return Ok(Vec::new());
        };
        let mut query = db.prepare(
            "SELECT uuid, data FROM tasks WHERE CASE WHEN json_valid(data) THEN \
             json_extract(data, '$.status') = 'pending' END",
        )?;
        let rows = query.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut todos = Vec::new();
        for row in rows {
            let (uuid, data) = row?;
            let props: HashMap<String, String> = serde_json::from_str(&data)
                .with_context(|| format!("reading todo {uuid} in the replica"))?;
            let value = |key: &str| props.get(key).map(String::as_str);
            todos.extend(Self::todo_from(
                Uuid::try_parse(&uuid)?,
                props.keys(),
                value,
            ));
        }
        Ok(todos)
    }

    async fn open(&self) -> Result<Replica> {
        let storage = SqliteStorage::new(&self.data_dir, AccessMode::ReadWrite, true)
            .await
            .with_context(|| format!("opening the todo replica at {}", self.data_dir.display()))?;
        Ok(Replica::new(storage))
    }

    /// The todo `task` represents. It reads the stored properties alone,
    /// never the replica's dependency map, which costs one storage round
    /// trip per working-set entry.
    fn todo_of(task: &TaskData) -> Option<Todo> {
        Self::todo_from(task.get_uuid(), task.properties(), |key| task.get(key))
    }

    fn todo_from<'a>(
        uuid: Uuid,
        properties: impl Iterator<Item = &'a String>,
        get: impl Fn(&str) -> Option<&'a str>,
    ) -> Option<Todo> {
        let value = |key: &str| get(key).map(str::to_string);
        let stamp = |key: &str| {
            get(key)
                .and_then(|t| t.parse::<i64>().ok())
                .map(|t| utc_timestamp(t).format("%Y%m%dT%H%M%SZ").to_string())
        };
        Exported {
            uuid: uuid.to_string(),
            description: value("description").unwrap_or_default(),
            status: value("status").unwrap_or_default(),
            start: value("start"),
            holder: value("holder"),
            subof: value("subof"),
            order: get("order")
                .and_then(|o| o.parse::<f64>().ok())
                .map(|o| o.round() as i64),
            project: value("project"),
            entry: stamp("entry"),
            modified: stamp("modified"),
            depends: properties
                .filter_map(|p| p.strip_prefix("dep_"))
                .filter_map(|dep| Uuid::try_parse(dep).ok())
                .map(|uuid| uuid.to_string())
                .collect(),
            ticket_tracker: value("ticket_tracker"),
            ticket_key: value("ticket_key"),
            ticket_status: value("ticket_status"),
        }
        .into_todo()
    }

    async fn todos(&self, replica: &mut Replica) -> Result<Vec<Todo>> {
        Ok(replica
            .all_task_data()
            .await?
            .values()
            .filter_map(Self::todo_of)
            .collect())
    }

    /// The todo `id` names, by its full uuid or a unique prefix of one. A
    /// prefix reads the replica's uuids, and only its matches' tasks.
    async fn find(&self, replica: &mut Replica, id: &str) -> Result<Option<Todo>> {
        if !is_uuid_prefix(id) {
            return Ok(None);
        }
        if let Ok(uuid) = Uuid::try_parse(id) {
            let task = replica.get_task_data(uuid).await?;
            return Ok(task.as_ref().and_then(Self::todo_of));
        }
        let uuids = replica.all_task_uuids().await?;
        let mut todos = Vec::new();
        for uuid in uuids.into_iter().filter(|u| u.to_string().starts_with(id)) {
            if let Some(task) = replica.get_task_data(uuid).await? {
                todos.extend(Self::todo_of(&task));
            }
        }
        by_prefix(id, todos)
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

    /// Adds each blocker in `by` to `id` when `add`, and removes them
    /// otherwise, as one `dep_<uuid>` property apiece. A blocker that does
    /// not parse as a uuid, or that is already recorded or already absent,
    /// changes nothing.
    async fn set_dependencies(
        &self,
        replica: &mut Replica,
        id: &str,
        by: &[String],
        add: bool,
    ) -> Result<()> {
        let todo = self.resolve(replica, id).await?;
        let mut deps = Vec::new();
        for blocker in by {
            let Ok(uuid) = Uuid::try_parse(blocker) else {
                continue;
            };
            deps.push(uuid);
        }
        if deps.is_empty() {
            return Ok(());
        }
        let deps: Vec<Uuid> = deps
            .into_iter()
            .filter(|dep| todo.blocked_by.contains(&dep.to_string()) != add)
            .collect();
        if deps.is_empty() {
            return Ok(());
        }
        Self::change(replica, &todo, |task| {
            for dep in deps {
                task.set(format!("dep_{dep}"), add.then(String::new));
            }
        })
        .await
    }

    /// Applies `change` to the task behind `todo` and commits it.
    async fn change(
        replica: &mut Replica,
        todo: &Todo,
        change: impl FnOnce(&mut Writer),
    ) -> Result<()> {
        let mut task = replica
            .get_task_data(Uuid::try_parse(&todo.id)?)
            .await?
            .ok_or_else(|| anyhow!("no todo {}", todo.id))?;
        let mut ops = Operations::new();
        change(&mut Writer::new(&mut task, &mut ops));
        replica.commit_operations(ops).await?;
        Ok(())
    }

    /// Moves the task behind `todo` to `next`, when there is one, and
    /// returns the change.
    async fn move_to(
        replica: &mut Replica,
        todo: &Todo,
        next: Option<Status>,
    ) -> Result<Vec<StatusChange>> {
        let Some(next) = next else {
            return Ok(Vec::new());
        };
        let change = StatusChange::of(todo, Some(next.clone()));
        Self::change(replica, todo, |task| write_status(task, &next)).await?;
        Ok(vec![change])
    }
}

/// The task statuses devkit writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskStatus {
    Pending,
    Completed,
    Deleted,
}

impl TaskStatus {
    /// The value the task data stores, as taskwarrior writes it.
    fn stored(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Deleted => "deleted",
        }
    }
}

/// One task's writes, made to its data as taskchampion's `Task` setters
/// make them: the first write also stamps `modified`. The replica builds a
/// `Task` only with a dependency map, read from every working-set entry.
struct Writer<'a> {
    task: &'a mut TaskData,
    ops: &'a mut Operations,
    stamped: bool,
}

impl<'a> Writer<'a> {
    fn new(task: &'a mut TaskData, ops: &'a mut Operations) -> Self {
        Self {
            task,
            ops,
            stamped: false,
        }
    }

    fn now() -> String {
        Utc::now().timestamp().to_string()
    }

    fn set(&mut self, key: impl Into<String>, value: Option<String>) {
        if !self.stamped {
            self.stamped = true;
            self.task.update("modified", Some(Self::now()), self.ops);
        }
        self.task.update(key, value, self.ops);
    }

    /// Sets `status` with `end` in step: a completed or deleted task has an
    /// end time, and a pending task has none.
    fn status(&mut self, status: TaskStatus) {
        let closed = status != TaskStatus::Pending;
        if closed != self.task.has("end") {
            self.set("end", closed.then(Self::now));
        }
        self.set("status", Some(status.stored().to_string()));
    }

    /// Starts the task, keeping a start time it already has, so a claim
    /// handed down to a sub-agent keeps its start.
    fn start(&mut self) {
        if !self.task.has("start") {
            self.set("start", Some(Self::now()));
        }
    }

    fn stop(&mut self) {
        if self.task.has("start") {
            self.set("start", None);
        }
    }
}

/// Writes `status` the way taskwarrior's `start`, `done` and `delete` do.
fn write_status(task: &mut Writer, status: &Status) {
    let holder = |by: &devkit_todo::Holder| Some(by.to_string());
    match status {
        Status::Pending => release(task),
        Status::InProgress { by } => {
            task.status(TaskStatus::Pending);
            task.start();
            task.set("holder", holder(by));
        }
        Status::Completed { by } => close(task, TaskStatus::Completed, by.as_ref()),
        Status::Cancelled { by } => close(task, TaskStatus::Deleted, by.as_ref()),
    }
}

/// Leaves `task` pending, with no start time and no holder.
fn release(task: &mut Writer) {
    task.status(TaskStatus::Pending);
    task.stop();
    task.set("holder", None);
}

/// Ends `task` as `status`, completed or deleted, recording `by` when known.
fn close(task: &mut Writer, status: TaskStatus, by: Option<&devkit_todo::Holder>) {
    task.stop();
    task.status(status);
    if let Some(by) = by {
        task.set("holder", Some(by.to_string()));
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

    fn list_open(&self, filter: &Filter) -> Result<Vec<Todo>> {
        let mut todos = self.open_todos()?;
        todos.retain(|t| filter.matches(t.project.as_deref()));
        Ok(todos)
    }

    /// Counted by SQLite over the stored status and project, so no closed
    /// task is decoded. A project that names no node holds no todo.
    fn closed_counts(&self, filter: &Filter) -> Result<ClosedCounts> {
        let Some(db) = self.database()? else {
            return Ok(ClosedCounts::new());
        };
        let mut query = db.prepare(
            "SELECT json_extract(data, '$.project'), json_extract(data, '$.status'), count(*) \
             FROM tasks WHERE CASE WHEN json_valid(data) THEN \
             json_extract(data, '$.status') IN ('completed', 'deleted') END GROUP BY 1, 2",
        )?;
        let rows = query.query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, usize>(2)?,
            ))
        })?;
        let mut counts = ClosedCounts::new();
        for row in rows {
            let (project, status, count) = row?;
            let Some(project) = project.filter(|p| node_of(p).is_some()) else {
                continue;
            };
            if !filter.matches(node_of(&project).flatten().as_deref()) {
                continue;
            }
            let at = counts.entry(project).or_default();
            match status.as_str() {
                "completed" => at.completed += count,
                _ => at.cancelled += count,
            }
        }
        Ok(counts)
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
            let uuid = match &todo.ticket {
                Some(ticket) => {
                    let uuid = Uuid::from_bytes(
                        ticket_todo_uuid("", &ticket.tracker, &ticket.key).into_bytes(),
                    );
                    if replica.get_task_data(uuid).await?.is_some() {
                        return Ok(uuid.to_string());
                    }
                    uuid
                }
                None => Uuid::new_v4(),
            };
            let mut ops = Operations::new();
            let mut data = TaskData::create(uuid, &mut ops);
            let mut task = Writer::new(&mut data, &mut ops);
            task.set("description", Some(one_line(&todo.description)));
            task.status(TaskStatus::Pending);
            task.set("entry", Some(Writer::now()));
            task.set("project", Some(project_of(todo.project.as_deref())));
            task.set("order", Some(order.to_string()));
            task.set("subof", parent);
            if let Some(ticket) = todo.ticket {
                task.set("ticket_tracker", Some(ticket.tracker));
                task.set("ticket_key", Some(ticket.key));
                task.set("ticket_status", ticket.synced_status);
            }
            replica.commit_operations(ops).await?;
            Ok(uuid.to_string())
        })
    }

    fn apply(&self, edit: &Edit) -> Result<Vec<StatusChange>> {
        self.locked(async |replica| match edit {
            Edit::SetStatus { id, to, actor } => {
                let todo = self.resolve(replica, id).await?;
                let next = transition(&todo.status, *to, actor)?;
                Self::move_to(replica, &todo, next).await
            }
            Edit::TakeOver { id, from, actor } => {
                let todo = self.resolve(replica, id).await?;
                let next = take_over(&todo.status, from, actor)?;
                Self::move_to(replica, &todo, next).await
            }
            Edit::Describe { id, description } => {
                let todo = self.resolve(replica, id).await?;
                Self::change(replica, &todo, |task| {
                    task.set("description", Some(one_line(description)));
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
                Self::change(replica, &todo, |task| {
                    task.set("subof", parent);
                    task.set("order", Some(order.to_string()));
                })
                .await?;
                Ok(Vec::new())
            }
            Edit::Reorder { id, order } => {
                let todo = self.resolve(replica, id).await?;
                Self::change(replica, &todo, |task| {
                    task.set("order", Some(order.to_string()));
                })
                .await?;
                Ok(Vec::new())
            }
            Edit::Relocate { id, project } => {
                let todo = self.resolve(replica, id).await?;
                let project = project_of(project.as_deref());
                Self::change(replica, &todo, |task| {
                    task.set("project", Some(project));
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
                for todo in self.open_todos()? {
                    if !matches!(&todo.status, Status::InProgress { by } if holder.covers(by)) {
                        continue;
                    }
                    let mut task = replica
                        .get_task_data(Uuid::try_parse(&todo.id)?)
                        .await?
                        .ok_or_else(|| anyhow!("no todo {}", todo.id))?;
                    release(&mut Writer::new(&mut task, &mut ops));
                    changes.push(StatusChange::of(&todo, Some(Status::Pending)));
                }
                replica.commit_operations(ops).await?;
                Ok(changes)
            }
            Edit::Block { id, by } => {
                self.set_dependencies(replica, id, by, true).await?;
                Ok(Vec::new())
            }
            Edit::Unblock { id, by } => {
                self.set_dependencies(replica, id, by, false).await?;
                Ok(Vec::new())
            }
            Edit::RecordSync { id, synced_status } => {
                let todo = self.resolve(replica, id).await?;
                if todo.ticket.is_none() {
                    anyhow::bail!("todo {id} has no ticket");
                }
                Self::change(replica, &todo, |task| {
                    task.set("ticket_status", Some(synced_status.clone()));
                })
                .await?;
                Ok(Vec::new())
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
    use std::collections::BTreeMap;

    use taskchampion::{Operation, Task};

    use super::*;

    /// One write devkit makes to a task.
    #[derive(Clone, Copy, Debug)]
    enum Step {
        Status(TaskStatus),
        Start,
        Stop,
        Set(&'static str, Option<&'static str>),
        Depend(bool),
    }

    const BLOCKER: Uuid = Uuid::from_u128(7);

    /// `steps` through taskchampion's `Task` setters, guarded as devkit
    /// guarded them: a stop only on an active task.
    fn through_task(task: &mut Task, steps: &[Step], ops: &mut Operations) {
        for step in steps {
            match *step {
                Step::Status(status) => task.set_status(
                    match status {
                        TaskStatus::Pending => taskchampion::Status::Pending,
                        TaskStatus::Completed => taskchampion::Status::Completed,
                        TaskStatus::Deleted => taskchampion::Status::Deleted,
                    },
                    ops,
                ),
                Step::Start => task.start(ops),
                Step::Stop if task.is_active() => task.stop(ops),
                Step::Stop => Ok(()),
                Step::Set(key, value) => task.set_value(key, value.map(str::to_string), ops),
                Step::Depend(true) => task.add_dependency(BLOCKER, ops),
                Step::Depend(false) => task.remove_dependency(BLOCKER, ops),
            }
            .unwrap();
        }
    }

    fn through_writer(task: &mut Writer, steps: &[Step]) {
        for step in steps {
            match *step {
                Step::Status(status) => task.status(status),
                Step::Start => task.start(),
                Step::Stop => task.stop(),
                Step::Set(key, value) => task.set(key, value.map(str::to_string)),
                Step::Depend(add) => task.set(format!("dep_{BLOCKER}"), add.then(String::new)),
            }
        }
    }

    /// `value` with a timestamp taken during the test read as `<now>`, so
    /// two writers a second apart compare equal and a fixed one does not.
    fn settled(value: &Option<String>, since: i64) -> Option<String> {
        value.as_ref().map(|v| match v.parse::<i64>() {
            Ok(t) if t >= since => "<now>".to_string(),
            _ => v.clone(),
        })
    }

    type Write = (String, Option<String>, Option<String>);

    fn writes(ops: &Operations, since: i64) -> Vec<Write> {
        ops.iter()
            .map(|op| match op {
                Operation::Update {
                    property,
                    old_value,
                    value,
                    ..
                } => (
                    property.clone(),
                    settled(old_value, since),
                    settled(value, since),
                ),
                other => panic!("unexpected operation {other:?}"),
            })
            .collect()
    }

    fn properties(task: &TaskData, since: i64) -> BTreeMap<String, Option<String>> {
        task.iter()
            .map(|(k, v)| (k.clone(), settled(&Some(v.clone()), since)))
            .collect()
    }

    #[test]
    fn the_writer_writes_as_the_task_setters_do() {
        let dir = tempfile::tempdir().unwrap();
        let since = Utc::now().timestamp();
        let statuses = [
            taskchampion::Status::Pending,
            taskchampion::Status::Completed,
            taskchampion::Status::Deleted,
        ];
        let scenarios: &[&[Step]] = &[
            &[Step::Status(TaskStatus::Pending)],
            &[Step::Status(TaskStatus::Completed)],
            &[Step::Status(TaskStatus::Deleted)],
            &[Step::Start],
            &[Step::Stop],
            &[Step::Set("description", Some("renamed"))],
            &[Step::Set("subof", None)],
            &[Step::Depend(true)],
            &[Step::Depend(false)],
            &[
                Step::Status(TaskStatus::Pending),
                Step::Stop,
                Step::Set("holder", None),
            ],
            &[
                Step::Status(TaskStatus::Pending),
                Step::Start,
                Step::Set("holder", Some("s1")),
            ],
            &[
                Step::Stop,
                Step::Status(TaskStatus::Completed),
                Step::Set("holder", Some("s1")),
            ],
        ];
        block_on(async {
            let storage = SqliteStorage::new(dir.path(), AccessMode::ReadWrite, true).await?;
            let mut replica = Replica::new(storage);
            let mut seeded = Vec::new();
            for status in &statuses {
                for start in [None, Some("1700000000")] {
                    for end in [None, Some("1700000100")] {
                        let uuid = Uuid::new_v4();
                        let mut ops = Operations::new();
                        let mut task = TaskData::create(uuid, &mut ops);
                        task.update("description", Some("d".into()), &mut ops);
                        task.update("status", Some(status.to_string().to_lowercase()), &mut ops);
                        task.update("start", start.map(str::to_string), &mut ops);
                        task.update("end", end.map(str::to_string), &mut ops);
                        task.update(format!("dep_{BLOCKER}"), Some(String::new()), &mut ops);
                        replica.commit_operations(ops).await?;
                        seeded.push((uuid, status.clone(), start, end));
                    }
                }
            }
            for (uuid, status, start, end) in seeded {
                for steps in scenarios {
                    let label = format!("{status:?} start={start:?} end={end:?} {steps:?}");
                    let mut task = replica.get_task(uuid).await?.unwrap();
                    let mut task_ops = Operations::new();
                    through_task(&mut task, steps, &mut task_ops);
                    let mut data = replica.get_task_data(uuid).await?.unwrap();
                    let mut writer_ops = Operations::new();
                    through_writer(&mut Writer::new(&mut data, &mut writer_ops), steps);
                    assert_eq!(
                        writes(&writer_ops, since),
                        writes(&task_ops, since),
                        "{label}"
                    );
                    assert_eq!(
                        properties(&data, since),
                        properties(&task.into_task_data(), since),
                        "{label}"
                    );
                }
            }
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn a_urls_user_and_password_are_redacted() {
        let url = "http://alice:hunter2@sync.example/";
        let text = "GET http://alice:hunter2@sync.example/v1: refused; hunter2";
        let out = redacted(text, url);
        assert!(!out.contains("hunter2") && !out.contains("alice:"), "{out}");
        assert_eq!(redacted("plain", "https://sync.example/"), "plain");
    }
}
