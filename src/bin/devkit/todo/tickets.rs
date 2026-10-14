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
    node,
    render::TicketWork,
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

/// Whether the ticket key `key` names the tracker issue `issue` a worktree
/// record holds: `SWE-123` names itself, and `owner/repo#65` is issue `65`.
fn names_issue(key: &str, issue: &str) -> bool {
    key.eq_ignore_ascii_case(issue) || key.rsplit_once('#').is_some_and(|(_, n)| n == issue)
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
/// node. A worktree on another machine has no record here, so it is absent.
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
            let issue = record::read_on(&wt.path, &wt.branch)?.tracker_issue()?;
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
    fn a_ticket_key_names_its_issue() {
        assert!(names_issue("SWE-123", "swe-123"));
        assert!(names_issue("o/r#65", "65"));
        assert!(!names_issue("o/r#65", "6"));
        assert!(!names_issue("SWE-123", "SWE-12"));
    }

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
