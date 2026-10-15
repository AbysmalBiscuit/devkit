//! A ticket todo's `start`, `done` or `cancel` reaching its ticket before the
//! command returns, with `"status"` in `[ticket.sync] push`: the ticket is
//! read, [`status_verdict`] decides, the tracker is written, and only then
//! the todo changes. Only the `devkit todo` verbs come here, so no hook waits
//! on the tracker.

use std::{
    io::{IsTerminal as _, Write as _},
    path::Path,
};

use anyhow::{Context, Result, bail};
use devkit_common::{
    caller::Caller,
    tracker::{
        self, github_status,
        status::{self, StatusWriter as _},
        tickets::{CloseReason, TicketStatus},
    },
};
use devkit_config::{IssueEvent, PushKind, TrackerKind};
use devkit_todo::{
    Edit, Holder, Todo, TodoStore as _,
    push::{LocalStatus, Verdict, status_verdict},
    ticket::status_name,
};

use super::store::Store;

/// A status verb's push: the change, and how a conflict is settled.
pub(crate) struct StatusPush<'a> {
    pub local: LocalStatus,
    pub overwrite: bool,
    pub caller: Caller,
    pub actor: &'a Holder,
    /// The checkout the command runs in, whose config decides the push.
    pub cwd: &'a Path,
}

impl StatusPush<'_> {
    /// Whether `todo`'s change goes through [`push_status`]: a ticket todo,
    /// in a repository pushing statuses, changed from a checkout of that
    /// repository. Any other todo commits as it always has.
    pub(crate) fn pushes(&self, store: &Store, todo: &Todo) -> bool {
        todo.ticket.is_some()
            && store.ticket().sync.pushes(PushKind::Status)
            && store.origin().is_some_and(|origin| {
                super::ticket_event::root_on(origin, todo.node(), store.config(), self.actor)
                    .is_some()
            })
    }

    fn event(&self) -> IssueEvent {
        match self.local {
            LocalStatus::Start => IssueEvent::TodoStart,
            LocalStatus::Done => IssueEvent::TodoDone,
            LocalStatus::Cancel => IssueEvent::TodoCancel,
        }
    }

    fn word(&self) -> &'static str {
        match self.local {
            LocalStatus::Start => "start",
            LocalStatus::Done => "done",
            LocalStatus::Cancel => "cancel",
        }
    }
}

/// Moves `todo`'s ticket as `push` asks, then applies `edit`, the todo's own
/// change. Returns whether `edit` was applied: a person who declines to write
/// over a tracker change gets the tracker's value instead. A failed read or
/// write, or a conflict nobody settled, is an error naming the ticket, with
/// nothing changed on either side.
pub(crate) fn push_status(
    store: &Store,
    todo: &Todo,
    edit: &Edit,
    push: &StatusPush,
) -> Result<bool> {
    let key = &todo.ticket.as_ref().context("not a ticket todo")?.key;
    let short = devkit_todo::short_id(&todo.id);
    let unchanged = || format!("ticket {key}: todo {short} is unchanged");
    let selected = tracker::select(None, &push.cwd.to_string_lossy(), None);
    let config = selected.config.unwrap_or_default();
    let tracker = &selected.tracker.tracker;
    let github = tracker.kind() == TrackerKind::Github;
    let event = push.event();
    let transition = config.ticket.events.get(event);
    let closes = match push.local {
        LocalStatus::Done if github => Some(CloseReason::Completed),
        LocalStatus::Cancel if github => Some(CloseReason::NotPlanned),
        _ => None,
    };
    if transition.is_none() && closes.is_none() {
        store.apply_unfired(edit)?;
        return Ok(true);
    }
    // A close needs no `[github] project`, so a missing one only skips the
    // event's move, and says so once the close has landed.
    let (writer, unwritable) = match transition
        .map(|_| status::writer_for(tracker.kind(), &config.github, &selected.forge.repos))
    {
        Some(Ok(writer)) => (Some(writer), None),
        Some(Err(e)) if closes.is_some() => (None, Some(e)),
        Some(Err(e)) => return Err(e.context(unchanged())),
        None => (None, None),
    };
    let current = tracker
        .tickets(std::slice::from_ref(key))
        .with_context(unchanged)?
        .into_iter()
        .find(|t| &t.key == key)
        .with_context(|| format!("{} has no ticket {key} it can read", tracker.kind()))
        .with_context(unchanged)?
        .status;
    let baseline = todo
        .ticket
        .as_ref()
        .and_then(|t| t.synced_status.as_deref());
    let verdict = status_verdict(push.local, baseline, &current, transition, github);
    let write = match &verdict {
        Verdict::Write => true,
        Verdict::Already | Verdict::NoMove => false,
        Verdict::Conflict { .. } if push.overwrite => true,
        Verdict::Conflict { current, baseline } => {
            let values = format!(
                "tracker {current:?}, last synced {baseline:?}, local {}",
                push.word()
            );
            if push.caller != Caller::Human || !std::io::stdin().is_terminal() {
                bail!(
                    "{key} changed on the tracker since the last sync: {values}. Ask the user; \
                     rerun with --overwrite-tracker to write over it, or run `devkit todo sync \
                     --ticket {key}` to take the tracker's value."
                );
            }
            if !confirm(&format!("{key}: {values}. Write over the tracker?")) {
                let node = todo.node().to_string();
                super::tickets::sync_one(store, push.cwd, &node, key)?;
                eprintln!("devkit todo: {key} left as the tracker has it");
                return Ok(false);
            }
            true
        }
    };
    let closing = closes.filter(|_| write || verdict == Verdict::NoMove);
    if let Some(reason) = closing {
        let repo = selected.forge.repos.issues().with_context(unchanged)?;
        github_status::close_issue(repo, github_status::number(key, &repo.slug)?, reason)
            .with_context(unchanged)?;
        if let Some(e) = &unwritable {
            eprintln!("devkit todo: ticket {key} closed; [ticket.events.{event}] not moved: {e:#}");
        }
    }
    if let (Some(writer), Some(t)) = (&writer, transition) {
        let field = match (write, &verdict) {
            (true, _) => Some(status::set_status(writer, key, event, &t.to)),
            (false, Verdict::Already) if closes.is_some() => {
                Some(writer.move_status(key, event, t))
            }
            _ => None,
        };
        if let Some(moved) = field {
            moved.with_context(|| match closing {
                Some(_) => format!("ticket {key} closed, but its [ticket.events.{event}] move failed; todo {short} is unchanged"),
                None => unchanged(),
            })?;
        }
    }
    let recorded = match (closing, write, &verdict) {
        (Some(reason), ..) => Some(status_name(&TicketStatus::Closed(reason))),
        (None, true, _) => transition.map(|t| t.to.clone()),
        (None, false, Verdict::Already) => Some(status_name(&current)),
        (None, false, _) => None,
    };
    if verdict == Verdict::NoMove {
        let now = status_name(&current);
        let moved = if closing.is_some() {
            "closed; its status"
        } else {
            "not moved:"
        };
        eprintln!(
            "devkit todo: ticket {key} {moved} {now:?} is not in [ticket.events.{event}] from"
        );
    }
    store.apply_unfired(edit)?;
    if let Some(synced_status) = recorded {
        store.apply(&Edit::RecordSync {
            id: todo.id.clone(),
            synced_status,
        })?;
    }
    Ok(true)
}

fn confirm(question: &str) -> bool {
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}
