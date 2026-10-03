//! Agent todo lists: who holds a todo, where a list lives, and the store
//! that keeps them.

pub mod builtin;
pub mod holder;
pub mod node;
pub mod transition;

pub use builtin::BuiltinStore;
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
    /// Nests the todo under `parent`, or at the top level when `None`.
    Move {
        id: String,
        parent: Option<String>,
        order: i64,
    },
    Reorder {
        id: String,
        order: i64,
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
pub trait TodoStore: Send + Sync {
    /// Every todo `filter` covers, whatever its status.
    fn list(&self, filter: &Filter) -> anyhow::Result<Vec<Todo>>;
    /// Returns the new todo's id.
    fn add(&self, todo: NewTodo) -> anyhow::Result<String>;
    /// A refused status change is an error whose root cause is [`Claimed`];
    /// an unknown id is the error `no todo <id>`.
    fn apply(&self, edit: &Edit) -> anyhow::Result<()>;
}

/// `text` on one line, every whitespace run collapsed to a single space.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
