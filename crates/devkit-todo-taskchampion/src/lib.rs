//! Agent todo lists kept in a taskchampion replica that devkit embeds, local
//! or synced to a directory or a taskchampion sync server. The tasks have the
//! shape and root project [`schema`] gives them, so
//! taskwarrior reads a synced replica's lists.

mod location;
pub mod schema;
mod store;

pub use location::{ReplicaLocation, ReplicaSource, replica_location};
pub use store::{SyncTarget, TaskchampionStore};
pub use taskchampion::Uuid;
