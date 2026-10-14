//! `devkit todo sync`'s ticket half: the tickets a repository's
//! `[ticket.sync]` rules select, brought into the todo store as one todo per
//! ticket on the repository's node. It only reads the tracker.
//!
//! Under `<state dir>/ticket-sync/`, each repository node has `<node>.last`,
//! touched by every successful sync, `<node>.failed`, holding the last
//! background failure's reason, and `<node>.lock`, held by the background
//! sync running for it.

use std::{
    collections::HashMap,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{Context, Result};
use devkit_common::tracker;
use devkit_config::TicketSyncConfig;
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
    let known = tracker.status_names()?;
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

/// The ticket sync's state files for one repository node.
struct State {
    dir: PathBuf,
    name: String,
}

impl State {
    fn of(node: &str) -> Self {
        let name = node
            .chars()
            .map(|c| match c {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => c,
                _ => '_',
            })
            .collect();
        Self {
            dir: devkit_common::paths::state_dir().join("ticket-sync"),
            name,
        }
    }

    fn path(&self, ext: &str) -> PathBuf {
        self.dir.join(format!("{}.{ext}", self.name))
    }

    /// Whether the file `ext` was written within `age`.
    fn within(&self, ext: &str, age: Duration) -> bool {
        fs::metadata(self.path(ext))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|elapsed| elapsed < age)
    }

    /// Whether a background sync is due: the last one is older than
    /// `max_age_secs` or never ran, and none failed within that age.
    fn due(&self, config: &TicketSyncConfig) -> bool {
        let age = Duration::from_secs(config.max_age_secs);
        !config.rules.is_empty() && !self.within("last", age) && !self.within("failed", age)
    }

    fn succeeded(&self) {
        let _ = fs::create_dir_all(&self.dir);
        let _ = fs::remove_file(self.path("failed"));
        let _ = fs::write(self.path("last"), "");
    }

    fn failed(&self, reason: &str) {
        let _ = fs::create_dir_all(&self.dir);
        let _ = fs::write(self.path("failed"), reason);
    }
}

/// Records a successful sync of `node`'s tickets, so reads leave it be for
/// `[ticket.sync] max_age_secs`.
pub(crate) fn record_success(node: &str) {
    State::of(node).succeeded();
}

/// Starts a background ticket sync from `cwd` when `node`'s is due, and
/// returns at once: a read never waits on the tracker.
pub(crate) fn spawn_if_stale(cwd: &Path, node: &str, config: &TicketSyncConfig) {
    if !State::of(node).due(config) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.args(["todo", "sync", "--tickets-only", "--background"])
        .current_dir(cwd);
    let _ = devkit_common::sys::spawn_background(&mut cmd);
}

/// The background ticket sync [`spawn_if_stale`] starts. It gives up at once
/// when another holds `node`'s lock or the sync is no longer due, and a
/// failure is recorded, holding off the next attempt, rather than returned.
pub(crate) fn sync_in_background(
    store: &impl TodoStore,
    cwd: &Path,
    node: &str,
    config: &TicketSyncConfig,
) -> Result<Option<SyncReport>> {
    let state = State::of(node);
    if !state.due(config) {
        return Ok(None);
    }
    fs::create_dir_all(&state.dir)?;
    let mut lock = devkit_common::store::open_lock(&state.path("lock"))?;
    let _guard = match lock.try_write() {
        Ok(guard) => guard,
        Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    // Another run may have finished between the check above and the lock.
    if !state.due(config) {
        return Ok(None);
    }
    match sync_tickets(store, cwd, node, false) {
        Ok(report) => {
            state.succeeded();
            Ok(report)
        }
        Err(e) => {
            state.failed(&format!("{e:#}"));
            Ok(None)
        }
    }
}
