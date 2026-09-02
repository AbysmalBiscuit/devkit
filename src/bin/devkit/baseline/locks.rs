//! Advisory locks for baseline creation and deletion. Lock files live in
//! `<baseline_dir>/.locks/` rather than inside a baseline, so a lock survives
//! the removal of the tree it guards. They are persistent: unlinking one lets
//! separate processes lock different inodes for the same logical path, so a
//! lock file is never removed, not even when the baseline it named is gone.
//! `.locks` sits inside `baseline_dir` alongside the slots, so a sweep over
//! that directory skips it rather than treating it as a deletable slot.
//!
//! Lock ordering is fixed: the directory lock is taken before any slot lock,
//! never the reverse. Both waits are unbounded, so a caller that needs to run
//! work already under a lock must call the unlocked body directly — two opens
//! of one lock file are two open-file descriptions, and `flock` blocks a
//! process against itself.

use anyhow::{Context, Result};
use fd_lock::RwLock;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

/// Set on a hook child's environment during bootstrap, carrying the slot name
/// being built.
pub const REENTRY_VAR: &str = "DEVKIT_BASELINE_BOOTSTRAP";

const DIR: &str = ".locks";
/// No 12-hex slot name can equal this, so the directory lock and a slot lock
/// are always different files.
const DIR_LOCK: &str = "_dir";

fn lock_path(baseline_dir: &Path, stem: &str) -> PathBuf {
    // A stem carrying a separator can resolve to another lock's path — most
    // dangerously the directory lock, which a slot lock must never alias.
    // `format!` appends `.lock`, so a bare `..` is the single component
    // `...lock`; only a separator escapes the component.
    debug_assert!(!stem.contains(['/', '\\']), "lock stem is one component");
    baseline_dir.join(DIR).join(format!("{stem}.lock"))
}

/// The wait is unbounded on purpose. A worktree racing a long bootstrap must
/// wait and then find a finished tree, and a timed-out acquisition inside a
/// prune sweep would abandon the sweep while still holding the directory lock.
fn hold<T>(path: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let parent = path.parent().context("lock path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let mut lock = RwLock::new(file);
    let _held = lock
        .write()
        .with_context(|| format!("locking {}", path.display()))?;
    f()
}

/// Whether the ambient re-entry marker names the slot about to be locked.
pub fn reentry_conflict(marker: Option<&str>, slot_name: &str) -> bool {
    marker == Some(slot_name)
}

/// Serializes work on one baseline slot. The key is the slot **directory
/// name** (`d13d90b724bf`, or `d13d90b724bf_2` after a collision), not the sha:
/// two shas can share a short prefix and land in different directories, and a
/// caller holding only a path can always name the slot.
pub fn with_slot<T>(
    baseline_dir: &Path,
    slot_name: &str,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let marker = std::env::var(REENTRY_VAR).ok();
    with_slot_marked(baseline_dir, slot_name, marker.as_deref(), f)
}

/// [`with_slot`] with the re-entry marker supplied rather than read from the
/// environment.
///
/// The bail precedes the lock: a hook that runs `devrun up --role baseline`
/// for the baseline currently bootstrapping would otherwise block on a lock
/// its own parent holds, forever and silently, and the diagnostic naming the
/// hook would never print.
fn with_slot_marked<T>(
    baseline_dir: &Path,
    slot_name: &str,
    marker: Option<&str>,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    if reentry_conflict(marker, slot_name) {
        anyhow::bail!(
            "an `after_worktree_create` hook ran `devrun up --role baseline` for the \
             baseline being bootstrapped ({slot_name}); remove that call from the hook"
        );
    }
    hold(&lock_path(baseline_dir, slot_name), f)
}

/// Serializes a sweep over the whole directory against concurrent deletions.
pub fn with_dir<T>(baseline_dir: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    hold(&lock_path(baseline_dir, DIR_LOCK), f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marker_conflicts_only_with_its_own_slot() {
        assert!(reentry_conflict(Some("d13d90b724bf"), "d13d90b724bf"));
        assert!(!reentry_conflict(Some("d13d90b724bf"), "0123456789ab"));
        assert!(!reentry_conflict(None, "d13d90b724bf"));
        // A collision slot is a different tree and so a different lock.
        assert!(!reentry_conflict(Some("d13d90b724bf"), "d13d90b724bf_2"));
    }

    /// The bail must happen before the lock is taken: a re-entrant call that
    /// locked first would deadlock against its own parent and the diagnostic
    /// would never print. The absent lock file is what proves the order.
    #[test]
    fn a_reentrant_slot_bails_without_taking_its_lock() {
        let dir = tempfile::tempdir().unwrap();
        let err = with_slot_marked(
            dir.path(),
            "d13d90b724bf",
            Some("d13d90b724bf"),
            || -> Result<()> { panic!("the closure must not run") },
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("d13d90b724bf"), "{msg}");
        assert!(msg.contains("after_worktree_create"), "{msg}");
        assert!(
            !dir.path().join(".locks/d13d90b724bf.lock").exists(),
            "{msg}"
        );
    }

    #[test]
    fn an_unrelated_marker_locks_and_runs() {
        let dir = tempfile::tempdir().unwrap();
        let got =
            with_slot_marked(dir.path(), "d13d90b724bf", Some("0123456789ab"), || Ok(3)).unwrap();
        assert_eq!(got, 3);
        assert!(dir.path().join(".locks/d13d90b724bf.lock").exists());
    }

    #[test]
    fn a_slot_lock_runs_its_closure_and_releases() {
        let dir = tempfile::tempdir().unwrap();
        let got = with_slot(dir.path(), "d13d90b724bf", || Ok(7)).unwrap();
        assert_eq!(got, 7);
        // Released: a second acquisition in the same thread would otherwise block
        // forever, since two opens of one lock file are two open-file descriptions.
        assert_eq!(with_slot(dir.path(), "d13d90b724bf", || Ok(8)).unwrap(), 8);
    }

    #[test]
    fn different_slots_do_not_block_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let got = with_slot(dir.path(), "aaaaaaaaaaaa", || {
            with_slot(dir.path(), "bbbbbbbbbbbb", || Ok(1))
        })
        .unwrap();
        assert_eq!(got, 1);
    }

    #[test]
    fn the_dir_lock_and_a_slot_lock_nest_in_that_order() {
        let dir = tempfile::tempdir().unwrap();
        let got = with_dir(dir.path(), || {
            with_slot(dir.path(), "aaaaaaaaaaaa", || Ok(2))
        })
        .unwrap();
        assert_eq!(got, 2);
    }

    /// Mutual exclusion, which is the whole point of the lock. Two opens of one
    /// lock file are two open-file descriptions, so `flock` serializes them even
    /// inside a single process — the same guarantee that holds across processes.
    /// The order of the two appends is what proves the contender waited.
    #[test]
    fn a_contender_waits_for_the_holder() {
        use std::sync::mpsc;

        let dir = tempfile::tempdir().unwrap();
        let log = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let (started_tx, started_rx) = mpsc::channel();

        std::thread::scope(|s| {
            with_slot(dir.path(), "aaaaaaaaaaaa", || {
                let log_b = std::sync::Arc::clone(&log);
                let path = dir.path().to_path_buf();
                s.spawn(move || {
                    started_tx.send(()).unwrap();
                    with_slot(&path, "aaaaaaaaaaaa", || {
                        log_b.lock().unwrap().push('B');
                        Ok(())
                    })
                    .unwrap();
                });
                // The contender has entered `with_slot` and can only be blocking on
                // the lock this closure holds. Waiting for the signal rather than
                // sleeping keeps the test honest on a loaded runner.
                started_rx.recv().unwrap();
                log.lock().unwrap().push('A');
                Ok(())
            })
            .unwrap();
        });

        assert_eq!(*log.lock().unwrap(), "AB", "the contender did not wait");
    }
}
