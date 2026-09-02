//! Pieces the `devkit` binaries share.
//!
//! The six CLIs live in `src/bin/`; anything more than one of them needs lives
//! here so there is a single definition rather than six copies.

pub mod completions;
pub mod help;
