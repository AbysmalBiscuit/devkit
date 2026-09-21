//! Matching a `repo-rules-agent` index and config-declared file dumps against
//! the paths a tool call is about to write.
//!
//! Reads the index, configured context files, and environment conditions.
//! Hook payloads and holder state belong to the callers.

pub mod context;
pub mod index;
pub mod model;
pub mod query;
pub mod render;
pub mod vocab;
