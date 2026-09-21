//! The probe this binary answers so a link's identity can be checked.
//!
//! The shim table itself lives in `devkit_common::shim`, where the command
//! guard can read it too. Dispatch reads `argv[0]` rather than
//! `current_exe()`: the two disagree by link type. `current_exe()` resolves a
//! symlink to its target and reports `devkit` for every shim, while `argv[0]`
//! carries the name the caller actually typed under both hardlinks and
//! symlinks.

pub use devkit_common::shim::Shim;

/// The argv[1] a probe passes to ask "are you genuinely devkit" without
/// invoking any real subcommand or touching stdin, intercepted in `main`
/// before any clap parsing. Defined next to the responder so the probe
/// (`links::is_devkit_binary`) and `main` cannot drift apart.
pub const PROBE_FLAG: &str = "--devkit-shim-probe";

/// The fixed line the probe responder prints and the probe expects back
/// verbatim.
pub const PROBE_MARKER: &str = "devkit-shim-ok";
