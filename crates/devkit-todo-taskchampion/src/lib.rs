//! Agent todo lists kept in a taskchampion replica that devkit embeds, local
//! or synced to a directory or a taskchampion sync server. The tasks have the
//! shape and root project [`devkit_todo_taskwarrior::schema`] gives them, so
//! taskwarrior reads a synced replica's lists.

mod store;

pub use store::{SyncTarget, TaskchampionStore};
pub use taskchampion::Uuid;
