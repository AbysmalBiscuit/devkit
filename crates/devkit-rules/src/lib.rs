//! Matching a `repo-rules-agent` index and config-declared file dumps against
//! the paths a tool call is about to write.
//!
//! The crate's only IO is reading one JSON file. It knows nothing about
//! harnesses, hook payloads or sessions, because it has three callers: the
//! `pre-tool-use` stage, `devkit rules context`, and `devkit rules query`.

pub mod model;
pub mod vocab;
