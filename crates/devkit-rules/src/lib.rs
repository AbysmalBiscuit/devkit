//! Matching a `repo-rules-agent` rule store and config-declared file dumps
//! against the paths a tool call is about to write, and editing that store by
//! hand.
//!
//! Reads the store (a SQLite store, a JSON index or the shared Postgres
//! database), configured context files, and environment conditions, and
//! writes the store for `devkit rules add`, `edit` and `remove`.
//! Hook payloads and holder state belong to the callers.

pub mod cache;
pub mod context;
pub mod edit;
pub mod index;
pub mod model;
pub mod postgres;
pub mod query;
pub mod render;
pub mod repo_config;
pub mod source;
pub mod sqlite;
pub mod vocab;
