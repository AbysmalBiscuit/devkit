//! The one door every subcommand's config resolution passes through.

use anyhow::Result;
use devkit_config::{Config, Provenance};
use std::path::Path;

/// Resolve the `devkit.toml` layers discovered from `start` — or the single
/// file `explicit` names — and size the shared worker pool from the result.
///
/// Every subcommand reaches its config through here, whether it needs the app
/// catalog (`devkit_ports::load::load`, which wraps this) or only the config
/// itself. That is what lets [`crate::pool`] be sized in one place instead of
/// at each config load, where one of them would be forgotten. The `Result` is
/// handed back untouched, so a caller can still classify it with
/// `devkit_config::Health::of`.
pub fn resolve(explicit: Option<&Path>, start: &Path) -> Result<(Config, Provenance)> {
    // One `rev-parse --show-toplevel` answers both: `main_checkout_from` takes
    // the root rather than resolving its own. Outside a repository the failed
    // root settles the main checkout too, since `worktree list` cannot report
    // a checkout where `rev-parse` found none.
    let checkout_root = crate::git::checkout_root(start).ok();
    let main_checkout = checkout_root
        .as_deref()
        .and_then(|here| crate::git::main_checkout_from(start, here).ok().flatten());
    let resolved = devkit_config::resolve(
        explicit,
        start,
        main_checkout.as_deref(),
        checkout_root.as_deref(),
    );
    if let Ok((cfg, _)) = &resolved {
        crate::pool::configure(cfg.parallelism.threads);
    }
    resolved
}
