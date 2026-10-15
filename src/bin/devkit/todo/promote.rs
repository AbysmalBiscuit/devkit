//! `devkit todo promote`: a todo filed as a ticket, or linked to one.
//!
//! The ticket's todo comes from a one-ticket sync, every blocking edge of the
//! old todo moves to it, and the old todo is cancelled with the ticket's key
//! as its text. Its children stay where they are.

use anyhow::{Context, Result, bail};
use devkit_common::{record, vcs::Checkout};
use devkit_config::PushKind;
use devkit_todo::{
    Edit, Filter, Holder, StatusKind, Todo, TodoStore as _, graph, layout::Layout, transition,
};

use super::{push, store::Store, tickets};
use crate::{
    issue::create::{self, CreateArgs},
    template::{FieldArgs, VarArgs},
};

/// Where a promote runs and whom for.
pub(crate) struct Promote<'a> {
    pub store: &'a Store,
    /// The checkout the command runs in, a checkout of the ticket's
    /// repository.
    pub checkout: &'a Checkout,
    pub layout: &'a Layout,
    /// The repository node the ticket's todo lands on.
    pub node: &'a str,
    pub actor: &'a Holder,
    pub push: &'a push::LinkPush<'a>,
}

/// What a promote files beside the todo's text, as `ticket create` takes it.
pub(crate) struct Filing {
    pub body: Option<String>,
    pub vars: VarArgs,
    pub fields: FieldArgs,
    /// The directory the command runs in.
    pub dir: String,
}

/// What to promote `id` to: `ticket`, an existing ticket's key, or else a
/// ticket filed from `filing` with the todo's text as its title.
pub(crate) fn promote(
    at: &Promote,
    id: &str,
    ticket: Option<&str>,
    filing: Filing,
) -> Result<()> {
    let store = at.store;
    let old = super::resolve_id(store, id)?;
    let short = devkit_todo::short_id(&old.id).to_string();
    if let Some(t) = &old.ticket {
        bail!(
            "todo {short} is already ticket {}; promote takes a todo without a ticket",
            t.key
        );
    }
    transition(&old.status, StatusKind::Cancelled, at.actor)?;
    if store.ticket().sync.rules.is_empty() {
        bail!(
            "todo {short} not promoted: a ticket's todo comes from syncing it, and this \
             repository has no [ticket.sync] rules"
        );
    }
    if !store.replica_pulled() {
        bail!("todo {short} not promoted: the store's sync did not complete");
    }
    let (key, fields) = match ticket {
        Some(key) => (key.to_string(), Ok(())),
        None => {
            let created = create::create(&CreateArgs {
                title: old.description.clone(),
                body: filing.body,
                vars: filing.vars,
                fields: filing.fields,
                dir: Some(filing.dir),
                config: None,
                verb: "todo promote",
                likely_parents: likely_parents(at),
                if_found: Some(format!("devkit todo promote {short} --ticket <number>")),
            })?;
            (created.key, created.fields)
        }
    };
    let linked = link(at, &old, ticket, key);
    if linked.is_err()
        && let Err(e) = &fields
    {
        eprintln!("devkit todo: {e:#}");
    }
    linked.and(fields)
}

/// Links `old` to ticket `key`: syncs the ticket's todo, moves `old`'s
/// blocking edges to it, and cancels `old` with the key as its text.
/// `ticket` is the key the caller named, absent when the promote filed it.
fn link(at: &Promote, old: &Todo, ticket: Option<&str>, key: String) -> Result<()> {
    let store = at.store;
    let short = devkit_todo::short_id(&old.id);
    let unfinished = || {
        format!(
            "ticket {key} stands, and todo {short} is not promoted yet: finish with `devkit todo \
             promote {short} --ticket {key}`"
        )
    };
    let new = tickets::sync_one(store, at.checkout.dir(), at.node, &key)
        .and_then(|todo| todo.with_context(|| format!("no todo synced for {key}")))
        .with_context(|| match ticket {
            Some(_) => format!("todo {short} is unchanged"),
            None => unfinished(),
        })?;
    rewire(at, old, &new).with_context(unfinished)?;
    let key = new.ticket.as_ref().map_or(key, |t| t.key.clone());
    store.apply(&Edit::Describe {
        id: old.id.clone(),
        description: key.clone(),
    })?;
    store.apply(&Edit::SetStatus {
        id: old.id.clone(),
        to: StatusKind::Cancelled,
        actor: at.actor.clone(),
    })?;
    println!(
        "devkit todo: todo {short} is now ticket {key}, todo {}",
        devkit_todo::short_id(&new.id)
    );
    Ok(())
}

/// Moves every blocking edge of `old` to `new`: each todo `old` blocks is
/// blocked by `new` instead, and `old`'s own blockers block `new` too.
fn rewire(at: &Promote, old: &Todo, new: &Todo) -> Result<()> {
    let store = at.store;
    let dependents: Vec<Todo> = store
        .list(&Filter::all())?
        .into_iter()
        .filter(|t| t.blocked_by.contains(&old.id))
        .collect();
    for dependent in dependents {
        if dependent.id != new.id {
            block(at, &dependent, new)?;
        }
        store.apply(&Edit::Unblock {
            id: dependent.id,
            by: vec![old.id.clone()],
        })?;
    }
    for blocker in &old.blocked_by {
        let Some(blocker) = store.get(blocker)?.filter(|b| b.id != new.id) else {
            continue;
        };
        let new = super::resolve_id(store, &new.id)?;
        block(at, &new, &blocker)?;
    }
    Ok(())
}

/// Records that `blocker` blocks `blocked`. An edge between two tickets of
/// one tracker reaches it where the repository pushes blocking links;
/// elsewhere it is local, and the next sync drops it. An edge that would
/// close a cycle is skipped with a warning, so the rest of the promote
/// still lands.
fn block(at: &Promote, blocked: &Todo, blocker: &Todo) -> Result<()> {
    if blocked.blocked_by.contains(&blocker.id) {
        return Ok(());
    }
    let todos = at.store.list(&Filter::all())?;
    if let Some(cycle) =
        graph::cycle_through(&todos, &blocked.id, std::slice::from_ref(&blocker.id))
    {
        let cycle = cycle
            .iter()
            .map(|id| devkit_todo::short_id(id))
            .collect::<Vec<_>>()
            .join(" -> ");
        eprintln!(
            "devkit todo: {} blocks {} skipped: it would close the cycle {cycle}",
            name(blocker),
            name(blocked)
        );
        return Ok(());
    }
    let tickets = match (&blocked.ticket, &blocker.ticket) {
        (Some(a), Some(b)) if a.same_tracker(b) => Some((&a.key, &b.key)),
        _ => None,
    };
    if tickets.is_some() && at.push.pushes(at.store, PushKind::Blocking, blocked) {
        return push::push_block(at.store, blocked, blocker, true, at.push).map(drop);
    }
    at.store.apply(&Edit::Block {
        id: blocked.id.clone(),
        by: vec![blocker.id.clone()],
    })?;
    if let Some((key, by)) = tickets {
        eprintln!(
            "devkit todo: {by} blocks {key} in the todo list only: without \"blocking\" in \
             [ticket.sync] push the next sync drops the link, so add it on the tracker"
        );
    }
    Ok(())
}

/// A todo as a message names it: its ticket's key, else its short id.
fn name(todo: &Todo) -> &str {
    todo.ticket
        .as_ref()
        .map_or_else(|| devkit_todo::short_id(&todo.id), |t| t.key.as_str())
}

/// The values a promoted ticket's parent likely takes: the ticket this
/// checkout's `workspace setup` record names, and that ticket's parent.
fn likely_parents(at: &Promote) -> Vec<(String, &'static str)> {
    let todos = at.store.list(&Filter::all()).unwrap_or_default();
    let synced = tickets::ticket_workspaces(at.checkout, at.layout, &todos)
        .into_iter()
        .find(|w| w.here)
        .and_then(|w| todos.iter().find(|t| t.id == w.todo));
    let Some(ticket) = synced else {
        return at
            .checkout
            .here()
            .and_then(|wt| record::read_on(&wt.path, &wt.branch))
            .filter(|r| r.origin != Some(record::RecordOrigin::Checkout))
            .and_then(|r| r.tracker_issue())
            .map(|issue| (issue, "this workspace's ticket"))
            .into_iter()
            .collect();
    };
    let key = |t: &Todo| t.ticket.as_ref().map(|r| r.key.clone());
    let parent = ticket
        .parent
        .as_ref()
        .and_then(|p| todos.iter().find(|t| &t.id == p))
        .and_then(key);
    key(ticket)
        .map(|k| (k, "this workspace's ticket"))
        .into_iter()
        .chain(parent.map(|p| (p, "its parent")))
        .collect()
}
