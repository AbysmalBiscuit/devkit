//! The one place that names every todo backend.

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use ambassador::Delegate;
use anyhow::{Result, bail};
use devkit_common::{
    secrets::{self, DopplerScope, Source},
    vcs::Checkout,
};
use devkit_config::{
    NoConfig, PostgresConfig, TaskchampionConfig, TodoBackend, TodoConfig, expand_tilde,
};
use devkit_todo::{
    TodoStore,
    activity::{ActivityLog, ActivityStore, Recorded, ambassador_impl_ActivityStore},
    ambassador_impl_TodoStore,
};
use devkit_todo_builtin::BuiltinStore;
use devkit_todo_postgres::{Database, PostgresActivity, PostgresStore};
use devkit_todo_taskchampion::{SyncTarget, TaskchampionStore, Uuid};
use devkit_todo_taskwarrior::TaskwarriorStore;
use serde::{Deserialize, de::IntoDeserializer};

use super::sync::SyncOutcome;

/// Overrides `[todo] backend`, so a container can pick its store while the
/// project's own config still loads.
pub(crate) const BACKEND_VAR: &str = "DEVKIT_TODO_BACKEND";

type SyncVars = [&'static str; 3];

/// The server credentials, in the order [`SyncTarget::Server`] takes them.
pub(crate) const SYNC_VARS: SyncVars = [
    "DEVKIT_TODO_SYNC_URL",
    "DEVKIT_TODO_SYNC_CLIENT_ID",
    "DEVKIT_TODO_SYNC_SECRET",
];

/// The Postgres backend's connection URL.
pub(crate) const DATABASE_VAR: &str = "DEVKIT_TODO_DATABASE_URL";

/// How long a hook waits for the taskchampion replica's lock before it
/// queues its write instead, and for each answer from the todo database,
/// connecting included, before it gives up.
const HOOK_WAIT: Duration = Duration::from_secs(1);

/// All the time the session-end hook gives its todo database work, the
/// release and the activity records together: the harness gives that hook
/// two seconds in all, and process start and the other releases need the
/// rest.
pub(crate) const SESSION_END_DATABASE_BUDGET: Duration = Duration::from_millis(1500);

/// Set once a hook has one budget for all of its database work.
static DATABASE_DEADLINE: OnceLock<Instant> = OnceLock::new();

/// Makes every todo database this process opens, before or after, give up
/// by `budget` from now.
pub(crate) fn finish_database_work_within(budget: Duration) {
    let _ = DATABASE_DEADLINE.set(Instant::now() + budget);
}

/// How long a CLI call waits for each answer from the todo database,
/// connecting included.
const CLI_DATABASE_WAIT: Duration = Duration::from_secs(15);

/// How long a CLI write waits for the taskchampion replica's lock: longer
/// than one sync attempt against a server that stops answering (a 10 s
/// connect and a 60 s read), so a write outlasts a stuck sync and fails only
/// behind something stuck for good.
pub(crate) const CLI_LOCK_WAIT: Duration = Duration::from_secs(75);

/// The store `[todo] backend` names, its claim changes recorded in the
/// activity log.
#[derive(Delegate)]
#[delegate(TodoStore)]
pub(crate) struct Store(Recorded<Backend, Activity>);

#[derive(Delegate)]
#[delegate(TodoStore)]
pub(crate) enum Backend {
    Builtin(BuiltinStore),
    Taskwarrior(TaskwarriorStore),
    Taskchampion(Replica),
    Postgres(PostgresStore),
}

/// Where the activity log is kept: in the todo database on the Postgres
/// backend, so a swarm reports from one place, and under devkit's state
/// directory on every other.
#[derive(Delegate)]
#[delegate(ActivityStore)]
pub(crate) enum Activity {
    Local(ActivityLog),
    Postgres(PostgresActivity),
}

/// Who opens a store, which sets how long it waits on a busy replica lock
/// or a slow database.
#[derive(Clone, Copy)]
enum Opener {
    Cli,
    Hook,
}

impl Opener {
    fn lock_wait(self) -> Duration {
        match self {
            Self::Cli => CLI_LOCK_WAIT,
            Self::Hook => HOOK_WAIT,
        }
    }

    fn database_wait(self) -> Duration {
        match self {
            Self::Cli => CLI_DATABASE_WAIT,
            Self::Hook => HOOK_WAIT,
        }
    }
}

/// The taskchampion replica, and whether its config points it at a sync
/// target. Only `devkit todo sync` resolves the target, credentials and all,
/// so no other caller waits on Doppler.
#[derive(Delegate)]
#[delegate(TodoStore, target = "store")]
pub(crate) struct Replica {
    store: TaskchampionStore,
    syncs: bool,
}

/// Whether `config` and the environment name any sync target, judged without
/// resolving a credential: a sync directory, a Doppler project, or any sync
/// variable in the environment or the secrets file.
fn names_a_target(config: &TaskchampionConfig) -> bool {
    config.server_dir.is_some()
        || config.doppler_project.is_some()
        || SYNC_VARS
            .iter()
            .any(|v| secrets::source(v) != Source::Unset)
}

/// Where the effective backend came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackendSource {
    Env,
    Config,
    Default,
}

/// `DEVKIT_TODO_BACKEND` over `[todo] backend`. An unknown value is an error
/// naming the variable and the accepted spellings.
pub(crate) fn effective_backend(
    config: Option<&TodoConfig>,
    env: Option<&str>,
) -> Result<(TodoBackend, BackendSource)> {
    if let Some(name) = env.map(str::trim).filter(|v| !v.is_empty()) {
        return match TodoBackend::deserialize(name.into_deserializer()) {
            Ok(backend) => Ok((backend, BackendSource::Env)),
            Err(e) => {
                let e: serde::de::value::Error = e;
                bail!("{BACKEND_VAR}: {e}")
            }
        };
    }
    Ok(match config.map(|c| c.backend) {
        Some(backend) if backend != TodoBackend::default() => (backend, BackendSource::Config),
        _ => (TodoBackend::default(), BackendSource::Default),
    })
}

/// The Doppler scope a backend's `doppler_project` and `doppler_config`
/// name, `None` without a project.
pub(crate) fn doppler_scope(project: Option<&str>, config: Option<&str>) -> Option<DopplerScope> {
    project.map(|project| DopplerScope {
        project: project.to_string(),
        config: config.map(str::to_string),
    })
}

/// The todo database `config` and [`DATABASE_VAR`] name, opened with `wait`
/// and not yet connected, and where its URL resolved from. A URL that is
/// missing or does not parse is an error naming the variable, never the URL.
pub(crate) fn open_database(
    config: &PostgresConfig,
    wait: Duration,
) -> (Result<Arc<Database>, String>, Source) {
    let [(url, source)] = secrets::resolve_many(
        &[DATABASE_VAR],
        doppler_scope(
            config.doppler_project.as_deref(),
            config.doppler_config.as_deref(),
        )
        .as_ref(),
    );
    let db = match url {
        None => Err(format!("{DATABASE_VAR} is not set")),
        Some(url) => Database::new(&url, wait).map_err(|e| format!("{DATABASE_VAR}: {e:#}")),
    };
    (db, source)
}

/// [`open_database`], resolved and connected once per config in a process,
/// so a hook asks Doppler at most once and waits on an unreachable database
/// once. A URL that does not resolve gives a database every call on fails.
pub(crate) fn database(config: &PostgresConfig, wait: Duration) -> Arc<Database> {
    type Key = (Option<String>, Option<String>, Duration);
    static OPEN: Mutex<Option<HashMap<Key, Arc<Database>>>> = Mutex::new(None);
    let key = (
        config.doppler_project.clone(),
        config.doppler_config.clone(),
        wait,
    );
    let mut open = OPEN.lock().unwrap_or_else(|e| e.into_inner());
    let db = open
        .get_or_insert_default()
        .entry(key)
        .or_insert_with(|| {
            open_database(config, wait)
                .0
                .unwrap_or_else(Database::unusable)
        })
        .clone();
    if let Some(at) = DATABASE_DEADLINE.get() {
        db.finish_by(*at);
    }
    db
}

/// The replica's sync target: `server_dir` when set, without reading any
/// credential; a server when all of [`SYNC_VARS`] resolve; none when none
/// do. Some but not all resolving, or a client id that is not a UUID, is an
/// error naming the variables and never their values.
pub(crate) fn sync_target(config: &TaskchampionConfig) -> Result<Option<SyncTarget>> {
    sync_target_with(config, secrets::resolve_many)
}

fn sync_target_with(
    config: &TaskchampionConfig,
    resolve: impl FnOnce(
        &SyncVars,
        Option<&DopplerScope>,
    ) -> [(Option<String>, Source); SYNC_VARS.len()],
) -> Result<Option<SyncTarget>> {
    if let Some(dir) = &config.server_dir {
        return Ok(Some(SyncTarget::Dir(expand_tilde(dir))));
    }
    let scope = doppler_scope(
        config.doppler_project.as_deref(),
        config.doppler_config.as_deref(),
    );
    let resolved = resolve(&SYNC_VARS, scope.as_ref());
    let missing: Vec<&str> = SYNC_VARS
        .iter()
        .zip(&resolved)
        .filter(|(_, (value, _))| value.is_none())
        .map(|(name, _)| *name)
        .collect();
    let [(Some(url), _), (Some(client_id), _), (Some(secret), _)] = resolved else {
        if missing.len() == SYNC_VARS.len() {
            return Ok(None);
        }
        bail!(
            "todo sync needs {}; missing: {}",
            SYNC_VARS.join(", "),
            missing.join(", ")
        );
    };
    let Ok(client_id) = Uuid::try_parse(client_id.trim()) else {
        bail!("{} is not a UUID", SYNC_VARS[1]);
    };
    Ok(Some(SyncTarget::Server {
        url,
        client_id,
        secret: secret.into_bytes(),
    }))
}

/// Where the taskchampion replica lives.
fn data_dir(config: &TaskchampionConfig) -> std::path::PathBuf {
    match &config.data_dir {
        Some(dir) => expand_tilde(dir),
        None => devkit_todo::state_dir().join("taskchampion"),
    }
}

impl Backend {
    fn recorded(self) -> Store {
        Store(Recorded::new(self, Activity::Local(ActivityLog::open())))
    }
}

impl Activity {
    /// The log that goes with the store `config` names.
    fn of(config: &TodoConfig, opener: Opener) -> Self {
        match config.backend {
            TodoBackend::Postgres => Self::Postgres(PostgresActivity::new(
                database(&config.postgres, opener.database_wait()),
                &config.project,
            )),
            TodoBackend::Builtin | TodoBackend::Taskwarrior | TodoBackend::Taskchampion => {
                Self::Local(ActivityLog::open())
            }
        }
    }
}

fn taskchampion(config: &TodoConfig) -> TaskchampionStore {
    TaskchampionStore::at(data_dir(&config.taskchampion)).with_root(&config.project)
}

impl Store {
    /// The store `config` names, its claim changes recorded in the activity
    /// log that goes with it.
    fn open(config: &TodoConfig, opener: Opener) -> Self {
        let backend = match config.backend {
            TodoBackend::Builtin => Backend::Builtin(BuiltinStore::open()),
            TodoBackend::Taskwarrior => Backend::Taskwarrior(
                TaskwarriorStore::new(&config.taskwarrior.path).with_root(&config.project),
            ),
            TodoBackend::Taskchampion => Backend::Taskchampion(Replica {
                store: taskchampion(config).with_lock_wait(opener.lock_wait()),
                syncs: names_a_target(&config.taskchampion),
            }),
            TodoBackend::Postgres => Backend::Postgres(PostgresStore::new(
                database(&config.postgres, opener.database_wait()),
                &config.project,
            )),
        };
        Self(Recorded::new(backend, Activity::of(config, opener)))
    }

    fn backend(&self) -> &Backend {
        self.0.inner()
    }

    /// The taskchampion replica this store syncs: `None` for another backend
    /// or a replica with no sync target.
    fn synced_replica(&self) -> Option<&TaskchampionStore> {
        match self.backend() {
            Backend::Taskchampion(Replica { store, syncs: true }) => Some(store),
            _ => None,
        }
    }

    /// After a write: starts a background sync from `cwd` and returns at once.
    pub(crate) fn spawn_sync(&self, cwd: &Path) {
        if let Some(replica) = self.synced_replica() {
            super::sync::spawn(replica, cwd);
        }
    }

    /// Syncs from `cwd`, waiting up to `wait` for it to finish.
    pub(crate) fn sync(&self, cwd: &Path, wait: Duration) -> SyncOutcome {
        match self.synced_replica() {
            Some(replica) => super::sync::wait_for(replica, cwd, wait),
            None => SyncOutcome::NoTarget,
        }
    }

    /// The config a CLI call in `cwd` reads: `None` when there is none
    /// anywhere, as in a cloud session.
    fn cli_config(cwd: &Path) -> Result<Option<TodoConfig>> {
        match devkit_common::config::resolve(None, cwd) {
            Ok((config, _)) => Ok(Some(config.todo)),
            Err(e) if e.downcast_ref::<NoConfig>().is_some() => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The todo config a CLI call in `cwd` reads, its backend the effective
    /// one. No config anywhere is the default config; a config that fails to
    /// load or an unknown `DEVKIT_TODO_BACKEND` is an error.
    fn effective_cli_config(cwd: &Path) -> Result<TodoConfig> {
        let config = Self::cli_config(cwd)?;
        let env = std::env::var(BACKEND_VAR).ok();
        let (backend, _) = effective_backend(config.as_ref(), env.as_deref())?;
        Ok(TodoConfig {
            backend,
            ..config.unwrap_or_default()
        })
    }

    /// The store a CLI call in `cwd` uses.
    pub(crate) fn for_cli(cwd: &Path) -> Result<Self> {
        Ok(Self::open(&Self::effective_cli_config(cwd)?, Opener::Cli))
    }

    /// The activity log a CLI call in `cwd` reads.
    pub(crate) fn activity_for_cli(cwd: &Path) -> Result<Activity> {
        Ok(Activity::of(&Self::effective_cli_config(cwd)?, Opener::Cli))
    }

    /// Runs `f` with taskchampion's replica lock held throughout, so a busy
    /// lock stops all of `f`'s writes or none. Other backends just run `f`.
    pub(crate) fn while_locked<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        match self.backend() {
            Backend::Taskchampion(replica) => replica.store.while_locked(f),
            _ => f(),
        }
    }

    /// The taskchampion replica whose hook writes go through its queue:
    /// `None` for a backend whose writes always wait.
    pub(crate) fn queued_replica(&self) -> Option<&TaskchampionStore> {
        match self.backend() {
            Backend::Taskchampion(replica) => Some(&replica.store),
            _ => None,
        }
    }

    /// The replica at `dir` under `root`, for applying a queued hook write
    /// exactly where it was resolved. A hook's lock wait keeps a busy entry
    /// queued.
    pub(crate) fn queued_at(dir: &Path, root: &str) -> Self {
        Backend::Taskchampion(Replica {
            store: TaskchampionStore::at(dir.to_path_buf())
                .with_root(root)
                .with_lock_wait(HOOK_WAIT),
            syncs: false,
        })
        .recorded()
    }

    /// The replica `devkit todo sync` in `cwd` syncs, its target resolved:
    /// `None` for another backend or when no target resolves. Some but not
    /// all server credentials, or a client id that is not a UUID, is an
    /// error naming the variables.
    pub(crate) fn sync_replica(cwd: &Path) -> Result<Option<TaskchampionStore>> {
        let config = Self::cli_config(cwd)?;
        let env = std::env::var(BACKEND_VAR).ok();
        let (backend, _) = effective_backend(config.as_ref(), env.as_deref())?;
        if backend != TodoBackend::Taskchampion {
            return Ok(None);
        }
        let config = config.unwrap_or_default();
        Ok(sync_target(&config.taskchampion)?
            .map(|target| taskchampion(&config).with_target(target)))
    }

    /// The `[todo]` config a hook in `cwd` reads, with `backend_var`, the
    /// value of `DEVKIT_TODO_BACKEND`, applied. No config anywhere is the
    /// default config; a config that fails to load or an unknown backend is
    /// an error.
    fn hook_config(
        checkout: &Checkout,
        cwd: &Path,
        backend_var: Option<&str>,
    ) -> Result<TodoConfig> {
        let config = match devkit_common::config::resolve_in(checkout, None, cwd) {
            Ok((config, _)) => Some(config.todo),
            Err(e) if e.downcast_ref::<NoConfig>().is_some() => None,
            Err(e) => return Err(e),
        };
        let (backend, _) = effective_backend(config.as_ref(), backend_var)?;
        Ok(TodoConfig {
            backend,
            ..config.unwrap_or_default()
        })
    }

    /// The store a hook in `cwd` uses. A hook never fails on the store's
    /// account: a config that fails to load or an unknown
    /// `DEVKIT_TODO_BACKEND` is the built-in store.
    pub(crate) fn for_hook(checkout: &Checkout, cwd: &Path) -> Self {
        let backend_var = std::env::var(BACKEND_VAR).ok();
        match Self::hook_config(checkout, cwd, backend_var.as_deref()) {
            Ok(config) => Self::open(&config, Opener::Hook),
            Err(_) => Backend::Builtin(BuiltinStore::open()).recorded(),
        }
    }

    /// The activity log a hook in `cwd` writes, the one that goes with
    /// [`Store::for_hook`]'s store.
    pub(crate) fn activity_for_hook(checkout: &Checkout, cwd: &Path) -> Activity {
        let backend_var = std::env::var(BACKEND_VAR).ok();
        match Self::hook_config(checkout, cwd, backend_var.as_deref()) {
            Ok(config) => Activity::of(&config, Opener::Hook),
            Err(_) => Activity::Local(ActivityLog::open()),
        }
    }

    /// The store a stop hook in `cwd` reads open todos from, as
    /// [`Store::for_hook`] builds it, `backend_var` being the value of
    /// `DEVKIT_TODO_BACKEND`. `None` when the config fails to load, the
    /// backend is unknown, or `[todo] hold_stop` is off: the built-in store's
    /// lists are not the configured store's, so holding an agent to them
    /// would be wrong.
    pub(crate) fn for_hold(
        checkout: &Checkout,
        cwd: &Path,
        backend_var: Option<&str>,
    ) -> Option<Self> {
        let config = Self::hook_config(checkout, cwd, backend_var).ok()?;
        config.hold_stop.then(|| Self::open(&config, Opener::Hook))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "0b8d4c2e-5f6a-4b7c-8d9e-0f1a2b3c4d5e";

    fn config(backend: TodoBackend) -> TodoConfig {
        TodoConfig {
            backend,
            ..TodoConfig::default()
        }
    }

    #[test]
    fn the_env_wins_over_the_config() {
        let tw = config(TodoBackend::Taskwarrior);
        assert_eq!(
            effective_backend(Some(&tw), Some("taskchampion")).unwrap(),
            (TodoBackend::Taskchampion, BackendSource::Env)
        );
        assert_eq!(
            effective_backend(Some(&tw), Some("")).unwrap(),
            (TodoBackend::Taskwarrior, BackendSource::Config)
        );
    }

    #[test]
    fn the_config_wins_over_the_default() {
        assert_eq!(
            effective_backend(Some(&config(TodoBackend::Taskwarrior)), None).unwrap(),
            (TodoBackend::Taskwarrior, BackendSource::Config)
        );
        assert_eq!(
            effective_backend(None, None).unwrap(),
            (TodoBackend::Builtin, BackendSource::Default)
        );
    }

    #[test]
    fn an_unknown_env_value_names_the_variable_and_the_spellings() {
        let err = effective_backend(None, Some("taskchampio"))
            .unwrap_err()
            .to_string();
        assert!(err.contains(BACKEND_VAR), "{err}");
        assert!(err.contains("taskchampion"), "{err}");
    }

    fn resolved(
        values: [Option<&str>; 3],
    ) -> impl FnOnce(&SyncVars, Option<&DopplerScope>) -> [(Option<String>, Source); 3] {
        move |names, _| {
            assert_eq!(*names, SYNC_VARS);
            values.map(|v| (v.map(str::to_string), Source::Env))
        }
    }

    #[test]
    fn server_dir_wins_and_reads_no_credential() {
        let tc = TaskchampionConfig {
            server_dir: Some("/sync".into()),
            ..TaskchampionConfig::default()
        };
        let target = sync_target_with(&tc, |_, _| panic!("credentials read")).unwrap();
        assert!(matches!(target, Some(SyncTarget::Dir(d)) if d == Path::new("/sync")));
    }

    #[test]
    fn three_credentials_give_a_server() {
        let target = sync_target_with(
            &TaskchampionConfig::default(),
            resolved([Some("https://s"), Some(UUID), Some("k")]),
        )
        .unwrap();
        let Some(SyncTarget::Server {
            url,
            client_id,
            secret,
        }) = target
        else {
            panic!("no server: {target:?}");
        };
        assert_eq!(url, "https://s");
        assert_eq!(client_id.to_string(), UUID);
        assert_eq!(secret, b"k");
    }

    #[test]
    fn no_credentials_is_no_target() {
        let target = sync_target_with(&TaskchampionConfig::default(), resolved([None, None, None]));
        assert!(target.unwrap().is_none());
    }

    #[test]
    fn two_of_three_names_the_missing_one() {
        let err = sync_target_with(
            &TaskchampionConfig::default(),
            resolved([Some("https://s"), None, Some("hidden-secret")]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("missing: DEVKIT_TODO_SYNC_CLIENT_ID"), "{err}");
        assert!(!err.contains("hidden-secret"), "{err}");
    }

    #[test]
    fn a_client_id_that_is_not_a_uuid_names_its_variable() {
        let err = sync_target_with(
            &TaskchampionConfig::default(),
            resolved([Some("https://s"), Some("nope"), Some("k")]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("DEVKIT_TODO_SYNC_CLIENT_ID"), "{err}");
        assert!(!err.contains("nope"), "{err}");
    }

    /// A repository whose `devkit.toml` is `toml`, cut off from every config
    /// above it but the home config's `[todo]` table.
    fn repo_with(toml: &str) -> (tempfile::TempDir, Checkout) {
        let dir = tempfile::tempdir().unwrap();
        devkit_git::Git::fixture(dir.path())
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        std::fs::write(
            dir.path().join("devkit.toml"),
            format!("[config]\nroot = true\n\n{toml}"),
        )
        .unwrap();
        let checkout = Checkout::at(dir.path());
        (dir, checkout)
    }

    #[test]
    fn a_loadable_config_with_the_hold_on_holds() {
        let (dir, checkout) = repo_with("[todo]\nhold_stop = true\n");
        assert!(Store::for_hold(&checkout, dir.path(), None).is_some());
    }

    #[test]
    fn a_config_that_fails_to_load_never_holds() {
        let (dir, checkout) = repo_with("[todo]\nbackend = 3\n");
        assert!(Store::for_hold(&checkout, dir.path(), None).is_none());
    }

    #[test]
    fn an_unknown_backend_never_holds() {
        let (dir, checkout) = repo_with("[todo]\nhold_stop = true\n");
        assert!(Store::for_hold(&checkout, dir.path(), Some("jira")).is_none());
    }

    #[test]
    fn hold_stop_off_never_holds() {
        let (dir, checkout) = repo_with("[todo]\nhold_stop = false\n");
        assert!(Store::for_hold(&checkout, dir.path(), None).is_none());
    }

    #[test]
    fn a_padded_client_id_parses() {
        let padded = format!(" {UUID}\n");
        let target = sync_target_with(
            &TaskchampionConfig::default(),
            resolved([Some("https://s"), Some(&padded), Some("k")]),
        );
        assert!(matches!(target.unwrap(), Some(SyncTarget::Server { .. })));
    }
}
