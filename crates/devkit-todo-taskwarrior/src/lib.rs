//! Agent todo lists kept in the local taskwarrior, through the `task`
//! program, under one root project: global todos on the root itself, every
//! other node at `<root>.<node>`. A task outside the root is never a todo.
//!
//! Every write addresses tasks by full uuid. An id shorter than a uuid
//! resolves when at least 8 characters long and unique.
//!
//! A status change reads the task, decides the new status with
//! [`devkit_todo::transition`], and writes it, under a devkit file lock that
//! also covers placing a todo after its siblings and releasing a holder's
//! claims. Taskwarrior has no compare-and-swap, so the lock serializes
//! devkit's own callers only: a person running `task start` between devkit's
//! read and its write loses to devkit's write.
//!
//! A claim is written as `start:now` through `modify`, which, unlike
//! `task start`, adds no "Started task" annotation when `journal.time` is on.

mod cli;
pub mod schema;
mod store;

pub use cli::TaskwarriorNotFound;
pub use store::TaskwarriorStore;
