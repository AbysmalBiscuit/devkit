//! Syncing a taskchampion replica, only ever inside a `devkit todo sync`
//! process that nothing in devkit kills. `sync.lock` beside the replica is
//! held by the process syncing, `sync.pending` marks writes the server has
//! not seen, and `sync.failed` holds the last failure's reason.

use std::{
    fs::{self, File},
    io::ErrorKind,
    path::{Path, PathBuf},
    process::{Command, Stdio},
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
pub(crate) fn run(store: &TaskchampionStore, background: bool) -> Result<()> {
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
            if let Err(e) = store.sync_once() {
                let reason = format!("{e:#}");
                let _ = fs::write(failed_path(dir), &reason);
                if !background {
                    eprintln!("{}", failure_text(&reason));
                }
                break false;
            }
            let _ = fs::remove_file(failed_path(dir));
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

/// How a sync a caller waited for ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SyncOutcome {
    Done,
    Failed(String),
    StillRunning,
    NoTarget,
}

/// `devkit todo sync` as a detached child of this process, run in `cwd` so it
/// resolves the same store. With null stdio it never writes to a terminal or
/// a pipe its parent may have closed.
fn sync_command(cwd: &Path, background: bool) -> Result<Command> {
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args(["todo", "sync"])
        .args(background.then_some("--background"))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    devkit_common::sys::detach(&mut cmd);
    Ok(cmd)
}

/// Marks the replica pending and starts a background sync, without waiting.
pub(crate) fn spawn(store: &TaskchampionStore, cwd: &Path) {
    mark_pending(store.data_dir());
    if let Ok(mut cmd) = sync_command(cwd, true) {
        let _ = cmd.spawn();
    }
}

/// Starts a sync and waits for it up to `wait`. One still running at the
/// bound is left to finish on its own.
pub(crate) fn wait_for(store: &TaskchampionStore, cwd: &Path, wait: Duration) -> SyncOutcome {
    let started = SystemTime::now();
    let child = sync_command(cwd, false).and_then(|mut cmd| Ok(cmd.spawn()?));
    let mut child = match child {
        Ok(child) => child,
        Err(e) => return SyncOutcome::Failed(format!("{e:#}")),
    };
    let deadline = Instant::now() + wait;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => return SyncOutcome::StillRunning,
            Err(e) => return SyncOutcome::Failed(e.to_string()),
        }
    }
    let failed = failed_path(store.data_dir());
    let fresh = fs::metadata(&failed)
        .and_then(|m| m.modified())
        .is_ok_and(|at| at >= started);
    match fresh {
        true => SyncOutcome::Failed(fs::read_to_string(&failed).unwrap_or_default()),
        false => SyncOutcome::Done,
    }
}
