//! Agent todo lists: who holds a todo, where a list lives, and the trait a
//! store that keeps them implements.

// ambassador copies the trait's signatures verbatim into the dispatching
// crate, so they use absolute paths that must resolve here too.
extern crate self as devkit_todo;

pub mod activity;
#[cfg(feature = "test-support")]
pub mod contract;
pub mod graph;
pub mod holder;
pub mod layout;
pub mod names;
pub mod node;
pub mod push;
pub mod roles;
pub mod ticket;
pub mod transition;

use std::path::PathBuf;

use devkit_common::paths;
pub use holder::Holder;
pub use node::{Filter, NodeMatch};
use serde::{Deserialize, Serialize};
pub use transition::{Claimed, Status, StatusKind, take_over, transition};

/// The gap left between sibling orders, so a move rarely renumbers siblings.
pub const ORDER_GAP: i64 = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Todo {
    /// Opaque to callers: each backend picks its own form.
    pub id: String,
    /// One line; see [`one_line`].
    pub description: String,
    #[serde(flatten)]
    pub status: Status,
    /// The todo this one nests under. A parent the listing does not hold
    /// leaves the todo at the top level.
    #[serde(default)]
    pub parent: Option<String>,
    /// Position among siblings, lowest first.
    #[serde(default)]
    pub order: Option<i64>,
    /// The node the todo belongs to. Absent is the global list.
    #[serde(default)]
    pub project: Option<String>,
    /// RFC 3339 UTC, so it sorts as text.
    #[serde(default)]
    pub entry: Option<String>,
    /// RFC 3339 UTC, so it sorts as text.
    #[serde(default)]
    pub modified: Option<String>,
    /// Full ids of the todos this one waits on, sorted, with no duplicates.
    /// A blocker the listing does not hold still counts.
    #[serde(default)]
    pub blocked_by: Vec<String>,
    /// The tracker ticket this todo stands for, `None` for a todo of
    /// devkit's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<TicketRef>,
}

/// The tracker ticket a todo stands for. A store holds at most one todo per
/// tracker and key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketRef {
    /// The tracker's kind: `github` or `linear`.
    pub tracker: String,
    /// The ticket's id on the tracker: `owner/repo#65`, `SWE-123`.
    pub key: String,
    /// The ticket's tracker status as the last sync read it, so a sync
    /// changes the todo's status only when the ticket's has moved.
    #[serde(default)]
    pub synced_status: Option<String>,
}

impl TicketRef {
    /// Whether `other` names the same ticket.
    pub fn same_ticket(&self, other: &TicketRef) -> bool {
        self.tracker == other.tracker && self.key == other.key
    }

    /// Whether a link between this ticket and `other` can live on one
    /// tracker: the same tracker and, for keys naming a repository
    /// (`owner/repo#65`), the same repository.
    pub fn same_tracker(&self, other: &TicketRef) -> bool {
        let repo = |key: &str| {
            key.rsplit_once('#')
                .map(|(repo, _)| repo.to_ascii_lowercase())
        };
        self.tracker == other.tracker && repo(&self.key) == repo(&other.key)
    }
}

/// The id a ticket's todo takes in a store whose ids are uuids: a name-based
/// uuid over the store's root (`""` for a store without one), the tracker
/// and the key, so two syncs racing on one ticket create one todo.
pub fn ticket_todo_uuid(root: &str, tracker: &str, key: &str) -> uuid::Uuid {
    let name = [root, tracker, key].join("\0");
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, name.as_bytes())
}

impl Todo {
    /// The node the todo belongs to, the global list when it names none.
    pub fn node(&self) -> &str {
        self.project.as_deref().unwrap_or(node::GLOBAL)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewTodo {
    pub project: Option<String>,
    pub description: String,
    pub parent: Option<String>,
    /// `None` places the todo after its last sibling.
    pub order: Option<i64>,
    /// The ticket the todo stands for. Adding a ticket that already has a
    /// todo returns that todo and changes nothing.
    pub ticket: Option<TicketRef>,
}

/// One change to the store. Every edit but `ReleaseAll` names an existing
/// todo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    /// The stored status comes from [`transition`], never from the caller.
    SetStatus {
        id: String,
        to: StatusKind,
        actor: Holder,
    },
    Describe {
        id: String,
        description: String,
    },
    /// Nests the todo under `parent`, or at the top level when `None`, at
    /// `order` among its new siblings, or after the last of them when `None`.
    Move {
        id: String,
        parent: Option<String>,
        order: Option<i64>,
    },
    Reorder {
        id: String,
        order: i64,
    },
    /// Moves the todo to another node, `None` being the global list.
    Relocate {
        id: String,
        project: Option<String>,
    },
    /// Moves the todo to in progress by `actor` while `from` still holds
    /// it, as [`take_over`] rules, for a holder whose session went silent.
    TakeOver {
        id: String,
        from: Holder,
        actor: Holder,
    },
    /// Every todo in progress by a holder `holder` covers goes back to
    /// pending. A human holder releases nothing.
    ReleaseAll {
        holder: Holder,
    },
    /// Removes the record for good, for a description that must disappear.
    Purge(String),
    /// Adds each blocker in `by` to `id`. A blocker already recorded is
    /// left as it is. The store does not check that a blocker exists.
    Block {
        id: String,
        by: Vec<String>,
    },
    /// Removes each blocker in `by` from `id`. A blocker not recorded is
    /// left as it is.
    Unblock {
        id: String,
        by: Vec<String>,
    },
    /// Records `synced_status` as the ticket status the todo last synced.
    /// A todo with no ticket is the error `todo <id> has no ticket`.
    RecordSync {
        id: String,
        synced_status: String,
    },
}

/// One todo's status as an edit changed it, seen while the store held the
/// lock it wrote under, so a recorder needs no read of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusChange {
    pub todo: String,
    pub node: String,
    pub from: Status,
    /// `None` when the edit removed the record.
    pub to: Option<Status>,
    /// When the store made the change, taken under that lock.
    pub at: chrono::DateTime<chrono::Utc>,
}

impl StatusChange {
    /// `todo` moving to `to`, now.
    pub fn of(todo: &Todo, to: Option<Status>) -> Self {
        Self {
            todo: todo.id.clone(),
            node: todo.node().to_string(),
            from: todo.status.clone(),
            to,
            at: std::time::SystemTime::now().into(),
        }
    }
}

/// Where todos are kept. A backend that cannot apply an edit atomically
/// documents the race.
#[ambassador::delegatable_trait]
pub trait TodoStore {
    /// Every todo `filter` covers, whatever its status.
    fn list(
        &self,
        filter: &::devkit_todo::Filter,
    ) -> ::anyhow::Result<::std::vec::Vec<::devkit_todo::Todo>>;
    /// The todo `id` names. A backend whose ids can be abbreviated resolves a
    /// unique abbreviation; an ambiguous one is an error that names the
    /// matches.
    fn get(&self, id: &str) -> ::anyhow::Result<::std::option::Option<::devkit_todo::Todo>>;
    /// Returns the new todo's id.
    fn add(&self, todo: ::devkit_todo::NewTodo) -> ::anyhow::Result<::std::string::String>;
    /// Returns every status change the edit made, seen under the lock it
    /// wrote under. A refused status change is an error whose root cause is
    /// [`Claimed`]; an unknown id is the error `no todo <id>`.
    fn apply(
        &self,
        edit: &::devkit_todo::Edit,
    ) -> ::anyhow::Result<::std::vec::Vec<::devkit_todo::StatusChange>>;
}

impl<T: TodoStore + ?Sized> TodoStore for &T {
    fn list(&self, filter: &Filter) -> anyhow::Result<Vec<Todo>> {
        (**self).list(filter)
    }

    fn get(&self, id: &str) -> anyhow::Result<Option<Todo>> {
        (**self).get(id)
    }

    fn add(&self, todo: NewTodo) -> anyhow::Result<String> {
        (**self).add(todo)
    }

    fn apply(&self, edit: &Edit) -> anyhow::Result<Vec<StatusChange>> {
        (**self).apply(edit)
    }
}

/// Where devkit keeps todo state of its own, whichever backend holds the
/// todos: the native map, the context digests, the activity log, and the
/// built-in store.
pub fn state_dir() -> anyhow::Result<PathBuf> {
    Ok(paths::state_dir()?.join("todo"))
}

/// Where the lists last injected for `holder` are fingerprinted, keyed on a
/// hash of the full holder so a sub-agent's injection never suppresses its
/// session's.
pub fn digest_path(holder: &Holder) -> anyhow::Result<PathBuf> {
    Ok(state_dir()?.join("digests").join(render::digest(holder)))
}

/// `id` as text an agent reads it: a uuid shortens to its first 8
/// characters, taskwarrior's `uuid.short`; any other id stays whole.
pub fn short_id(id: &str) -> &str {
    let is_uuid = id.len() == 36
        && id.matches('-').count() == 4
        && id.chars().all(|c| c == '-' || c.is_ascii_hexdigit());
    if is_uuid { &id[..8] } else { id }
}

/// The shortest uuid prefix an id may be, taskwarrior's `uuid.short`.
pub const SHORT_ID: usize = 8;

/// Whether `id` can name a uuid by prefix: at least [`short_id`]'s 8
/// characters, and only hex digits and dashes. Anything else would reach a
/// store's query as syntax.
pub fn is_uuid_prefix(id: &str) -> bool {
    (SHORT_ID..=36).contains(&id.len()) && id.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
}

/// The one todo in `todos` whose uuid starts with `id`, for a backend whose
/// ids are uuids. More than one is an error naming every match.
pub fn by_prefix(id: &str, todos: impl IntoIterator<Item = Todo>) -> anyhow::Result<Option<Todo>> {
    if !is_uuid_prefix(id) {
        return Ok(None);
    }
    let mut matches: Vec<Todo> = todos.into_iter().filter(|t| t.id.starts_with(id)).collect();
    if matches.len() > 1 {
        matches.sort_by(|a, b| a.id.cmp(&b.id));
        let uuids: Vec<&str> = matches.iter().map(|t| t.id.as_str()).collect();
        anyhow::bail!("todo id {id} is ambiguous: {}", uuids.join(", "));
    }
    Ok(matches.pop())
}

/// `text` on one line, every whitespace run collapsed to a single space.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
pub mod diff;
pub mod hold;
pub mod native;
pub mod render;

#[cfg(test)]
mod tests {
    use super::{is_uuid_prefix, short_id, ticket_todo_uuid};

    #[test]
    fn ticket_todo_uuid_is_stable() {
        let id = ticket_todo_uuid("root", "github", "65");
        assert_eq!(id, ticket_todo_uuid("root", "github", "65"));
        assert_eq!(id.get_version_num(), 5);
        assert_ne!(id, ticket_todo_uuid("other", "github", "65"));
        assert_ne!(id, ticket_todo_uuid("root", "linear", "65"));
        assert_ne!(id, ticket_todo_uuid("root", "github", "66"));
    }

    #[test]
    fn only_hex_of_a_short_ids_length_names_a_uuid() {
        assert!(is_uuid_prefix("abcdef12"));
        assert!(is_uuid_prefix("96432cd6-082a-4d8c-a9cb-adef8823ff92"));
        assert!(!is_uuid_prefix("abcdef1"));
        assert!(!is_uuid_prefix("status:pending"));
        assert!(!is_uuid_prefix("abcdef12 or project:x"));
    }

    #[test]
    fn a_uuid_shortens_to_its_first_eight_characters() {
        assert_eq!(short_id("96432cd6-082a-4d8c-a9cb-adef8823ff92"), "96432cd6");
    }

    #[test]
    fn other_ids_stay_whole() {
        assert_eq!(short_id("17"), "17");
        let not_a_uuid = "x".repeat(36);
        assert_eq!(short_id(&not_a_uuid), not_a_uuid);
        let dashes = "zzzzzzzz-zzzz-zzzz-zzzz-zzzzzzzzzzzz";
        assert_eq!(short_id(dashes), dashes);
    }
}
