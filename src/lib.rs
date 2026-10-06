//! Pieces the `devkit` binaries share.
//!
//! The six CLIs live in `src/bin/`; anything more than one of them needs lives
//! here so there is a single definition rather than six copies.

pub mod completions;
pub mod help;

/// The version every binary reports. A build that sets
/// `DEVKIT_BUILD_VERSION` reports that instead, which is how a nightly names
/// the commit it was built from.
pub const VERSION: &str = match option_env!("DEVKIT_BUILD_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
