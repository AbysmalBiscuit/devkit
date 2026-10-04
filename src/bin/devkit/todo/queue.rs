//! Hook writes to a taskchampion replica, kept in arrival order beside it.
//!
//! A sync holds the replica lock across its network calls, since taskchampion
//! runs a whole sync in one SQLite write transaction, so a hook that cannot
//! wait it out queues its write here. Every hook write goes through the
//! queue, so a later write never overtakes an earlier one, and whoever next
//! gets the replica lock applies what is queued.

use std::{
    fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use devkit_common::store::{LockBusy, open_lock, with_file_lock};
use devkit_todo::Holder;
use serde::{Deserialize, Serialize};

use crate::hook::todo::Mirror;

/// One queued hook write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Deferred {
    /// A native tool call to mirror, resolved when it was queued, filed
    /// under `root`.
    Capture { root: String, mirror: Mirror },
    /// A holder whose claims under `root` return to pending.
    Release { root: String, holder: Holder },
}

fn queue_path(dir: &Path) -> PathBuf {
    dir.join("deferred.jsonl")
}

/// Guards the queue file for the moment a line is added or removed.
fn file_lock(dir: &Path) -> PathBuf {
    dir.join("deferred.lock")
}

/// Held by the one process applying the queue.
fn drain_lock(dir: &Path) -> PathBuf {
    dir.join("deferred.drain.lock")
}

fn lines(dir: &Path) -> Result<Vec<String>> {
    let path = queue_path(dir);
    match fs::read_to_string(&path) {
        Ok(text) => Ok(text.lines().map(str::to_string).collect()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Appends `entry` to the queue in `dir`.
pub(crate) fn push(dir: &Path, entry: &Deferred) -> Result<()> {
    let line = serde_json::to_string(entry).context("serializing a deferred.jsonl entry")?;
    let path = queue_path(dir);
    with_file_lock(&file_lock(dir), || {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        writeln!(file, "{line}").with_context(|| format!("appending to {}", path.display()))
    })
}

/// The oldest entry, `None` on an empty queue. A line that does not parse
/// comes back as an error and is dropped by the next [`pop`].
fn peek(dir: &Path) -> Result<Option<Result<Deferred>>> {
    with_file_lock(&file_lock(dir), || {
        Ok(lines(dir)?
            .first()
            .map(|line| serde_json::from_str(line).context("parsing a deferred.jsonl entry")))
    })
}

fn pop(dir: &Path) -> Result<()> {
    with_file_lock(&file_lock(dir), || {
        let path = queue_path(dir);
        let rest = lines(dir)?.into_iter().skip(1).collect::<Vec<_>>();
        match rest.is_empty() {
            true => match fs::remove_file(&path) {
                Err(e) if e.kind() != ErrorKind::NotFound => {
                    Err(e).with_context(|| format!("removing {}", path.display()))
                }
                _ => Ok(()),
            },
            false => fs::write(&path, rest.join("\n") + "\n")
                .with_context(|| format!("rewriting {}", path.display())),
        }
    })
}

/// Applies the queue in `dir` oldest first, unless another process is
/// already applying it. An entry whose replica lock stays busy, a
/// [`LockBusy`], stops the drain and stays queued; any other failure is
/// reported to `failed` and the entry dropped, as a write that failed at once
/// would be. Returns how many entries were applied.
pub(crate) fn drain(
    dir: &Path,
    mut apply: impl FnMut(&Deferred) -> Result<()>,
    mut failed: impl FnMut(anyhow::Error),
) -> usize {
    let mut applied = 0;
    loop {
        let Ok(mut lock) = open_lock(&drain_lock(dir)) else {
            return applied;
        };
        let Ok(guard) = lock.try_write() else {
            return applied;
        };
        loop {
            let entry = match peek(dir) {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(_) => return applied,
            };
            match entry.and_then(|entry| apply(&entry)) {
                Err(e) if e.is::<LockBusy>() => return applied,
                Err(e) => failed(e),
                Ok(()) => applied += 1,
            }
            if pop(dir).is_err() {
                return applied;
            }
        }
        drop(guard);
        // An entry pushed after the last peek, while this process still held
        // the drain lock, is this process's to apply.
        if !matches!(peek(dir), Ok(Some(_))) {
            return applied;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(holder: &str) -> Deferred {
        Deferred::Release {
            root: "devkit".into(),
            holder: Holder::new(holder),
        }
    }

    #[test]
    fn entries_apply_in_order_and_leave_the_queue_empty() {
        let dir = tempfile::tempdir().unwrap();
        for holder in ["a", "b", "c"] {
            push(dir.path(), &release(holder)).unwrap();
        }
        let mut seen = Vec::new();
        let applied = drain(
            dir.path(),
            |e| {
                seen.push(e.clone());
                Ok(())
            },
            |e| panic!("{e:#}"),
        );
        assert_eq!(applied, 3);
        assert_eq!(seen, [release("a"), release("b"), release("c")]);
        assert!(!queue_path(dir.path()).exists());
    }

    #[test]
    fn a_busy_replica_keeps_the_entry_and_everything_after_it() {
        let dir = tempfile::tempdir().unwrap();
        for holder in ["a", "b"] {
            push(dir.path(), &release(holder)).unwrap();
        }
        let busy = |_: &Deferred| -> Result<()> {
            Err(LockBusy {
                path: "x".into(),
                wait: std::time::Duration::ZERO,
            }
            .into())
        };
        assert_eq!(drain(dir.path(), busy, |e| panic!("{e:#}")), 0);
        let mut seen = Vec::new();
        drain(
            dir.path(),
            |e| {
                seen.push(e.clone());
                Ok(())
            },
            |e| panic!("{e:#}"),
        );
        assert_eq!(seen, [release("a"), release("b")]);
    }

    #[test]
    fn a_failing_entry_is_reported_and_dropped() {
        let dir = tempfile::tempdir().unwrap();
        push(dir.path(), &release("a")).unwrap();
        let mut failures = 0;
        drain(dir.path(), |_| anyhow::bail!("claimed"), |_| failures += 1);
        assert_eq!(failures, 1);
        assert!(!queue_path(dir.path()).exists());
    }
}
