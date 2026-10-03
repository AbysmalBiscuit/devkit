//! The one rule every status change goes through, so each backend derives the
//! stored holder the same way.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::Holder;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Status {
    Pending,
    InProgress {
        by: Holder,
    },
    /// `by` is who finished it, `None` when unknown.
    Completed {
        by: Option<Holder>,
    },
    /// A soft delete: the record stays.
    Cancelled {
        by: Option<Holder>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusKind {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl Status {
    pub fn kind(&self) -> StatusKind {
        match self {
            Self::Pending => StatusKind::Pending,
            Self::InProgress { .. } => StatusKind::InProgress,
            Self::Completed { .. } => StatusKind::Completed,
            Self::Cancelled { .. } => StatusKind::Cancelled,
        }
    }
}

/// A status change refused because another holder has the todo in progress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claimed {
    pub by: Holder,
}

impl fmt::Display for Claimed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "todo is in progress by {}", self.by)
    }
}

impl std::error::Error for Claimed {}

/// The status a todo moves to when `actor` asks for `to`, or `Ok(None)` when
/// nothing changes.
///
/// An actor that covers the claimant repeats a claim without taking it, so a
/// sub-agent's claim survives its session starting the same todo. A sub-agent
/// takes over a claim its session holds, so delegating does not lock it out.
/// Any other change to a todo in progress needs an actor that covers the
/// claimant.
pub fn transition(current: &Status, to: StatusKind, actor: &Holder) -> Result<Option<Status>, Claimed> {
    let claimant = match current {
        Status::InProgress { by } => {
            if to == StatusKind::InProgress && actor.covers(by) {
                return Ok(None);
            }
            if to == StatusKind::InProgress && by.covers(actor) {
                return Ok(Some(Status::InProgress { by: actor.clone() }));
            }
            if !actor.covers(by) {
                return Err(Claimed { by: by.clone() });
            }
            Some(by)
        }
        other if other.kind() == to => return Ok(None),
        _ => None,
    };
    let finisher = || Some(claimant.unwrap_or(actor).clone());
    Ok(Some(match to {
        StatusKind::Pending => Status::Pending,
        StatusKind::InProgress => Status::InProgress { by: actor.clone() },
        StatusKind::Completed => Status::Completed { by: finisher() },
        StatusKind::Cancelled => Status::Cancelled { by: finisher() },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(s: &str) -> Holder {
        Holder::new(s)
    }

    fn ip(s: &str) -> Status {
        Status::InProgress { by: h(s) }
    }

    #[test]
    fn pending_to_in_progress_records_the_actor() {
        assert_eq!(
            transition(&Status::Pending, StatusKind::InProgress, &h("S")),
            Ok(Some(ip("S")))
        );
    }

    #[test]
    fn the_session_repeating_its_sub_agents_claim_keeps_the_sub_agent() {
        assert_eq!(transition(&ip("S/a1"), StatusKind::InProgress, &h("S")), Ok(None));
    }

    #[test]
    fn a_sub_agent_takes_over_its_parents_claim() {
        assert_eq!(
            transition(&ip("S"), StatusKind::InProgress, &h("S/a1")),
            Ok(Some(ip("S/a1")))
        );
    }

    #[test]
    fn a_sibling_conflicts() {
        let claimed = Err(Claimed { by: h("S/a1") });
        assert_eq!(transition(&ip("S/a1"), StatusKind::InProgress, &h("S/a2")), claimed);
        assert_eq!(transition(&ip("S/a1"), StatusKind::Completed, &h("S/a2")), claimed);
        assert_eq!(transition(&ip("S/a1"), StatusKind::Pending, &h("S/a2")), claimed);
    }

    #[test]
    fn finishing_under_a_covering_actor_credits_the_claimant() {
        assert_eq!(
            transition(&ip("S/a1"), StatusKind::Completed, &h("S")),
            Ok(Some(Status::Completed { by: Some(h("S/a1")) }))
        );
    }

    #[test]
    fn finishing_unclaimed_credits_the_actor() {
        assert_eq!(
            transition(&Status::Pending, StatusKind::Cancelled, &h("S")),
            Ok(Some(Status::Cancelled { by: Some(h("S")) }))
        );
    }

    #[test]
    fn human_overrides_any_claim() {
        assert_eq!(
            transition(&ip("S/a1"), StatusKind::Pending, &Holder::human()),
            Ok(Some(Status::Pending))
        );
    }

    #[test]
    fn same_kind_is_a_no_op_outside_in_progress() {
        let done = Status::Completed { by: Some(h("S")) };
        assert_eq!(transition(&done, StatusKind::Completed, &h("T")), Ok(None));
        assert_eq!(transition(&Status::Pending, StatusKind::Pending, &h("T")), Ok(None));
    }

    #[test]
    fn undone_drops_the_holder() {
        let done = Status::Completed { by: Some(h("S")) };
        assert_eq!(transition(&done, StatusKind::Pending, &h("S")), Ok(Some(Status::Pending)));
    }
}
