//! Agent todo lists kept in a taskchampion replica that devkit embeds, local
//! or synced to a directory or a taskchampion sync server. The tasks have the
//! shape [`schema`] gives them, with bare project nodes shared with alacritree.

mod location;
pub mod schema;
mod store;

pub use location::{ReplicaLocation, ReplicaSource, replica_location};
pub use store::{SyncTarget, TaskchampionStore};
pub use taskchampion::Uuid;
