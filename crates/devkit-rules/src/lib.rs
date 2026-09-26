//! Matching a `repo-rules-agent` index and config-declared file dumps against
//! the paths a tool call is about to write, and editing that index by hand.
//!
//! Reads the index, configured context files, and environment conditions, and
//! writes the index for `devkit rules add`, `edit` and `remove`.
//! Hook payloads and holder state belong to the callers.

pub mod context;
pub mod edit;
pub mod index;
pub mod model;
pub mod query;
pub mod render;
pub mod repo_config;
pub mod vocab;
