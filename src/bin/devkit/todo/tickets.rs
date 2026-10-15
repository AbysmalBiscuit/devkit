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
use devkit_common::{record, tracker, vcs::Checkout};
use devkit_config::TicketSyncConfig;
use devkit_todo::{
    Filter, Holder, StatusKind, Todo, TodoStore,
    layout::{Facts, Layout},
    names::names_issue,
    node,
    render::TicketWork,
    ticket::{self, Change, SyncPlan},
};

use super::store::Store;

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
    store: &Store,
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
    apply_plan(store, &kind, &existing, &plan).map(Some)
}

/// `devkit todo sync --ticket`'s sync of `key` into `node`: [`sync_one`],
/// refused while the store's own sync has not completed, and saying on stderr
/// when the repository has no `[ticket.sync]` rules. Returns the key of the
/// ticket synced, as the tracker spells it.
pub(crate) fn sync_ticket(
    store: &Store,
    cwd: &Path,
    node: &str,
    key: &str,
) -> Result<Option<String>> {
    if !store.replica_pulled() {
        anyhow::bail!("ticket {key} not synced: the store's sync did not complete");
    }
    let synced = sync_one(store, cwd, node, key)?.and_then(|todo| todo.ticket.map(|t| t.key));
    if synced.is_none() {
        eprintln!("devkit todo: no [ticket.sync] rules here; ticket {key} not synced");
    }
    Ok(synced)
}

/// After `ticket create` filed `key` from `dir`: syncs it into the
/// repository node of the checkout at `dir`, which gives it its todo.
/// Nothing syncs outside a repository or in one without `[ticket.sync]`
/// rules.
pub(crate) fn sync_created(dir: &Path, key: &str) -> Result<()> {
    let checkout = Checkout::at(dir);
    let place = node::place_of(&checkout)?;
    if place == node::Place::Global {
        return Ok(());
    }
    let cwd = checkout.dir();
    let store = Store::for_cli(cwd)?;
    if store.ticket().sync.rules.is_empty() {
        return Ok(());
    }
    let actor = super::actor_from_env(devkit_common::caller::caller(), |key| {
        std::env::var(key).ok()
    });
    let node = Layout::new(store.config())?.fill("repo", &Facts::new(&place, None, &actor))?;
    sync_ticket(&store, cwd, &node, key)?;
    super::after_write(&store, cwd);
    Ok(())
}

/// Syncs the one ticket `key`, in any spelling [`tracker::Tracker::ticket_key`]
/// reads, into `node`, the repository node of the
/// checkout at `cwd`, reading no other ticket and running no rule search,
/// and returns its todo; `None` when the repository has no `[ticket.sync]`
/// rules, which queries no tracker. The ticket's parent and blockers link
/// to the todos already synced for them.
pub(crate) fn sync_one(
    store: &Store,
    cwd: &Path,
    node: &str,
    key: &str,
) -> Result<Option<Todo>> {
    let selected = tracker::select(None, &cwd.to_string_lossy(), None);
    let Some(config) = selected.config.filter(|c| !c.ticket.sync.rules.is_empty()) else {
        return Ok(None);
    };
    let tracker = &selected.tracker.tracker;
    let kind = tracker.kind().to_string();
    let key = tracker.ticket_key(key)?;
    let key = key.as_str();
    let fetched: Vec<_> = tracker
        .tickets(&[key.to_string()])
        .with_context(|| format!("reading {kind} ticket {key}"))?
        .into_iter()
        .filter(|t| t.key == key)
        .collect();
    anyhow::ensure!(!fetched.is_empty(), "{kind} has no ticket {key}");
    let existing = store.list(&Filter::exact([node.to_string()]))?;
    let known = tracker.status_names()?;
    let plan = ticket::plan(
        &kind,
        node,
        &existing,
        &fetched,
        &known,
        &config.ticket.sync,
    )?;
    apply_plan(store, &kind, &existing, &plan)?;
    Ok(store
        .list(&Filter::exact([node.to_string()]))?
        .into_iter()
        .find(|t| {
            t.ticket
                .as_ref()
                .is_some_and(|r| r.tracker == kind && r.key == key)
        }))
}

/// Lands `plan`, made from `existing`, the todos on its node.
fn apply_plan(
    store: &Store,
    kind: &str,
    existing: &[Todo],
    plan: &SyncPlan,
) -> Result<SyncReport> {
    let planned_on: HashMap<String, &Todo> = existing
        .iter()
        .filter_map(|t| {
            let ticket = t.ticket.as_ref().filter(|r| r.tracker == kind)?;
            Some((ticket.key.clone(), t))
        })
        .collect();
    apply(store, plan, &planned_on)
}

/// Lands `plan`'s adds, then its edits, each ticket key resolved through
/// `planned_on`, the todos the plan was made from, and the ids the adds
/// return. A status or description edit whose todo changed that field since
/// `planned_on` was read is skipped: the change made while the tracker
/// answered wins.
fn apply(
    store: &Store,
    plan: &SyncPlan,
    planned_on: &HashMap<String, &Todo>,
) -> Result<SyncReport> {
    let mut ids: HashMap<String, String> = planned_on
        .iter()
        .map(|(key, todo)| (key.clone(), todo.id.clone()))
        .collect();
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
        let resolved = edit.resolve(&ids)?;
        if let Some(seen) = planned_on.get(&edit.key)
            && moved_since(store, seen, &edit.change)?
        {
            continue;
        }
        store.apply(&resolved)?;
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

/// Whether the todo `seen` describes now holds another value in `store` for
/// the status or description `change` sets, or is gone; false for a change
/// that sets neither.
fn moved_since(store: &Store, seen: &Todo, change: &Change) -> Result<bool> {
    let field_moved: fn(&Todo, &Todo) -> bool = match change {
        Change::SetStatus(_) => |now, seen| now.status != seen.status,
        Change::Describe(_) => |now, seen| now.description != seen.description,
        Change::RecordSync(_) | Change::Move(_) | Change::Block(_) | Change::Unblock(_) => {
            return Ok(false);
        }
    };
    Ok(store
        .get_exact(&seen.id)?
        .is_none_or(|now| field_moved(&now, seen)))
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
        // Rewriting an empty file leaves its mtime alone on NTFS, so stamp it.
        let _ = fs::File::create(self.path("last"))
            .and_then(|last| last.set_modified(std::time::SystemTime::now()));
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
    store: &Store,
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

/// A synced ticket's todo that a worktree of this repository records as its
/// ticket, the link `workspace setup` makes.
#[derive(Clone)]
pub(crate) struct TicketWorkspace {
    /// The ticket todo's id.
    pub todo: String,
    /// The worktree's workspace node, where the ticket's work is filed.
    pub workspace: String,
    /// Whether the worktree is the one `checkout` was resolved in.
    pub here: bool,
}

/// The open ticket todo in `todos` that `text`, a harness task's subject or
/// plan step, names: by its ticket key (`SWE-123`, `owner/repo#65`), by its
/// number after a `#` (`#65`), or by the todo's own description.
pub(crate) fn named_ticket<'a>(todos: &'a [Todo], text: &str) -> Option<&'a Todo> {
    let text = devkit_todo::one_line(text);
    let text = text.trim();
    let names = |key: &str| match text.strip_prefix('#') {
        Some(number) => names_issue(key, number),
        None => key.eq_ignore_ascii_case(text),
    };
    let open = |t: &&Todo| t.status.kind().is_open();
    let tickets = || todos.iter().filter(open).filter(|t| t.ticket.is_some());
    tickets()
        .find(|t| t.ticket.as_ref().is_some_and(|r| names(&r.key)))
        .or_else(|| tickets().find(|t| t.description == text))
}

/// Each worktree of `checkout`'s repository whose record names the ticket of
/// a todo in `todos` on the repository node, with that worktree's workspace
/// node; a `pr checkout` record names none. A worktree on another machine has
/// no record here, so it is absent.
pub(crate) fn ticket_workspaces(
    checkout: &Checkout,
    layout: &Layout,
    todos: &[Todo],
) -> Vec<TicketWorkspace> {
    let worktrees = checkout.worktrees();
    let here = checkout.here().map(|h| h.path.as_path());
    worktrees
        .iter()
        .filter(|wt| !wt.bare)
        .filter_map(|wt| {
            let record = record::read_on(&wt.path, &wt.branch)?;
            // A reviewer's `pr checkout` is not where the ticket's work
            // happens.
            if record.origin == Some(record::RecordOrigin::Checkout) {
                return None;
            }
            let issue = record.tracker_issue()?;
            let place = node::place_from(worktrees.first(), Some(wt));
            let facts = Facts::new(&place, None, &Holder::human());
            let repo = layout.fill("repo", &facts).ok()?;
            let workspace = layout.ticket_root(&facts, false)?;
            let todo = todos.iter().find(|t| {
                t.node() == repo
                    && t.ticket
                        .as_ref()
                        .is_some_and(|r| names_issue(&r.key, &issue))
            })?;
            Some(TicketWorkspace {
                todo: todo.id.clone(),
                workspace,
                here: here == Some(wt.path.as_path()),
            })
        })
        .collect()
}

/// The open work under each of `workspaces`' tickets among `todos`: its
/// count, and with `expand` the todos themselves.
pub(crate) fn ticket_work(
    workspaces: &[&TicketWorkspace],
    todos: &[Todo],
    expand: bool,
) -> TicketWork {
    let mut work = TicketWork::default();
    for ws in workspaces {
        let inside = format!("{}.", ws.workspace);
        let open: Vec<Todo> = todos
            .iter()
            .filter(|t| t.node() == ws.workspace || t.node().starts_with(&inside))
            .filter(|t| t.status.kind().is_open())
            .cloned()
            .collect();
        *work.open.entry(ws.todo.clone()).or_default() += open.len();
        if expand {
            work.expanded
                .entry(ws.todo.clone())
                .or_default()
                .extend(open);
        }
    }
    work
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_names_an_open_ticket_by_key_number_or_text() {
        let todo = |id: &str, key: &str, status: &str| -> Todo {
            serde_json::from_value(serde_json::json!({
                "id": id,
                "description": format!("{key} Issue"),
                "status": status,
                "ticket": {"tracker": "github", "key": key},
            }))
            .unwrap()
        };
        let todos = [
            todo("a", "o/r#65", "pending"),
            todo("b", "SWE-7", "pending"),
            todo("c", "o/r#9", "completed"),
        ];
        let named = |text| named_ticket(&todos, text).map(|t| t.id.as_str());
        assert_eq!(named("#65"), Some("a"));
        assert_eq!(named("  O/R#65 "), Some("a"));
        assert_eq!(named("swe-7"), Some("b"));
        assert_eq!(named("SWE-7  Issue"), Some("b"));
        assert_eq!(named("65"), None);
        assert_eq!(named("#9"), None);
        assert_eq!(named(""), None);
        assert_eq!(named("#"), None);
        assert_eq!(named("fix #65 later"), None);
    }
}
