//! Where a record lands, and the one infallible entry point that puts it there.
//!
//! `<dir>/<YYYY-MM-DD>/<session_id>[-<agent_id>].jsonl`, opened with
//! `append(true)`, one write per record, no lock.
//!
//! `agent_id` is in the filename because parallel subagents share a
//! `session_id` and would otherwise contend on one file. The day directory is
//! UTC, not local: prune's age arithmetic and its never-delete-today guard both
//! read the directory name, and a local-time name makes both wrong twice a
//! year.
//!
//! Append atomicity is relied on but not guaranteed. `append(true)` maps to
//! `O_APPEND` on Unix and `FILE_APPEND_DATA` on Windows, both of which make the
//! offset update atomic, and a single write of a few kilobytes to a regular
//! file does not interleave in practice. Nothing is staked on it: a reader
//! skips lines that do not parse and reports how many it skipped, so a torn
//! record costs one sample.

use std::{
    io::Write,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, SystemTime},
};

use chrono::{DateTime, Utc};

use super::{Record, Settings};

/// Well under a second. Nothing downstream waits on a record, and the log
/// directory is user-configurable to a network home, so a hung mount must not
/// hold the hook.
const RECORD_DEADLINE: Duration = Duration::from_millis(250);

/// Write one record, or do not. Never fails, because the caller cannot afford
/// it to: `guard_shell` turns a panic raised after its write stage is live into
/// a denial, so a logging panic here would deny a command the guard had already
/// allowed. Logging that can change a verdict is worse than no logging.
pub fn record(settings: &Settings, rec: &Record) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        record_faulted(settings, rec, env_fault());
    }));
}

/// The fault knob is read here and passed down rather than reached for inside
/// the worker, so a test injects one by argument. Reading the environment from
/// two threads at once is what makes that the wrong seam.
fn record_faulted(settings: &Settings, rec: &Record, fault: Option<String>) {
    if !settings.enabled {
        return;
    }
    let Ok(line) = serde_json::to_string(rec) else {
        return;
    };
    let settings = settings.clone();
    let session = rec.session_id.clone();
    let agent = rec.agent_id.clone();
    // `catch_unwind` makes this panic-safe, not block-safe, so the write runs
    // under its own deadline and a worker still blocked on the disk is
    // abandoned rather than waited on.
    let _ = with_deadline(RECORD_DEADLINE, move || {
        inject(fault.as_deref());
        let path = file_for(
            &settings,
            session.as_deref(),
            agent.as_deref(),
            SystemTime::now(),
        );
        write_line(&path, &line)
    });
}

/// The debug-only fault knob the failure-contract tests drive. It does not
/// exist in a release binary: `DEVKIT_HARNESS_LOG_FAULT` is read only under
/// `debug_assertions`, so no shipped build can be made to panic or stall here.
#[cfg(debug_assertions)]
fn env_fault() -> Option<String> {
    std::env::var("DEVKIT_HARNESS_LOG_FAULT").ok()
}

#[cfg(not(debug_assertions))]
fn env_fault() -> Option<String> {
    None
}

fn inject(fault: Option<&str>) {
    match fault {
        Some("panic") => panic!("DEVKIT_HARNESS_LOG_FAULT=panic"),
        // Longer than the deadline, so the caller abandons this worker.
        Some("block") => std::thread::sleep(Duration::from_secs(30)),
        _ => {}
    }
}

fn write_line(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    // One write, so the whole record reaches the file as a single append.
    f.write_all(format!("{line}\n").as_bytes())
}

/// The UTC day directory a record written at `now` belongs to.
pub fn day_dir(now: SystemTime) -> String {
    let t: DateTime<Utc> = now.into();
    t.format("%Y-%m-%d").to_string()
}

/// An RFC 3339 timestamp in UTC, for a record's `recorded_at`. File order is
/// not logical order — call B's pre can land before call A's post — so readers
/// sort by this.
pub fn now_rfc3339() -> String {
    let t: DateTime<Utc> = SystemTime::now().into();
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The file a record with these ids lands in.
pub fn file_for(
    settings: &Settings,
    session: Option<&str>,
    agent: Option<&str>,
    now: SystemTime,
) -> PathBuf {
    let dir = settings.dir.join(day_dir(now));
    let stem = match (
        session.and_then(sanitize_component),
        agent.and_then(sanitize_component),
    ) {
        (Some(s), Some(a)) => format!("{s}-{a}"),
        (Some(s), None) => s,
        // Cursor's `workspaceOpen` fires outside any agent session and carries
        // no conversation id, and a garbage payload carries nothing, so this is
        // a live path rather than a defensive one. The pid keeps two such
        // processes off one file.
        (None, _) => format!("unknown-{}", std::process::id()),
    };
    dir.join(format!("{stem}.jsonl"))
}

/// One id component, made safe to put in a path. Every character outside
/// `[A-Za-z0-9._-]` becomes `_`, and a component that is empty afterwards is
/// dropped rather than producing a nameless file.
pub fn sanitize_component(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

fn with_deadline<T: Send + 'static>(
    deadline: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ()> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx.recv_timeout(deadline).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use devkit_config::{Fidelity, PromptFidelity};

    use super::*;
    use crate::harness_log::{Kind, SCHEMA_VERSION};

    fn settings_at(dir: &std::path::Path) -> Settings {
        Settings {
            enabled: true,
            command: Fidelity::Redacted,
            prompt: PromptFidelity::Off,
            auto_prune: true,
            dir: dir.to_path_buf(),
            max_age_days: None,
            max_bytes: None,
        }
    }

    fn unix(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn a_record() -> Record {
        Record {
            schema_version: SCHEMA_VERSION,
            recorded_at: now_rfc3339(),
            devkit_version: "test".into(),
            analyzer_version: 1,
            harness: Some("claude-code".into()),
            event: "stop".into(),
            vendor_event: Some("Stop".into()),
            session_id: Some("s1".into()),
            agent_id: None,
            tool_use_id: None,
            cwd: None,
            project_root: None,
            kind: Kind::Lifecycle,
        }
    }

    #[test]
    fn the_day_directory_is_utc() {
        let dir = tempfile::tempdir().unwrap();
        let s = settings_at(dir.path());
        // 2026-01-01T00:30:00Z is still 2025-12-31 in UTC-8.
        let p = file_for(&s, Some("s1"), None, unix(1_767_227_400));
        assert!(p.to_string_lossy().contains("2026-01-01"), "{p:?}");
    }

    #[test]
    fn a_subagent_gets_its_own_file() {
        let dir = tempfile::tempdir().unwrap();
        let s = settings_at(dir.path());
        let solo = file_for(&s, Some("s1"), None, unix(0));
        let sub = file_for(&s, Some("s1"), Some("a1"), unix(0));
        assert_ne!(solo, sub, "parallel subagents share a session id");
        assert!(
            sub.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("s1-a1")
        );
    }

    #[test]
    fn a_path_separator_in_an_id_cannot_escape_the_directory() {
        // `.` is inside the allowed set, so a traversal loses its separators
        // rather than its dots. That is enough: the result is one component,
        // and `file_for` appends `.jsonl` to it, so even a bare `..` names an
        // ordinary file.
        assert_eq!(sanitize_component("../../etc"), Some(".._.._etc".into()));
        assert_eq!(sanitize_component("a/b\\c"), Some("a_b_c".into()));
        assert_eq!(sanitize_component(""), None);
        assert_eq!(
            sanitize_component("keeps.these-ok_1"),
            Some("keeps.these-ok_1".into())
        );
    }

    #[test]
    fn no_id_can_name_a_file_outside_the_day_directory() {
        let dir = tempfile::tempdir().unwrap();
        let s = settings_at(dir.path());
        let day = dir.path().join(day_dir(SystemTime::now()));
        for hostile in ["../../etc/passwd", "..", ".", "/etc/passwd", "a\\b"] {
            let p = file_for(&s, Some(hostile), None, SystemTime::now());
            assert_eq!(p.parent(), Some(day.as_path()), "{hostile} escaped: {p:?}");
        }
    }

    #[test]
    fn a_payload_with_no_session_id_still_lands() {
        let dir = tempfile::tempdir().unwrap();
        let s = settings_at(dir.path());
        let p = file_for(&s, None, None, unix(0));
        assert!(
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("unknown-")
        );
        // An agent id alone names no session, so it falls to the same place.
        let q = file_for(&s, None, Some("a1"), unix(0));
        assert_eq!(p, q);
    }

    #[test]
    fn a_record_lands_and_parses() {
        let dir = tempfile::tempdir().unwrap();
        let s = settings_at(dir.path());
        record(&s, &a_record());
        record(&s, &a_record());
        let path = file_for(&s, Some("s1"), None, SystemTime::now());
        let body = std::fs::read_to_string(&path).expect("the record file");
        let lines: Vec<_> = body.lines().collect();
        assert_eq!(lines.len(), 2, "one line per record, appended");
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v["kind"], "lifecycle");
        assert_eq!(v["event"], "stop");
        assert_eq!(v["vendor_event"], "Stop");
    }

    #[test]
    fn nothing_is_written_when_logging_is_off() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = settings_at(dir.path());
        s.enabled = false;
        record(&s, &a_record());
        assert!(
            std::fs::read_dir(dir.path()).unwrap().next().is_none(),
            "off means no file at all, not an empty one"
        );
    }

    #[test]
    fn a_panic_inside_record_never_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let s = settings_at(dir.path());
        record_faulted(&s, &a_record(), Some("panic".into()));
    }

    #[test]
    fn a_blocked_write_is_abandoned_rather_than_waited_on() {
        let dir = tempfile::tempdir().unwrap();
        let s = settings_at(dir.path());
        let start = std::time::Instant::now();
        record_faulted(&s, &a_record(), Some("block".into()));
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "the deadline bounds it: {elapsed:?}"
        );
    }
}
