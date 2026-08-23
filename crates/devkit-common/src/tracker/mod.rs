//! The tracker seam: one contract over Linear, GitHub Issues, or no tracker.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

pub use devkit_config::TrackerKind;

#[cfg(any(test, feature = "test-support"))]
pub mod fake;
pub mod linear;
pub mod none;

/// Where an issue sits in its tracker's workflow. Linear's `state.type`
/// vocabulary, adopted as devkit's own because the status verdict, the triage
/// colours, and the dashboard bands were already written against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StateKind {
    Triage,
    Backlog,
    Unstarted,
    Started,
    Completed,
    Canceled,
}

impl StateKind {
    /// Every kind but the two terminal ones. The finished verdict and the
    /// dashboard's "open now" count both key off this.
    pub fn is_open(self) -> bool {
        !matches!(self, StateKind::Completed | StateKind::Canceled)
    }
}

impl std::fmt::Display for StateKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            StateKind::Triage => "triage",
            StateKind::Backlog => "backlog",
            StateKind::Unstarted => "unstarted",
            StateKind::Started => "started",
            StateKind::Completed => "completed",
            StateKind::Canceled => "canceled",
        })
    }
}

impl std::str::FromStr for StateKind {
    type Err = std::convert::Infallible;

    /// Never fails: a tracker may add a workflow state devkit has no name for,
    /// and one unknown state must not fail an entire status run.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "triage" => StateKind::Triage,
            "backlog" => StateKind::Backlog,
            "started" => StateKind::Started,
            "completed" => StateKind::Completed,
            "canceled" => StateKind::Canceled,
            _ => StateKind::Unstarted,
        })
    }
}

/// One issue's workflow state, as devkit renders it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub kind: StateKind,
    /// The tracker's own label, e.g. "In Progress" or "Not planned".
    pub name: String,
    /// Hex colour, when the tracker supplies one.
    pub color: Option<String>,
}

/// An issue id parsed from CLI input, plus the title slug when the input
/// carried one (a pasted issue URL usually does).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRef {
    pub id: String,
    pub slug: Option<String>,
}

/// A pull request linked to an issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRef {
    pub url: String,
    pub number: u64,
}

/// One issue assigned to the current user, with its state transitions. Drives
/// the dashboard's issues-over-time chart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssignedIssue {
    pub identifier: String,
    pub created_at: String,
    pub state: State,
    /// `(when, from, to)` per recorded transition, unsorted.
    pub history: Vec<(String, Option<State>, Option<State>)>,
}

/// Everything the issue summary file renders. Every field is empty rather than
/// absent when the tracker has nothing there.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueDetails {
    pub id: String,
    pub title: String,
    pub url: String,
    pub description: String,
    pub state: String,
    pub assignee: String,
    pub priority: String,
    pub estimate: String,
    pub labels: Vec<String>,
    pub parent: String,
    pub project: String,
}

/// One issue tracker. Every method is read-only: `devkit-issue` is a triage
/// facade and never mutates a tracker.
pub trait Tracker: Send + Sync {
    fn kind(&self) -> TrackerKind;
    /// Configured and able to authenticate. False means callers should degrade
    /// rather than error.
    fn ready(&self) -> bool;
    /// Parse CLI input — a bare id, a `#123`, or an issue URL — into an id and,
    /// when the input spelled one out, a title slug.
    fn issue_ref(&self, input: &str) -> IssueRef;
    fn title(&self, id: &str) -> Result<Option<String>>;
    fn details(&self, id: &str) -> Result<Option<IssueDetails>>;
    /// Batched: one round trip for every id.
    fn states(&self, ids: &[String]) -> HashMap<String, State>;
    fn issue_pr(&self, id: &str) -> Result<Option<PrRef>>;
    /// Issues that a bare number might refer to, for disambiguation.
    fn candidates(&self, n: u64) -> Result<Vec<IssueRef>>;
    /// PR URL to the issue ids it references.
    fn issues_for_prs(&self, urls: &[String]) -> HashMap<String, Vec<String>>;
    fn assigned_history(&self, on_page: &mut dyn FnMut(usize)) -> Result<Vec<AssignedIssue>>;
    /// Earliest timestamp the dashboard's timeline should start from.
    fn timeline_origin(&self) -> Result<Option<String>>;
    fn issue_url(&self, id: &str) -> Option<String>;
    /// A one-line identity for `devkit doctor`.
    fn check(&self) -> Result<String>;
}

/// The tracker for this project. An explicit `kind` always wins; otherwise a
/// resolvable Linear key, then a GitHub `origin` remote, then no tracker.
///
/// Detection is a floor, not a convenience: a globally exported
/// `LINEAR_API_KEY` resolves to Linear for every project, so a GitHub project on
/// such a machine must set `kind` explicitly. What detection buys is that every
/// config predating `[tracker]` keeps behaving exactly as it did.
///
/// This crate has no GitHub implementation yet, so that arm falls back to
/// `NoneTracker`. It warns only when the caller named GitHub explicitly —
/// detection choosing it is not something the user did wrong, and the read-only
/// paths that resolve a tracker per run would otherwise print on every call.
pub fn resolve(kind: Option<TrackerKind>, cwd: &Path) -> Box<dyn Tracker> {
    let explicitly_github = kind == Some(TrackerKind::Github);
    match kind.unwrap_or_else(|| detect(cwd)) {
        TrackerKind::Linear => Box::new(linear::LinearTracker::new(crate::secrets::resolve(
            "LINEAR_API_KEY",
        ))),
        TrackerKind::Github => {
            if explicitly_github {
                eprintln!(
                    "devkit: the GitHub tracker is not implemented yet — running without one"
                );
            }
            Box::new(none::NoneTracker)
        }
        TrackerKind::None => Box::new(none::NoneTracker),
    }
}

/// Detection order, used only when `[tracker] kind` is absent.
fn detect(cwd: &Path) -> TrackerKind {
    if crate::secrets::resolve("LINEAR_API_KEY").is_some() {
        return TrackerKind::Linear;
    }
    if crate::github::repo_slug(&cwd.to_string_lossy()).is_ok() {
        return TrackerKind::Github;
    }
    TrackerKind::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_kind_round_trips_through_its_wire_string() {
        for (k, s) in [
            (StateKind::Triage, "triage"),
            (StateKind::Backlog, "backlog"),
            (StateKind::Unstarted, "unstarted"),
            (StateKind::Started, "started"),
            (StateKind::Completed, "completed"),
            (StateKind::Canceled, "canceled"),
        ] {
            assert_eq!(k.to_string(), s);
            assert_eq!(s.parse::<StateKind>().unwrap(), k);
            assert_eq!(serde_json::to_value(k).unwrap(), serde_json::json!(s));
        }
    }

    #[test]
    fn an_unknown_state_string_parses_as_unstarted() {
        // A tracker may add a state devkit does not know. Treat it as open
        // rather than failing the whole status run.
        assert_eq!(
            "something-new".parse::<StateKind>().unwrap(),
            StateKind::Unstarted
        );
    }

    #[test]
    fn only_completed_and_canceled_are_closed() {
        assert!(StateKind::Triage.is_open());
        assert!(StateKind::Backlog.is_open());
        assert!(StateKind::Unstarted.is_open());
        assert!(StateKind::Started.is_open());
        assert!(!StateKind::Completed.is_open());
        assert!(!StateKind::Canceled.is_open());
    }

    #[test]
    fn state_kind_deserializes_from_its_wire_string() {
        for (s, k) in [
            ("triage", StateKind::Triage),
            ("backlog", StateKind::Backlog),
            ("unstarted", StateKind::Unstarted),
            ("started", StateKind::Started),
            ("completed", StateKind::Completed),
            ("canceled", StateKind::Canceled),
        ] {
            let value = serde_json::json!(s);
            assert_eq!(serde_json::from_value::<StateKind>(value).unwrap(), k);
        }
    }

    #[test]
    fn the_none_tracker_answers_empty_and_is_never_ready() {
        let t = resolve(Some(TrackerKind::None), Path::new("/nowhere"));
        assert_eq!(t.kind(), TrackerKind::None);
        assert!(!t.ready());
        assert!(t.states(&["ENG-1".into()]).is_empty());
        assert!(t.title("ENG-1").unwrap().is_none());
        assert!(t.details("ENG-1").unwrap().is_none());
        assert!(t.issue_url("ENG-1").is_none());
        assert!(t.candidates(7).unwrap().is_empty());
        assert!(t.assigned_history(&mut |_| {}).unwrap().is_empty());
    }

    #[test]
    fn the_none_tracker_passes_an_id_through_unchanged() {
        let t = resolve(Some(TrackerKind::None), Path::new("/nowhere"));
        let r = t.issue_ref("  eng-1  ");
        assert_eq!(r.id, "eng-1");
        assert_eq!(r.slug, None);
    }

    /// One directory, two explicit kinds, two different trackers: only the
    /// explicit argument can account for the difference, so detection cannot be
    /// deciding it. Nothing here reads the environment.
    #[test]
    fn the_same_directory_yields_whichever_kind_is_named() {
        let dir = Path::new("/nonexistent-devkit-tracker-probe");
        assert_eq!(
            resolve(Some(TrackerKind::Linear), dir).kind(),
            TrackerKind::Linear
        );
        assert_eq!(
            resolve(Some(TrackerKind::None), dir).kind(),
            TrackerKind::None
        );
    }
}
