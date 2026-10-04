//! Agent todo lists: who holds a todo, where a list lives, and the trait a
//! store that keeps them implements.

// ambassador copies the trait's signatures verbatim into the dispatching
// crate, so they use absolute paths that must resolve here too.
extern crate self as devkit_todo;

pub mod activity;
#[cfg(feature = "test-support")]
pub mod contract;
pub mod holder;
pub mod node;
pub mod transition;

use std::path::PathBuf;

use devkit_common::paths;
pub use holder::Holder;
pub use node::{Filter, NodeMatch};
use serde::{Deserialize, Serialize};
pub use transition::{Claimed, Status, StatusKind, transition};

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
    /// Every todo in progress by a holder `holder` covers goes back to
    /// pending. A human holder releases nothing.
    ReleaseAll {
        holder: Holder,
    },
    /// Removes the record for good, for a description that must disappear.
    Purge(String),
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
    /// A refused status change is an error whose root cause is [`Claimed`];
    /// an unknown id is the error `no todo <id>`.
    fn apply(&self, edit: &::devkit_todo::Edit) -> ::anyhow::Result<()>;
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

    fn apply(&self, edit: &Edit) -> anyhow::Result<()> {
        (**self).apply(edit)
    }
}

/// Where devkit keeps todo state of its own, whichever backend holds the
/// todos: the native map, the context digests, the activity log, and the
/// built-in store.
pub fn state_dir() -> PathBuf {
    paths::state_dir().join("todo")
}

/// Where the lists last injected for `holder` are fingerprinted, keyed on a
/// hash of the full holder so a sub-agent's injection never suppresses its
/// session's.
pub fn digest_path(holder: &Holder) -> PathBuf {
    state_dir().join("digests").join(render::digest(holder))
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
const SHORT_ID: usize = 8;

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
pub mod native;
pub mod render;

#[cfg(test)]
mod tests {
    use super::{is_uuid_prefix, short_id};

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
