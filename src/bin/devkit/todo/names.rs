//! The caller's tracker as todo names read it: the resolver reading typed
//! names back, and listings showing each ticket todo's key.

use std::path::Path;

use devkit_common::tracker::{self, TrackerKind};
use devkit_config::IdStyle;
pub(crate) use devkit_todo::names::Names;
use devkit_todo::{TicketRef, Todo, names::TrackerKey, ticket::key_prefix};

/// The [`TrackerKey`] of the checkout at `cwd`, reading a GitHub issue's
/// short form or URL against the issues repository it names; `None` when its
/// tracker is not GitHub.
pub(crate) fn tracker_key(cwd: &Path) -> Option<Box<TrackerKey>> {
    let tracker = tracker::select(None, &cwd.to_string_lossy(), None)
        .tracker
        .tracker;
    (tracker.kind() == TrackerKind::Github)
        .then(|| Box::new(move |input: &str| tracker.ticket_key(input).ok()) as Box<TrackerKey>)
}

/// The names `todos` show under `style` for a caller at `cwd`. A GitHub key
/// shows as `#N` where the caller's tracker reads `#N` back to it, so every
/// shown key resolves from where it was shown; any other key shows whole.
pub(crate) fn names(style: IdStyle, todos: &[Todo], cwd: &Path) -> Names {
    let github = todos
        .iter()
        .any(|todo| todo.ticket.as_ref().is_some_and(|t| t.tracker == "github"));
    let read = (github && style != IdStyle::Full)
        .then(|| tracker_key(cwd))
        .flatten();
    Names::new(style, todos, |ticket| shown_key(ticket, read.as_deref()))
}

fn shown_key(ticket: &TicketRef, read: Option<&TrackerKey>) -> String {
    let short = key_prefix(&ticket.tracker, &ticket.key);
    let reads_back = ticket.tracker == "github"
        && read
            .and_then(|read| read(&short))
            .is_some_and(|key| key.eq_ignore_ascii_case(&ticket.key));
    if reads_back {
        short
    } else {
        ticket.key.clone()
    }
}
