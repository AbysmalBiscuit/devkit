//! What a tracker sync changes in the store, diffed from the tickets a
//! tracker returned and the ticket todos on a repository's node.
//!
//! A ticket todo's status changes only when its ticket's status moved since
//! the last sync, so an agent's start survives a ticket standing still. Only
//! parents and edges joining two ticket todos are the tracker's to change.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Result, bail};
use devkit_common::tracker::{
    status::same_status,
    tickets::{CloseReason, SyncedTicket, TicketStatus},
};
use devkit_config::{MappedStatus, TicketSyncConfig};

use crate::{Edit, Holder, NewTodo, StatusKind, TicketRef, Todo, one_line};

/// The text a ticket's todo carries: `#65 Add sync` on GitHub, `SWE-123 Fix
/// the plate loader` elsewhere.
pub fn todo_text(tracker: &str, key: &str, title: &str) -> String {
    let key = match tracker {
        "github" => format!("#{}", key.rsplit('#').next().unwrap_or(key)),
        _ => key.to_string(),
    };
    one_line(&format!("{key} {title}"))
}

/// The name a ticket todo records as its ticket's last synced status: the
/// tracker's own name, or `closed:<reason>` for a closed GitHub issue.
pub fn status_name(status: &TicketStatus) -> String {
    match status {
        TicketStatus::Named(name) => name.clone(),
        TicketStatus::Closed(CloseReason::Completed) => "closed:completed".into(),
        TicketStatus::Closed(CloseReason::NotPlanned) => "closed:not_planned".into(),
        TicketStatus::Closed(CloseReason::Duplicate) => "closed:duplicate".into(),
    }
}

/// One change to a ticket's todo, naming todos by ticket key, since a
/// ticket added in the same sync has no id until its add lands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// Moves the todo to this status, as the tracker holder.
    SetStatus(StatusKind),
    /// Records this name as the ticket's last synced status.
    RecordSync(String),
    Describe(String),
    /// Nests the todo under the todo of the ticket named, or at the top
    /// level.
    Move(Option<String>),
    Block(Vec<String>),
    Unblock(Vec<String>),
}

/// A [`Change`] to the todo of the ticket `key`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedEdit {
    pub key: String,
    pub change: Change,
}

impl PlannedEdit {
    /// The store edit, with each ticket key resolved through `ids`. A key
    /// with no id is an error naming it.
    pub fn resolve(&self, ids: &HashMap<String, String>) -> Result<Edit> {
        let id_of = |key: &str| {
            ids.get(key)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no todo for ticket {key}"))
        };
        let ids_of = |keys: &[String]| keys.iter().map(|k| id_of(k)).collect::<Result<Vec<_>>>();
        let id = id_of(&self.key)?;
        Ok(match &self.change {
            Change::SetStatus(to) => Edit::SetStatus {
                id,
                to: *to,
                actor: Holder::tracker(),
            },
            Change::RecordSync(name) => Edit::RecordSync {
                id,
                synced_status: name.clone(),
            },
            Change::Describe(text) => Edit::Describe {
                id,
                description: text.clone(),
            },
            Change::Move(parent) => Edit::Move {
                id,
                parent: parent.as_deref().map(id_of).transpose()?,
                order: None,
            },
            Change::Block(keys) => Edit::Block {
                id,
                by: ids_of(keys)?,
            },
            Change::Unblock(keys) => Edit::Unblock {
                id,
                by: ids_of(keys)?,
            },
        })
    }
}

/// Everything one sync changes: the ticket todos to add, then the edits,
/// applied in order once the adds have landed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncPlan {
    pub adds: Vec<NewTodo>,
    pub edits: Vec<PlannedEdit>,
}

impl SyncPlan {
    pub fn is_empty(&self) -> bool {
        self.adds.is_empty() && self.edits.is_empty()
    }

    /// One line per change, for `--dry-run`. Each ticket appears by its key.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for add in &self.adds {
            out.push_str(&format!("add {}\n", add.description));
        }
        for edit in &self.edits {
            let key = &edit.key;
            let line = match &edit.change {
                Change::SetStatus(to) => format!("status {key} -> {}", kind_name(*to)),
                Change::RecordSync(name) => format!("synced {key} as {name:?}"),
                Change::Describe(text) => format!("describe {key}: {text}"),
                Change::Move(Some(parent)) => format!("move {key} under {parent}"),
                Change::Move(None) => format!("move {key} to the top level"),
                Change::Block(keys) => format!("block {key} by {}", keys.join(", ")),
                Change::Unblock(keys) => format!("unblock {key} from {}", keys.join(", ")),
            };
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}

fn kind_name(kind: StatusKind) -> &'static str {
    match kind {
        StatusKind::Pending => "pending",
        StatusKind::InProgress => "in progress",
        StatusKind::Completed => "completed",
        StatusKind::Cancelled => "cancelled",
    }
}

fn mapped_kind(status: MappedStatus) -> StatusKind {
    match status {
        MappedStatus::Pending => StatusKind::Pending,
        MappedStatus::InProgress => StatusKind::InProgress,
        MappedStatus::Completed => StatusKind::Completed,
        MappedStatus::Cancelled => StatusKind::Cancelled,
    }
}

/// The todo status `status` maps to, `None` for a name the map lacks.
fn target(status: &TicketStatus, config: &TicketSyncConfig) -> Option<StatusKind> {
    match status {
        TicketStatus::Named(name) => config.lookup(name).map(mapped_kind),
        TicketStatus::Closed(CloseReason::Completed) => Some(StatusKind::Completed),
        TicketStatus::Closed(CloseReason::NotPlanned | CloseReason::Duplicate) => {
            Some(StatusKind::Cancelled)
        }
    }
}

/// Fails, naming every fault, when a fetched ticket is in a status the map
/// lacks, the map names one status twice, or the map names a status
/// `known_names` lacks.
fn validate(
    fetched: &[SyncedTicket],
    known_names: &[String],
    config: &TicketSyncConfig,
) -> Result<()> {
    let mut unmapped: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for ticket in fetched {
        if let TicketStatus::Named(name) = &ticket.status
            && config.lookup(name).is_none()
        {
            unmapped.entry(name.trim()).or_default().push(&ticket.key);
        }
    }
    let mut faults: Vec<String> = unmapped
        .into_iter()
        .map(|(name, keys)| {
            format!(
                "[ticket.sync.statuses] does not map the status {name:?} (tickets {})",
                keys.join(", ")
            )
        })
        .collect();
    let mut spellings: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for name in config.statuses.keys() {
        spellings
            .entry(name.trim().to_lowercase())
            .or_default()
            .push(name);
    }
    for names in spellings.values().filter(|names| names.len() > 1) {
        let names: Vec<String> = names.iter().map(|n| format!("{n:?}")).collect();
        faults.push(format!(
            "[ticket.sync.statuses] maps {} as separate names, but status names match ignoring case and surrounding whitespace",
            names.join(", ")
        ));
    }
    for name in config.statuses.keys() {
        if !known_names.iter().any(|known| same_status(known, name)) {
            let known: Vec<String> = known_names.iter().map(|n| format!("{n:?}")).collect();
            faults.push(format!(
                "[ticket.sync.statuses] maps {name:?}, which the tracker has no status for; it has {}",
                known.join(", ")
            ));
        }
    }
    match faults.is_empty() {
        true => Ok(()),
        false => bail!("{}", faults.join("; ")),
    }
}

/// What a sync changes on `node`, given the todos already there and the
/// tickets `tracker` returned. `known_names` are the statuses the tracker
/// has, which every name the map lists must be one of. Every check runs
/// before anything is planned, so a fault plans nothing.
pub fn plan(
    tracker: &str,
    node: &str,
    existing: &[Todo],
    fetched: &[SyncedTicket],
    known_names: &[String],
    config: &TicketSyncConfig,
) -> Result<SyncPlan> {
    validate(fetched, known_names, config)?;
    let ticket_todos: HashMap<&str, &Todo> = existing
        .iter()
        .filter_map(|todo| {
            let ticket = todo.ticket.as_ref().filter(|t| t.tracker == tracker)?;
            Some((ticket.key.as_str(), todo))
        })
        .collect();
    let key_of_id: HashMap<&str, &str> = ticket_todos
        .iter()
        .map(|(key, todo)| (todo.id.as_str(), *key))
        .collect();
    let synced: BTreeSet<&str> = ticket_todos
        .keys()
        .copied()
        .chain(fetched.iter().map(|t| t.key.as_str()))
        .collect();

    let mut out = SyncPlan::default();
    let mut relations = Vec::new();
    for ticket in fetched {
        let name = status_name(&ticket.status);
        let to = target(&ticket.status, config).expect("validated");
        let text = todo_text(tracker, &ticket.key, &ticket.title);
        let edit = |change| PlannedEdit {
            key: ticket.key.clone(),
            change,
        };
        let Some(todo) = ticket_todos.get(ticket.key.as_str()) else {
            out.adds.push(NewTodo {
                project: Some(node.to_string()),
                description: text,
                parent: None,
                order: None,
                ticket: Some(TicketRef {
                    tracker: tracker.to_string(),
                    key: ticket.key.clone(),
                    synced_status: Some(name),
                }),
            });
            if to != StatusKind::Pending {
                out.edits.push(edit(Change::SetStatus(to)));
            }
            if let Some(parent) = ticket
                .parent
                .as_ref()
                .filter(|p| synced.contains(p.as_str()) && **p != ticket.key)
            {
                relations.push(edit(Change::Move(Some(parent.clone()))));
            }
            let blockers: Vec<String> = ticket
                .blocked_by
                .iter()
                .filter(|b| synced.contains(b.as_str()) && **b != ticket.key)
                .cloned()
                .collect();
            if !blockers.is_empty() {
                relations.push(edit(Change::Block(blockers)));
            }
            continue;
        };
        if todo.description != text {
            out.edits.push(edit(Change::Describe(text)));
        }
        let last = todo
            .ticket
            .as_ref()
            .and_then(|t| t.synced_status.as_deref());
        if last != Some(name.as_str()) {
            out.edits.push(edit(Change::SetStatus(to)));
            out.edits.push(edit(Change::RecordSync(name)));
        }

        let want_parent = ticket
            .parent
            .as_deref()
            .filter(|p| synced.contains(p) && *p != ticket.key);
        let have_parent = todo.parent.as_deref().map(|id| key_of_id.get(id).copied());
        let moves = match (have_parent, want_parent) {
            (Some(None), _) => false,
            (Some(Some(have)), Some(want)) => have != want,
            (Some(Some(_)), None) => true,
            (None, want) => want.is_some(),
        };
        if moves {
            relations.push(edit(Change::Move(want_parent.map(String::from))));
        }

        let have: BTreeSet<&str> = todo
            .blocked_by
            .iter()
            .filter_map(|id| key_of_id.get(id.as_str()).copied())
            .collect();
        let want: BTreeSet<&str> = ticket
            .blocked_by
            .iter()
            .map(String::as_str)
            .filter(|b| synced.contains(b) && *b != ticket.key)
            .collect();
        let block: Vec<String> = want.difference(&have).map(|k| k.to_string()).collect();
        let unblock: Vec<String> = have.difference(&want).map(|k| k.to_string()).collect();
        if !block.is_empty() {
            relations.push(edit(Change::Block(block)));
        }
        if !unblock.is_empty() {
            relations.push(edit(Change::Unblock(unblock)));
        }
    }
    out.edits.extend(relations);
    Ok(out)
}
