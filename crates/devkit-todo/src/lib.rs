//! Agent todo lists: who holds a todo, where a list lives, and the trait a
//! store that keeps them implements.

// ambassador copies the trait's signatures verbatim into the dispatching
// crate, so they use absolute paths that must resolve here too.
extern crate self as devkit_todo;

pub mod builtin;
#[cfg(feature = "test-support")]
pub mod contract;
pub mod holder;
pub mod node;
pub mod transition;

use std::path::PathBuf;

pub use builtin::BuiltinStore;
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

/// Where devkit keeps todo state of its own, whichever backend holds the
/// todos: the native map, the context digests, and the built-in store.
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

/// `text` on one line, every whitespace run collapsed to a single space.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
pub mod diff;
pub mod native;
pub mod render;

#[cfg(test)]
mod tests {
    use super::short_id;

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
