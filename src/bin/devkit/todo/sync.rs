//! Syncing a taskchampion replica, only ever inside a `devkit todo sync`
//! process that nothing in devkit kills. `sync.lock` beside the replica is
//! held by the process syncing, `sync.pending` marks writes the server has
//! not seen, and `sync.failed` holds the last failure's reason.

use std::{
    ffi::OsStr,
    fs::{self, File},
    io::ErrorKind,
    path::{Path, PathBuf},
    process::{Child, Command},
    time::{Duration, Instant, SystemTime},
};

use anyhow::Result;
use devkit_todo_taskchampion::TaskchampionStore;

/// How long a failed sync holds off background attempts, so a server that is
/// down costs one attempt this often.
const FAILURE_HOLD: Duration = Duration::from_secs(60);

fn lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join("sync.lock")
}

fn pending_path(data_dir: &Path) -> PathBuf {
    data_dir.join("sync.pending")
}

fn failed_path(data_dir: &Path) -> PathBuf {
    data_dir.join("sync.failed")
}

/// Records that the replica has a write the server has not seen.
pub(crate) fn mark_pending(data_dir: &Path) {
    let _ = fs::create_dir_all(data_dir);
    let _ = File::create(pending_path(data_dir));
}

/// Writes `text` to `path`, readable only by this user on Unix: a sync
/// failure's reason can name the server.
fn write_private(path: &Path, text: &str) {
    let _ = fs::remove_file(path);
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    if let Ok(mut file) = options.open(path) {
        let _ = std::io::Write::write_all(&mut file, text.as_bytes());
    }
}

fn failed_recently(data_dir: &Path) -> bool {
    fs::metadata(failed_path(data_dir))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| at.elapsed().ok())
        .is_some_and(|age| age < FAILURE_HOLD)
}

/// The text `devkit todo sync` and `list --sync` print when a sync fails.
pub(crate) fn failure_text(reason: &str) -> String {
    format!("devkit todo: sync failed: {reason}; changes are saved and sync with the next write")
}

/// `devkit todo sync`. In the background it gives up at once when another
/// process is syncing or the last sync failed within [`FAILURE_HOLD`];
/// otherwise it waits its turn. Each pass clears `sync.pending` first and
/// reruns when a write set it meanwhile. A failure is reported, never
/// returned: the writes stay in the replica for the next sync.
///
/// `result`, when given, receives this invocation's own failure reason, so a
/// caller waiting on this process learns its result even when another sync
/// fails meanwhile; `sync.failed` only holds off background attempts.
pub(crate) fn run(
    store: &TaskchampionStore,
    background: bool,
    result: Option<&Path>,
) -> Result<()> {
    let dir = store.data_dir();
    if background && failed_recently(dir) {
        return Ok(());
    }
    loop {
        let mut lock = devkit_common::store::open_lock(&lock_path(dir))?;
        let guard = if background {
            match lock.try_write() {
                Ok(guard) => guard,
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(e.into()),
            }
        } else {
            lock.write()?
        };
        let succeeded = loop {
            let _ = fs::remove_file(pending_path(dir));
            crate::hook::todo::drain(dir);
            if let Err(e) = store.sync_once() {
                let reason = format!("{e:#}");
                write_private(&failed_path(dir), &reason);
                if let Some(result) = result {
                    write_private(result, &reason);
                }
                if !background {
                    eprintln!("{}", failure_text(&reason));
                }
                break false;
            }
            let _ = fs::remove_file(failed_path(dir));
            // Hook writes queued behind this sync's lock go out on a rerun.
            if crate::hook::todo::drain(dir) > 0 {
                mark_pending(dir);
            }
            if !pending_path(dir).exists() {
                break true;
            }
        };
        drop(guard);
        // A write that landed after the last check could find the lock still
        // held and leave its marker to this process.
        if !(succeeded && pending_path(dir).exists()) {
            return Ok(());
        }
    }
}

/// How long a caller that wants fresh lists waits for a sync.
pub(crate) const FRESH_WAIT: Duration = Duration::from_secs(20);

/// `list --sync`'s help, so the wait it states is the one it waits.
pub(crate) fn list_sync_help() -> String {
    format!(
        "Sync the todo store before listing; waits up to {} seconds",
        FRESH_WAIT.as_secs()
    )
}

/// `sync --background`'s help, stating the hold after a failure.
pub(crate) fn background_help() -> String {
    format!(
        "Give up at once when another sync is running or one failed in the last {} seconds. \
         What a write starts after it commits",
        FAILURE_HOLD.as_secs()
    )
}

/// How a sync a caller waited for ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SyncOutcome {
    Done,
    Failed(String),
    StillRunning,
    NoTarget,
}

/// Starts `devkit todo sync` detached from this process, in `cwd` so it
/// resolves the same store. It holds none of this process's handles, so it
/// never writes to a terminal or keeps a caller's pipe open.
fn start_sync(cwd: &Path, extra: &[&OsStr]) -> Result<Child> {
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args(["todo", "sync"]).args(extra).current_dir(cwd);
    Ok(devkit_common::sys::spawn_background(&mut cmd)?)
}

/// Marks the replica pending and starts a background sync, without waiting.
pub(crate) fn spawn(store: &TaskchampionStore, cwd: &Path) {
    mark_pending(store.data_dir());
    let _ = start_sync(cwd, &["--background".as_ref()]);
}

/// A result file no other waiter uses: this process's id and the time.
fn result_path(data_dir: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    data_dir.join(format!("sync.result.{}.{nanos}", std::process::id()))
}

/// Starts a sync and waits for it up to `wait`. One still running at the
/// bound is left to finish on its own.
pub(crate) fn wait_for(store: &TaskchampionStore, cwd: &Path, wait: Duration) -> SyncOutcome {
    let result = result_path(store.data_dir());
    let mut child = match start_sync(cwd, &["--result-file".as_ref(), result.as_os_str()]) {
        Ok(child) => child,
        Err(e) => return SyncOutcome::Failed(format!("{e:#}")),
    };
    let deadline = Instant::now() + wait;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => return SyncOutcome::StillRunning,
            Err(e) => return SyncOutcome::Failed(e.to_string()),
        }
    };
    let reason = fs::read_to_string(&result).ok();
    let _ = fs::remove_file(&result);
    match (reason, status.success()) {
        (Some(reason), _) => SyncOutcome::Failed(reason),
        (None, true) => SyncOutcome::Done,
        (None, false) => SyncOutcome::Failed(format!("devkit todo sync exited with {status}")),
    }
}
