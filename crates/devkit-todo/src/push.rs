//! Whether a local change to a ticket's todo may reach the ticket: the
//! tracker's current value against the baseline the last sync recorded.
//!
//! The baseline is the todo's `synced_status` for a status, and the store's
//! relation for a link, which sync keeps mirroring the tracker's.

use devkit_common::tracker::{
    status::same_status,
    tickets::{CloseReason, TicketStatus},
};
use devkit_config::{EventTransition, IssueEvent, TrackerKind};

use crate::ticket::status_name;

/// The status change a todo verb makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalStatus {
    Start,
    Done,
    Cancel,
}

impl LocalStatus {
    /// The `[ticket.events]` move this change fires.
    pub fn event(self) -> IssueEvent {
        match self {
            Self::Start => IssueEvent::TodoStart,
            Self::Done => IssueEvent::TodoDone,
            Self::Cancel => IssueEvent::TodoCancel,
        }
    }

    /// The todo verb that makes this change.
    pub fn word(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Done => "done",
            Self::Cancel => "cancel",
        }
    }

    /// How this change closes the ticket's issue on `tracker`: on GitHub,
    /// `done` and `cancel` close it; other trackers close nothing.
    pub fn closes(self, tracker: TrackerKind) -> Option<CloseReason> {
        match (self, tracker) {
            (Self::Done, TrackerKind::Github) => Some(CloseReason::Completed),
            (Self::Cancel, TrackerKind::Github) => Some(CloseReason::NotPlanned),
            _ => None,
        }
    }
}

/// What a push does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Write the change to the tracker.
    Write,
    /// The tracker already holds the change; nothing to write.
    Already,
    /// The ticket stands where the last sync left it, in a status the
    /// event's `from` does not name: the todo changes, the status stays. A
    /// GitHub `done` or `cancel` still closes the issue.
    NoMove,
    /// The tracker changed since the last sync; nothing may be written
    /// without asking.
    Conflict { current: String, baseline: String },
}

/// A status push's [`Verdict`] and what following it writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusVerdict {
    pub verdict: Verdict,
    /// What the push writes and records when it goes ahead: for `Write`,
    /// `Already` and `NoMove` as they stand, and for a `Conflict` once it is
    /// settled in favour of writing over the tracker.
    pub writes: StatusWrites,
}

/// The tracker writes a status push makes, in order, and the status it then
/// records as the ticket's last synced one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusWrites {
    /// Close the GitHub issue for this reason.
    pub close: Option<CloseReason>,
    /// Write the event's status field.
    pub field: Option<FieldWrite>,
    /// The status recorded once the writes land.
    pub record: Option<String>,
}

/// How a status push writes the event's `to`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldWrite {
    /// Set it, with no `from` check: the verdict already decided.
    Set,
    /// Move it as the event's `from` allows: the issue is already closed as
    /// the change asks, and a project field may still lag behind.
    Move,
}

/// How a missing baseline or link reads in a conflict.
const NONE: &str = "(none)";

/// The `from` entries that name a status. `*` names none, so an unset `from`
/// waives no tracker change.
fn named_from(event: &EventTransition) -> impl Iterator<Item = &str> {
    event
        .from
        .iter()
        .map(String::as_str)
        .filter(|f| f.trim() != "*")
}

/// What pushing `local` does to a ticket at `current`, whose last synced
/// status is `baseline`, with `event` the `[ticket.events]` move the change
/// fires on `tracker`. Statuses use [`status_name`] spelling. On GitHub, `done`
/// and `cancel` also close the issue, so a closed issue is compared by its
/// close reason and an open one by its project field.
pub fn status_verdict(
    local: LocalStatus,
    baseline: Option<&str>,
    current: &TicketStatus,
    event: Option<&EventTransition>,
    tracker: TrackerKind,
) -> StatusVerdict {
    let verdict = verdict_of(local, baseline, current, event, tracker);
    let closes = local.closes(tracker);
    let closed = closes.map(|reason| status_name(&TicketStatus::Closed(reason)));
    let writes = match &verdict {
        Verdict::Write | Verdict::Conflict { .. } => StatusWrites {
            close: closes,
            field: event.map(|_| FieldWrite::Set),
            record: closed.or_else(|| event.map(|e| e.to.clone())),
        },
        Verdict::Already => StatusWrites {
            close: None,
            field: (closes.is_some() && event.is_some()).then_some(FieldWrite::Move),
            record: Some(status_name(current)),
        },
        Verdict::NoMove => StatusWrites {
            close: closes,
            field: None,
            record: closed,
        },
    };
    StatusVerdict { verdict, writes }
}

fn verdict_of(
    local: LocalStatus,
    baseline: Option<&str>,
    current: &TicketStatus,
    event: Option<&EventTransition>,
    tracker: TrackerKind,
) -> Verdict {
    let closes = local.closes(tracker);
    let name = status_name(current);
    let conflict = || Verdict::Conflict {
        current: name.clone(),
        baseline: baseline.unwrap_or(NONE).to_string(),
    };
    match (closes, current) {
        (Some(want), TicketStatus::Closed(reason)) if *reason == want => return Verdict::Already,
        (Some(_), TicketStatus::Closed(_)) => return conflict(),
        (None, TicketStatus::Named(at)) if event.is_some_and(|e| same_status(at, &e.to)) => {
            return Verdict::Already;
        }
        _ => {}
    }
    let Some(event) = event else {
        return match closes {
            Some(_) => Verdict::Write,
            None => Verdict::NoMove,
        };
    };
    let named: Vec<&str> = named_from(event).collect();
    let in_from = named.iter().any(|f| same_status(f, &name));
    let unmoved = baseline.is_some_and(|b| same_status(b, &name));
    match (unmoved, in_from) {
        (_, true) => Verdict::Write,
        (true, false) if named.is_empty() => Verdict::Write,
        (true, false) => Verdict::NoMove,
        (false, false) => conflict(),
    }
}

/// What setting a ticket's parent to `target` does, with `baseline` its
/// parent in the store and `current` its parent on the tracker, all ticket
/// keys.
pub fn parent_verdict(
    baseline: Option<&str>,
    current: Option<&str>,
    target: Option<&str>,
) -> Verdict {
    if current == target {
        Verdict::Already
    } else if current == baseline {
        Verdict::Write
    } else {
        Verdict::Conflict {
            current: current.unwrap_or(NONE).to_string(),
            baseline: baseline.unwrap_or(NONE).to_string(),
        }
    }
}

/// What adding (`add`) or removing a blocking link does, with `current_has`
/// whether the tracker holds it and `other_relation` any other relation the
/// pair holds on Linear, where a pair holds one relation and a new one
/// replaces it.
pub fn block_verdict(current_has: bool, other_relation: Option<&str>, add: bool) -> Verdict {
    match other_relation {
        _ if current_has == add => Verdict::Already,
        Some(other) => Verdict::Conflict {
            current: other.to_string(),
            baseline: if add { NONE } else { "blocks" }.to_string(),
        },
        None => Verdict::Write,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(from: &[&str], to: &str) -> EventTransition {
        EventTransition {
            from: from.iter().map(|s| s.to_string()).collect(),
            to: to.into(),
        }
    }

    fn named(name: &str) -> TicketStatus {
        TicketStatus::Named(name.into())
    }

    fn conflict(current: &str, baseline: &str) -> Verdict {
        Verdict::Conflict {
            current: current.into(),
            baseline: baseline.into(),
        }
    }

    const LINEAR: TrackerKind = TrackerKind::Linear;
    const GITHUB: TrackerKind = TrackerKind::Github;

    #[test]
    fn already_at_to_is_already() {
        let done = event(&["Todo"], "Done");
        let v = status_verdict(
            LocalStatus::Done,
            Some("In review"),
            &named(" done"),
            Some(&done),
            LINEAR,
        )
        .verdict;
        assert_eq!(v, Verdict::Already);
    }

    #[test]
    fn unmoved_ticket_writes() {
        let done = event(&["*"], "Done");
        let v = status_verdict(
            LocalStatus::Done,
            Some("Todo"),
            &named("todo"),
            Some(&done),
            LINEAR,
        )
        .verdict;
        assert_eq!(v, Verdict::Write);
        let done = event(&["Todo", "In progress"], "Done");
        let v = status_verdict(
            LocalStatus::Done,
            Some("Todo"),
            &named("Todo"),
            Some(&done),
            LINEAR,
        )
        .verdict;
        assert_eq!(v, Verdict::Write);
    }

    #[test]
    fn unmoved_ticket_outside_named_from_does_not_move() {
        let start = event(&["Todo"], "In progress");
        let v = status_verdict(
            LocalStatus::Start,
            Some("Backlog"),
            &named("Backlog"),
            Some(&start),
            LINEAR,
        )
        .verdict;
        assert_eq!(v, Verdict::NoMove);
    }

    #[test]
    fn tracker_move_inside_named_from_writes() {
        let done = event(&["Todo", "In review"], "Done");
        let v = status_verdict(
            LocalStatus::Done,
            Some("Todo"),
            &named("In review"),
            Some(&done),
            LINEAR,
        )
        .verdict;
        assert_eq!(v, Verdict::Write);
    }

    #[test]
    fn tracker_move_with_star_from_conflicts() {
        for from in [&["*"][..], &["*", "Todo"]] {
            let done = event(from, "Done");
            let v = status_verdict(
                LocalStatus::Done,
                Some("Todo"),
                &named("Backlog"),
                Some(&done),
                LINEAR,
            )
            .verdict;
            assert_eq!(v, conflict("Backlog", "Todo"), "from {from:?}");
        }
    }

    #[test]
    fn tracker_move_outside_from_conflicts() {
        let done = event(&["Todo", "In review"], "Done");
        let v = status_verdict(
            LocalStatus::Done,
            Some("Todo"),
            &named("Backlog"),
            Some(&done),
            LINEAR,
        )
        .verdict;
        assert_eq!(v, conflict("Backlog", "Todo"));
        let v = status_verdict(
            LocalStatus::Done,
            None,
            &named("Todo"),
            Some(&event(&["*"], "Done")),
            LINEAR,
        )
        .verdict;
        assert_eq!(v, conflict("Todo", "(none)"));
    }

    #[test]
    fn github_closed_completed_is_already_for_done() {
        let closed = TicketStatus::Closed(CloseReason::Completed);
        let done = event(&["*"], "Done");
        for event in [None, Some(&done)] {
            let v = status_verdict(LocalStatus::Done, Some("Todo"), &closed, event, GITHUB).verdict;
            assert_eq!(v, Verdict::Already);
        }
        let not_planned = TicketStatus::Closed(CloseReason::NotPlanned);
        let v = status_verdict(
            LocalStatus::Cancel,
            Some("Todo"),
            &not_planned,
            None,
            GITHUB,
        )
        .verdict;
        assert_eq!(v, Verdict::Already);
    }

    #[test]
    fn github_closed_not_planned_conflicts_for_done() {
        let not_planned = TicketStatus::Closed(CloseReason::NotPlanned);
        let done = event(&["*"], "Done");
        for event in [None, Some(&done)] {
            let v = status_verdict(LocalStatus::Done, Some("Todo"), &not_planned, event, GITHUB)
                .verdict;
            assert_eq!(v, conflict("closed:not_planned", "Todo"));
        }
        let completed = TicketStatus::Closed(CloseReason::Completed);
        let v = status_verdict(LocalStatus::Cancel, Some("Todo"), &completed, None, GITHUB).verdict;
        assert_eq!(v, conflict("closed:completed", "Todo"));
    }

    #[test]
    fn github_open_without_event_writes() {
        for local in [LocalStatus::Done, LocalStatus::Cancel] {
            let v = status_verdict(local, Some("Todo"), &named("Backlog"), None, GITHUB).verdict;
            assert_eq!(v, Verdict::Write, "{local:?}");
        }
    }

    #[test]
    fn github_open_issue_at_to_is_compared_by_its_field() {
        let done = event(&["Todo"], "Done");
        let v = status_verdict(
            LocalStatus::Done,
            Some("Done"),
            &named("Done"),
            Some(&done),
            GITHUB,
        )
        .verdict;
        assert_eq!(v, Verdict::NoMove);
        let v = status_verdict(
            LocalStatus::Done,
            Some("Todo"),
            &named("Done"),
            Some(&done),
            GITHUB,
        )
        .verdict;
        assert_eq!(v, conflict("Done", "Todo"));
    }

    #[test]
    fn no_event_and_no_close_does_not_move() {
        let v = status_verdict(
            LocalStatus::Start,
            Some("Todo"),
            &named("Todo"),
            None,
            GITHUB,
        )
        .verdict;
        assert_eq!(v, Verdict::NoMove);
    }

    fn writes(close: Option<CloseReason>, field: Option<FieldWrite>, record: &str) -> StatusWrites {
        StatusWrites {
            close,
            field,
            record: (!record.is_empty()).then(|| record.to_string()),
        }
    }

    #[test]
    fn a_write_closes_sets_the_field_and_records_what_it_wrote() {
        let done = event(&["*"], "Done");
        let todo = named("Todo");
        let v = status_verdict(LocalStatus::Done, Some("Todo"), &todo, Some(&done), GITHUB);
        assert_eq!(v.verdict, Verdict::Write);
        let closed = Some(CloseReason::Completed);
        assert_eq!(
            v.writes,
            writes(closed, Some(FieldWrite::Set), "closed:completed")
        );
        let v = status_verdict(LocalStatus::Done, Some("Todo"), &todo, Some(&done), LINEAR);
        assert_eq!(v.writes, writes(None, Some(FieldWrite::Set), "Done"));
        let v = status_verdict(LocalStatus::Cancel, Some("Todo"), &todo, None, GITHUB);
        let closed = Some(CloseReason::NotPlanned);
        assert_eq!(v.writes, writes(closed, None, "closed:not_planned"));
    }

    #[test]
    fn a_conflict_written_over_writes_as_a_write() {
        let done = event(&["Todo"], "Done");
        let v = status_verdict(
            LocalStatus::Done,
            Some("Todo"),
            &named("Backlog"),
            Some(&done),
            GITHUB,
        );
        assert_eq!(v.verdict, conflict("Backlog", "Todo"));
        let closed = Some(CloseReason::Completed);
        assert_eq!(
            v.writes,
            writes(closed, Some(FieldWrite::Set), "closed:completed")
        );
    }

    #[test]
    fn an_issue_already_closed_moves_only_its_field() {
        let closed = TicketStatus::Closed(CloseReason::Completed);
        let done = event(&["Todo"], "Done");
        let v = status_verdict(
            LocalStatus::Done,
            Some("Todo"),
            &closed,
            Some(&done),
            GITHUB,
        );
        assert_eq!(v.verdict, Verdict::Already);
        assert_eq!(
            v.writes,
            writes(None, Some(FieldWrite::Move), "closed:completed")
        );
        let v = status_verdict(LocalStatus::Done, Some("Todo"), &closed, None, GITHUB);
        assert_eq!(v.writes, writes(None, None, "closed:completed"));
        let v = status_verdict(
            LocalStatus::Done,
            Some("Todo"),
            &named("Done"),
            Some(&done),
            LINEAR,
        );
        assert_eq!(v.writes, writes(None, None, "Done"));
    }

    #[test]
    fn no_move_still_closes_and_records_only_the_close() {
        let done = event(&["Todo"], "Done");
        let backlog = named("Backlog");
        let v = status_verdict(
            LocalStatus::Done,
            Some("Backlog"),
            &backlog,
            Some(&done),
            GITHUB,
        );
        assert_eq!(v.verdict, Verdict::NoMove);
        let closed = Some(CloseReason::Completed);
        assert_eq!(v.writes, writes(closed, None, "closed:completed"));
        let v = status_verdict(
            LocalStatus::Done,
            Some("Backlog"),
            &backlog,
            Some(&done),
            LINEAR,
        );
        assert_eq!(v.writes, StatusWrites::default());
    }

    #[test]
    fn parent_third_ticket_conflicts() {
        let v = parent_verdict(Some("o/r#1"), Some("o/r#3"), Some("o/r#2"));
        assert_eq!(v, conflict("o/r#3", "o/r#1"));
        let v = parent_verdict(None, Some("o/r#3"), Some("o/r#2"));
        assert_eq!(v, conflict("o/r#3", "(none)"));
        assert_eq!(parent_verdict(None, None, Some("o/r#2")), Verdict::Write);
    }

    #[test]
    fn parent_already_set_is_already() {
        assert_eq!(
            parent_verdict(None, Some("o/r#2"), Some("o/r#2")),
            Verdict::Already
        );
        assert_eq!(parent_verdict(Some("o/r#2"), None, None), Verdict::Already);
    }

    #[test]
    fn block_with_another_linear_relation_conflicts() {
        assert_eq!(
            block_verdict(false, Some("related"), true),
            conflict("related", "(none)")
        );
        assert_eq!(block_verdict(false, None, true), Verdict::Write);
    }

    #[test]
    fn unblock_of_missing_link_is_already() {
        assert_eq!(block_verdict(false, None, false), Verdict::Already);
        assert_eq!(block_verdict(true, None, false), Verdict::Write);
    }
}
