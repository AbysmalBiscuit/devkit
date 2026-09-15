//! Retention: deleting whole files, never rewriting one.
//!
//! Per-file deletion is what keeps the append-only invariant intact — a file is
//! either wholly there or gone, and no reader ever sees a partial one — and a
//! day directory is removed once it is empty.
//!
//! Age first: a directory whose UTC name is older than `max_age_days`, measured
//! from the name rather than from any mtime, since a file touched late in a
//! stale directory should not keep it alive. Then size: oldest first until the
//! total is under `max_bytes`.
//!
//! The whole sweep is fail-open. A prune failure never changes a hook's exit.

use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use chrono::NaiveDate;
use fd_lock::RwLock;

use super::Settings;

/// The only thing standing between a sweep and a live session's file, because
/// writers never take a lock at all. Without it, `auto_prune` turns a rare
/// accident into a scheduled one: a session starting at 23:50 UTC and running
/// two hours loses its earlier records the first time any other session ends
/// after midnight and finds yesterday's directory over a cap.
///
/// This is an approximation of a liveness check, and it approximates the case
/// that actually happens.
const RECENCY_WINDOW: Duration = Duration::from_secs(60 * 60);

/// What a sweep did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    pub files_removed: usize,
    pub bytes_freed: u64,
    pub dirs_removed: usize,
    /// Another pruner held the lock, so this one did nothing.
    pub bailed: bool,
    /// Whether the size sweep ended under `max_bytes`. `true` when no cap is
    /// set. A sweep that cannot meet the cap reports that rather than deleting
    /// past its guards: today's directory alone over the cap, or every
    /// remaining file inside the recency window, is not a reason to delete a
    /// live session's records.
    pub cap_reached: bool,
}

/// Sweep the log directory. Fail-open throughout: an unreadable directory, a
/// removal that fails, a lock that cannot be taken — each ends the sweep rather
/// than failing the caller.
pub fn sweep(settings: &Settings) -> Outcome {
    sweep_at(settings, SystemTime::now())
}

/// [`sweep`] against an explicit clock, so the current-day guard is testable
/// without waiting for midnight.
pub fn sweep_at(settings: &Settings, now: SystemTime) -> Outcome {
    let mut out = Outcome {
        cap_reached: true,
        ..Default::default()
    };
    if !settings.dir.exists() {
        return out;
    }
    // A held lock means bail immediately, never a blocking wait: session end
    // must not sit behind another session's sweep. The lock and its guard are
    // both locals here because the guard borrows the lock, so the pair has to
    // live for the whole sweep.
    let Some(mut lock) = open_lock(&settings.dir.join("prune.lock")) else {
        out.bailed = true;
        return out;
    };
    let Ok(_guard) = lock.try_write() else {
        out.bailed = true;
        return out;
    };

    let today = super::writer::day_dir(now);
    let mut days = day_dirs(&settings.dir);
    days.sort_by(|a, b| a.name.cmp(&b.name));

    if let Some(max_age) = settings.max_age_days {
        let cutoff = day_from_name(&today).map(|d| d - chrono::Duration::days(i64::from(max_age)));
        for day in &days {
            let (Some(cutoff), Some(named)) = (cutoff, day_from_name(&day.name)) else {
                // A directory whose name is not a date is not devkit's to age
                // out: it names no day to measure from.
                continue;
            };
            if named < cutoff {
                remove_files(&day.path, now, &mut out);
            }
        }
    }

    if let Some(max_bytes) = settings.max_bytes {
        let mut total: u64 = days.iter().map(|d| dir_size(&d.path)).sum();
        for day in &days {
            if total <= max_bytes {
                break;
            }
            // Today never goes, whatever the size sweep says.
            if day.name == today {
                continue;
            }
            let before = out.bytes_freed;
            remove_files(&day.path, now, &mut out);
            total = total.saturating_sub(out.bytes_freed - before);
        }
        out.cap_reached = total <= max_bytes;
    }

    for day in &days {
        if is_empty(&day.path) && remove_ok(std::fs::remove_dir(&day.path)) {
            out.dirs_removed += 1;
        }
    }
    out
}

struct Day {
    name: String,
    path: PathBuf,
}

fn day_dirs(root: &Path) -> Vec<Day> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| Day {
            name: e.file_name().to_string_lossy().into_owned(),
            path: e.path(),
        })
        .collect()
}

/// The UTC day a directory name means, or `None` when it names no day.
fn day_from_name(name: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(name, "%Y-%m-%d").ok()
}

/// Delete every file in `dir` outside the recency window, accumulating into
/// `out`. Files inside the window are spared: they may belong to a session that
/// is still writing.
fn remove_files(dir: &Path, now: SystemTime, out: &mut Outcome) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_file() || path.file_name().is_some_and(|n| n == "prune.lock") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if is_fresh(&meta, now) {
            continue;
        }
        let size = meta.len();
        if remove_ok(std::fs::remove_file(&path)) {
            out.files_removed += 1;
            out.bytes_freed += size;
        }
    }
}

fn is_fresh(meta: &std::fs::Metadata, now: SystemTime) -> bool {
    meta.modified()
        .ok()
        .and_then(|m| now.duration_since(m).ok())
        .is_none_or(|age| age < RECENCY_WINDOW)
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| e.metadata().ok())
        .filter(std::fs::Metadata::is_file)
        .map(|m| m.len())
        .sum()
}

fn is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut e| e.next().is_none())
}

/// A `NotFound` from a removal is success, not an error: a lock file deleted by
/// hand, or a pruner killed mid-sweep, leaves exactly that.
fn remove_ok(r: std::io::Result<()>) -> bool {
    match r {
        Ok(()) => true,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// The lock file, opened but not yet locked. Separate from taking the lock
/// because a `fd_lock` guard borrows its `RwLock`, so a function returning the
/// guard alone would return a borrow of a local.
///
/// Two opens of one lock file are two open file descriptions, so this excludes
/// another pruner *within* one process as well as across them.
fn open_lock(path: &Path) -> Option<RwLock<std::fs::File>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .ok()?;
    Some(RwLock::new(file))
}

#[cfg(test)]
mod tests {
    use devkit_config::{Fidelity, PromptFidelity};

    use super::*;

    /// Binds the `TempDir` for as long as its paths are used: a helper handing
    /// back a path derived from a guard must hand back the guard too.
    fn tree(days: &[(&str, &[&str])]) -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        for (day, files) in days {
            let dir = t.path().join(day);
            std::fs::create_dir_all(&dir).unwrap();
            for f in *files {
                std::fs::write(dir.join(f), "{}\n").unwrap();
            }
        }
        t
    }

    fn settings(dir: &Path, max_age_days: Option<u32>, max_bytes: Option<u64>) -> Settings {
        Settings {
            enabled: true,
            command: Fidelity::Redacted,
            prompt: PromptFidelity::Off,
            auto_prune: true,
            dir: dir.to_path_buf(),
            max_age_days,
            max_bytes,
        }
    }

    fn now() -> SystemTime {
        SystemTime::now()
    }

    fn today_utc() -> String {
        super::super::writer::day_dir(now())
    }

    fn set_mtime_days_ago(path: &Path, days: u64) {
        let when = now() - Duration::from_secs(days * 86_400);
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_modified(when).unwrap();
    }

    fn touch_now(path: &Path) {
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_modified(now()).unwrap();
    }

    fn write_bytes(path: &Path, n: usize) {
        std::fs::write(path, "x".repeat(n)).unwrap();
        set_mtime_days_ago(path, 5);
    }

    #[test]
    fn the_retention_sweep_reads_the_directory_name_not_an_mtime() {
        let t = tree(&[("2020-01-01", &["s1.jsonl"]), ("2099-01-01", &["s2.jsonl"])]);
        // A fresh mtime in a stale directory must not keep it alive, so the
        // file is aged past the recency window but the directory name is what
        // decides.
        set_mtime_days_ago(&t.path().join("2020-01-01/s1.jsonl"), 2);
        sweep(&settings(t.path(), Some(30), None));
        assert!(!t.path().join("2020-01-01").exists());
        assert!(t.path().join("2099-01-01").exists());
    }

    #[test]
    fn a_file_touched_inside_the_recency_window_is_spared() {
        let t = tree(&[("2020-01-01", &["live.jsonl", "old.jsonl"])]);
        touch_now(&t.path().join("2020-01-01/live.jsonl"));
        set_mtime_days_ago(&t.path().join("2020-01-01/old.jsonl"), 400);
        sweep(&settings(t.path(), Some(30), None));
        assert!(
            t.path().join("2020-01-01/live.jsonl").exists(),
            "a live session's file"
        );
        assert!(!t.path().join("2020-01-01/old.jsonl").exists());
        assert!(
            t.path().join("2020-01-01").exists(),
            "and the directory stays while it holds one"
        );
    }

    #[test]
    fn a_directory_is_removed_only_once_empty() {
        let t = tree(&[("2020-01-01", &["a.jsonl"])]);
        set_mtime_days_ago(&t.path().join("2020-01-01/a.jsonl"), 400);
        let out = sweep(&settings(t.path(), Some(30), None));
        assert!(!t.path().join("2020-01-01").exists());
        assert_eq!(out.files_removed, 1);
        assert_eq!(out.dirs_removed, 1);
    }

    #[test]
    fn todays_directory_survives_the_size_sweep() {
        let today = today_utc();
        let t = tree(&[(today.as_str(), &["big.jsonl"])]);
        write_bytes(&t.path().join(&today).join("big.jsonl"), 10_000);
        let out = sweep(&settings(t.path(), None, Some(100)));
        assert!(t.path().join(&today).exists());
        assert!(
            !out.cap_reached,
            "a sweep that cannot meet the cap reports it rather than deleting on"
        );
        assert_eq!(out.files_removed, 0);
    }

    #[test]
    fn the_size_sweep_takes_the_oldest_first() {
        let today = today_utc();
        let t = tree(&[("2020-01-01", &[]), ("2020-06-01", &[]), (&today, &[])]);
        write_bytes(&t.path().join("2020-01-01/a.jsonl"), 1_000);
        write_bytes(&t.path().join("2020-06-01/b.jsonl"), 1_000);
        write_bytes(&t.path().join(&today).join("c.jsonl"), 1_000);
        let out = sweep(&settings(t.path(), None, Some(2_100)));
        assert!(!t.path().join("2020-01-01").exists(), "the oldest went");
        assert!(t.path().join("2020-06-01").exists(), "and nothing more");
        assert!(out.cap_reached);
        assert_eq!(out.bytes_freed, 1_000);
    }

    #[test]
    fn a_held_lock_bails_immediately() {
        let t = tree(&[("2020-01-01", &["a.jsonl"])]);
        set_mtime_days_ago(&t.path().join("2020-01-01/a.jsonl"), 400);
        let mut lock = open_lock(&t.path().join("prune.lock")).expect("the lock file");
        let _held = lock.try_write().expect("the first lock");
        let start = std::time::Instant::now();
        let out = sweep(&settings(t.path(), Some(1), None));
        assert!(out.bailed);
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "never a blocking wait"
        );
        assert!(
            t.path().join("2020-01-01/a.jsonl").exists(),
            "a bail deletes nothing"
        );
    }

    #[test]
    fn a_directory_that_names_no_day_is_left_alone() {
        let t = tree(&[("not-a-date", &["a.jsonl"])]);
        set_mtime_days_ago(&t.path().join("not-a-date/a.jsonl"), 400);
        sweep(&settings(t.path(), Some(1), None));
        assert!(
            t.path().join("not-a-date/a.jsonl").exists(),
            "it names no day to measure from"
        );
    }

    #[test]
    fn no_caps_means_unlimited() {
        let t = tree(&[("2020-01-01", &["a.jsonl"])]);
        set_mtime_days_ago(&t.path().join("2020-01-01/a.jsonl"), 4_000);
        let out = sweep(&settings(t.path(), None, None));
        assert!(t.path().join("2020-01-01/a.jsonl").exists());
        assert_eq!(out.files_removed, 0);
        assert!(out.cap_reached, "no cap is a cap that is always met");
    }

    #[test]
    fn a_missing_log_directory_is_not_an_error() {
        let t = tempfile::tempdir().unwrap();
        let out = sweep(&settings(&t.path().join("never-created"), Some(1), Some(1)));
        assert_eq!(out, Outcome {
            cap_reached: true,
            ..Default::default()
        });
    }

    #[test]
    fn the_prune_lock_is_never_swept_as_a_record() {
        let t = tree(&[("2020-01-01", &["a.jsonl"])]);
        let lock = t.path().join("2020-01-01/prune.lock");
        std::fs::write(&lock, "").unwrap();
        set_mtime_days_ago(&lock, 400);
        set_mtime_days_ago(&t.path().join("2020-01-01/a.jsonl"), 400);
        let out = sweep(&settings(t.path(), Some(1), None));
        assert_eq!(out.files_removed, 1, "the record, not the lock");
    }
}
