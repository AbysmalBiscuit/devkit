//! The `todo_start` and `todo_done` ticket events: when a todo `devkit todo
//! sync` brought in for a ticket becomes in progress or completed, the
//! ticket's tracker status moves as `[ticket.events]` configures, in a
//! detached `devkit ticket event` so no todo command waits on the tracker.

use std::{path::Path, process::Command};

use devkit_common::vcs::Checkout;
use devkit_config::{IssueEvent, TicketConfig, TodoConfig};
use devkit_todo::{
    Holder, StatusChange, StatusKind, TodoStore,
    layout::{Facts, Layout},
    node::{self, Place},
};

/// The event a status change fires, if any.
fn event_of(change: &StatusChange) -> Option<IssueEvent> {
    let to = change.to.as_ref()?.kind();
    if to == change.from.kind() {
        return None;
    }
    match to {
        StatusKind::InProgress => Some(IssueEvent::TodoStart),
        StatusKind::Completed => Some(IssueEvent::TodoDone),
        StatusKind::Pending | StatusKind::Cancelled => None,
    }
}

/// The root of `checkout` when its repository node is `node`.
fn root_on(
    checkout: &Checkout,
    node: &str,
    config: &TodoConfig,
    actor: &Holder,
) -> Option<std::path::PathBuf> {
    let place = node::place_of(checkout).ok()?;
    if place == Place::Global {
        return None;
    }
    let repo = Layout::new(config)
        .ok()?
        .fill("repo", &Facts::new(&place, None, actor))
        .ok()?;
    (repo == node).then(|| checkout.here().map(|here| here.path.clone()))?
}

/// Fires the ticket event each of `changes`, made by `actor` from `origin`,
/// calls for. A change `tracker` made fires nothing, so a sync never moves a
/// ticket back. A ticket todo changed from outside its repository fires
/// nothing and says so on stderr: only a checkout of the repository resolves
/// the ticket's config.
pub(crate) fn fire_for(
    changes: &[StatusChange],
    actor: &Holder,
    store: &impl TodoStore,
    config: &TodoConfig,
    ticket: &TicketConfig,
    origin: &Checkout,
) {
    if actor.is_tracker() {
        return;
    }
    for change in changes {
        let Some(event) = event_of(change) else {
            continue;
        };
        let Ok(Some(todo)) = store.get(&change.todo) else {
            continue;
        };
        let Some(key) = todo.ticket.map(|t| t.key) else {
            continue;
        };
        match root_on(origin, &change.node, config, actor) {
            Some(root) if ticket.events.get(event).is_some() => spawn(&root, event, &key),
            Some(_) => {}
            None => eprintln!("devkit todo: ticket {key} not moved: run from its repository"),
        }
    }
}

fn spawn(root: &Path, event: IssueEvent, key: &str) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.arg("ticket")
        .arg("-C")
        .arg(root)
        .args(["event", event.as_str(), key, "--log-file"])
        .arg(devkit_common::paths::state_dir().join("ticket-event.log"))
        .current_dir(root);
    let _ = devkit_common::sys::spawn_background(&mut cmd);
}
