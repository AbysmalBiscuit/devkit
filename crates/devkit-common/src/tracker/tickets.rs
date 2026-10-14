//! Tickets as `devkit todo sync` reads them: the fields a ticket's todo
//! follows, from whichever tracker holds them. Every read here is read-only.

use super::status::same_status;

/// One ticket as a sync reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncedTicket {
    /// The tracker's id for it: `owner/repo#65` on GitHub, `SWE-123` on
    /// Linear.
    pub key: String,
    pub title: String,
    pub status: TicketStatus,
    /// The keys of the tickets blocking this one, in the same tracker scope.
    pub blocked_by: Vec<String>,
    /// The key of the ticket this one is a sub-issue of.
    pub parent: Option<String>,
}

/// Whether `ticket` is closed, or in one of `closed_names`: a ticket no
/// sync rule brings in.
pub(crate) fn in_closed_name(ticket: &SyncedTicket, closed_names: &[String]) -> bool {
    match &ticket.status {
        TicketStatus::Named(name) => closed_names.iter().any(|c| same_status(c, name)),
        TicketStatus::Closed(_) => true,
    }
}

/// Where a ticket stands on its tracker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TicketStatus {
    /// A status the tracker names: a Linear workflow state, or an open GitHub
    /// issue's `[github] status_field` option, `""` when it has none.
    Named(String),
    /// A closed GitHub issue, whatever its project field says.
    Closed(CloseReason),
}

/// Why a GitHub issue was closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    Completed,
    NotPlanned,
    Duplicate,
}
