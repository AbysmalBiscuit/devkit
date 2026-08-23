//! The tracker seam: one contract over Linear, GitHub Issues, or no tracker.

use serde::{Deserialize, Serialize};

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
}
