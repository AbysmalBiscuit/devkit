//! The `start` status event, claimed on SessionStart and fired in the
//! background.
//!
//! The hook stays local: it reads the worktree's record and the project's
//! config, claims the event in the record, and leaves the tracker to a
//! detached `devkit issue event start`. It writes no stdout and every failure
//! is silence, so a SessionStart verdict never depends on it.

use std::{path::Path, process::Command};

use devkit_common::{
    record::{self, IssueRecord, RecordOrigin, RecordState},
    tracker::{self, TrackerKind},
    vcs::Checkout,
    worktree::IssueId,
};
use devkit_config::IssueEvent;

/// The background run's outcome, or its error, is appended here: the one
/// place a person can see why a move they never ran failed.
pub(crate) fn log_path(root: &Path) -> std::path::PathBuf {
    root.join(".devkit").join("issue-event.log")
}

/// Whether a session starting in this worktree may fire `start`: a worktree
/// `issue setup` created (or one whose record predates `origin`), for a
/// tracker issue, and not a legacy record already past `start`, which a
/// record carrying a PR but no `events` is.
fn qualifies(rec: &IssueRecord) -> bool {
    let from_setup = matches!(rec.origin, None | Some(RecordOrigin::Setup));
    let tracker_issue = rec
        .issue
        .parse::<IssueId>()
        .is_ok_and(|id| id.tracker().is_some());
    let legacy_past_start = rec.events.is_none() && rec.pr.is_some();
    from_setup && tracker_issue && !legacy_past_start
}

/// Claim `start` for the checkout and spawn its run, when the worktree
/// qualifies and the project configures `start` against a tracker.
pub(crate) fn on_session_start(checkout: &Checkout) {
    let Some(root) = checkout.root() else { return };
    let RecordState::Ok(rec) = record::read_state(root) else {
        return;
    };
    if !qualifies(&rec) {
        return;
    }
    let Ok((cfg, _)) = devkit_common::config::resolve_in(checkout, None, root) else {
        return;
    };
    if cfg.issue.events.start.is_none() {
        return;
    }
    let start = root.to_string_lossy();
    let forge = devkit_common::forge::resolve(&cfg.forge, &cfg.github, &start, None);
    if tracker::resolve(cfg.tracker.kind, root, &forge.repos)
        .tracker
        .kind()
        == TrackerKind::None
    {
        return;
    }
    if let Ok(true) = record::claim(root, IssueEvent::Start) {
        spawn(root);
    }
}

fn spawn(root: &Path) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.args(["issue", "event", "start", "--dir"])
        .arg(root)
        .arg("--log-file")
        .arg(log_path(root))
        .current_dir(root);
    let _ = devkit_common::sys::spawn_background(&mut cmd);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_setup_worktrees_with_a_tracker_id_qualify() {
        let base = IssueRecord {
            issue: "65".into(),
            origin: Some(RecordOrigin::Setup),
            events: Some(vec![]),
            ..Default::default()
        };
        let pr = Some(devkit_common::forge::PrLocator {
            repo: None,
            number: 7,
        });
        assert!(qualifies(&base));
        assert!(qualifies(&IssueRecord {
            origin: None,
            ..base.clone()
        }));
        assert!(qualifies(&IssueRecord {
            pr: pr.clone(),
            ..base.clone()
        }));
        assert!(!qualifies(&IssueRecord {
            origin: Some(RecordOrigin::Checkout),
            ..base.clone()
        }));
        assert!(!qualifies(&IssueRecord {
            issue: "UNKNOWN".into(),
            ..base.clone()
        }));
        assert!(!qualifies(&IssueRecord {
            issue: String::new(),
            ..base.clone()
        }));
        assert!(!qualifies(&IssueRecord {
            events: None,
            pr,
            ..base
        }));
    }
}
