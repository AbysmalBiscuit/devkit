//! Agent todo lists: who holds a todo, where a list lives, and the store
//! that keeps them.

pub mod holder;
pub mod transition;

pub use holder::Holder;
pub use transition::{Claimed, Status, StatusKind, transition};
