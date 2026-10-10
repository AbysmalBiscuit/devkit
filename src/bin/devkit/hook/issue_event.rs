//! The `start` status event, claimed on SessionStart and fired in the
//! background.
//!
//! The hook stays local: it reads the worktree's record and the project's
//! config, claims the event in the record, and leaves the tracker to a
//! detached `devkit ticket event start`. It writes no stdout and every failure
//! is silence, so a SessionStart verdict never depends on it.

use std::{path::Path, process::Command};

use devkit_common::{
    record::{self, IssueRecord, RecordOrigin, RecordState},
    tracker::{self, TrackerKind},
    vcs::Checkout,
};
use devkit_config::IssueEvent;

/// The background run's outcome, or its error, is appended here: the one
/// place a person can see why a move they never ran failed.
pub(crate) fn log_path(root: &Path) -> std::path::PathBuf {
    root.join(".devkit").join("issue-event.log")
}

/// Whether a session starting in this worktree may fire `start`: a worktree
/// `workspace setup` created, for a tracker issue. The origin is required, not
/// inferred: `devrun up` gives a hand-made worktree a record named after its
/// branch, and a branch like `ENG-123` reads as a tracker id.
fn qualifies(rec: &IssueRecord) -> bool {
    rec.origin == Some(RecordOrigin::Setup) && rec.tracker_issue().is_some()
}

/// Claim `start` for the checkout and spawn its run, when the worktree
/// qualifies and the project configures `start` against a tracker.
pub(crate) fn on_session_start(checkout: &Checkout) {
    let Some(here) = checkout.here() else { return };
    let root = here.path.as_path();
    let RecordState::Ok(rec) = record::read_state(root) else {
        return;
    };
    if !rec.binds(&here.branch) || !qualifies(&rec) {
        return;
    }
    let Ok((cfg, _)) = devkit_common::config::resolve_in(checkout, None, root) else {
        return;
    };
    if cfg.ticket.events.start.is_none() {
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
    cmd.args(["ticket", "event", "start", "--dir"])
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
        assert!(qualifies(&base));
        assert!(qualifies(&IssueRecord {
            pr: Some(devkit_common::forge::PrLocator {
                repo: None,
                number: 7,
            }),
            ..base.clone()
        }));
        assert!(!qualifies(&IssueRecord {
            origin: None,
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
            ..base
        }));
    }
}
