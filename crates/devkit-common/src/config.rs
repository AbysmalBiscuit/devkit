//! The one door every subcommand's config resolution passes through.

use std::path::Path;

use anyhow::Result;
use devkit_config::{Config, Provenance};

use crate::git::Checkout;

/// Resolve the `devkit.toml` layers discovered from `start` — or the single
/// file `explicit` names — and size the shared worker pool from the result.
///
/// Every subcommand reaches its config through here, whether it needs the app
/// catalog (`devkit_ports::load::load`, which wraps this) or only the config
/// itself. That is what lets [`crate::pool`] be sized in one place instead of
/// at each config load, where one of them would be forgotten. The `Result` is
/// handed back untouched, so a caller can still classify it with
/// `devkit_config::Health::of`.
///
/// Resolves a [`Checkout`] of its own. A caller already holding one — the
/// hook path, where several helpers need the same answer — passes it to
/// [`resolve_in`] instead.
pub fn resolve(explicit: Option<&Path>, start: &Path) -> Result<(Config, Provenance)> {
    resolve_in(&Checkout::at(start), explicit, start)
}

/// [`resolve`] against a checkout the caller has already resolved.
///
/// `start` is still read on its own: a `devkit.toml` in a directory between
/// the checkout root and `start` is a layer, so the checkout does not stand in
/// for the working directory.
pub fn resolve_in(
    checkout: &Checkout,
    explicit: Option<&Path>,
    start: &Path,
) -> Result<(Config, Provenance)> {
    // Keyed off the main worktree alone: a bare repository has no directory to
    // put a `_worktrees` sibling beside, and falling back to the caller's own
    // checkout would give every linked worktree a different root.
    let derived = checkout
        .main_worktree()
        .and_then(crate::git::derived_worktree_root);
    let resolved = devkit_config::resolve(
        explicit,
        start,
        checkout.main_checkout(),
        checkout.root(),
        derived.as_deref(),
    );
    if let Ok((cfg, _)) = &resolved {
        crate::pool::configure(cfg.parallelism.threads);
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_common_door_passes_the_derived_root_through() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("proj");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            crate::git::Git::fixture(&repo)
                .args(args.iter().copied())
                .output()
                .unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(
            repo.join("devkit.toml"),
            "[config]\nroot = true\n[defaults]\n",
        )
        .unwrap();

        let cfg = resolve(None, &repo).unwrap().0;
        assert!(
            cfg.defaults.worktree_root.ends_with("proj_worktrees"),
            "derived root not threaded through: {}",
            cfg.defaults.worktree_root
        );
    }
}
