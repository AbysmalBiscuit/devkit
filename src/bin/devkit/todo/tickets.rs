//! `devkit todo sync`'s ticket half: the tickets a repository's
//! `[ticket.sync]` rules select, brought into the todo store as one todo per
//! ticket on the repository's node. It only reads the tracker.

use std::{collections::HashMap, path::Path};

use anyhow::{Context, Result};
use devkit_common::tracker;
use devkit_todo::{
    Filter, StatusKind, TodoStore,
    ticket::{self, SyncPlan},
};

/// What a ticket sync did.
pub(crate) struct SyncReport {
    pub added: usize,
    pub changed: usize,
}

/// Syncs the tickets the rules of the repository at `cwd` select into
/// `node`, its repository node, and returns what changed; `None` when the
/// repository has no `[ticket.sync]` rules, which queries no tracker. With
/// `dry_run` it prints what would change and writes nothing.
///
/// Every ticket a rule selects is fetched, and so is each ticket whose todo
/// is still pending or in progress, so a close or a reassignment reaches its
/// todo. Adds land before edits, which name todos by ticket.
pub(crate) fn sync_tickets(
    store: &impl TodoStore,
    cwd: &Path,
    node: &str,
    dry_run: bool,
) -> Result<Option<SyncReport>> {
    let selected = tracker::select(None, &cwd.to_string_lossy(), None);
    let Some(config) = selected.config.filter(|c| !c.ticket.sync.rules.is_empty()) else {
        return Ok(None);
    };
    let sync = &config.ticket.sync;
    let tracker = &selected.tracker.tracker;
    let kind = tracker.kind().to_string();
    let existing = store.list(&Filter::exact([node.to_string()]))?;
    let mut fetched = tracker
        .find_tickets(&sync.rules, &sync.closed_names())
        .with_context(|| format!("reading {kind} tickets"))?;
    let open: Vec<String> = existing
        .iter()
        .filter(|t| {
            matches!(
                t.status.kind(),
                StatusKind::Pending | StatusKind::InProgress
            )
        })
        .filter_map(|t| t.ticket.as_ref().filter(|r| r.tracker == kind))
        .map(|r| r.key.clone())
        .filter(|key| !fetched.iter().any(|t| &t.key == key))
        .collect();
    if !open.is_empty() {
        fetched.extend(
            tracker
                .tickets(&open)
                .with_context(|| format!("reading {kind} tickets"))?,
        );
    }
    let known = tracker.status_names(&fetched)?;
    let plan = ticket::plan(&kind, node, &existing, &fetched, &known, sync)?;
    if dry_run {
        match plan.is_empty() {
            true => println!("nothing to change"),
            false => print!("{}", plan.render()),
        }
        return Ok(Some(SyncReport {
            added: 0,
            changed: 0,
        }));
    }
    let ids = existing
        .iter()
        .filter_map(|t| {
            let ticket = t.ticket.as_ref().filter(|r| r.tracker == kind)?;
            Some((ticket.key.clone(), t.id.clone()))
        })
        .collect();
    apply(store, &plan, ids).map(Some)
}

/// Lands `plan`'s adds, then its edits, each ticket key resolved through
/// `ids` and the ids the adds return.
fn apply(
    store: &impl TodoStore,
    plan: &SyncPlan,
    mut ids: HashMap<String, String>,
) -> Result<SyncReport> {
    for add in &plan.adds {
        let key = add
            .ticket
            .as_ref()
            .map(|t| t.key.clone())
            .context("a planned add without a ticket")?;
        let id = store.add(add.clone())?;
        ids.insert(key, id);
    }
    for edit in &plan.edits {
        store.apply(&edit.resolve(&ids)?)?;
    }
    let mut changed: Vec<&str> = plan.edits.iter().map(|e| e.key.as_str()).collect();
    changed.sort();
    changed.dedup();
    Ok(SyncReport {
        added: plan.adds.len(),
        changed: changed
            .into_iter()
            .filter(|key| {
                !plan
                    .adds
                    .iter()
                    .any(|a| a.ticket.as_ref().is_some_and(|t| t.key == *key))
            })
            .count(),
    })
}
