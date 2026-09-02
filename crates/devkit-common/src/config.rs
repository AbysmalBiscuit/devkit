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
    // Outside a repository the failed root settles the main checkout too,
    // since `worktree list` cannot report a checkout where `rev-parse` found
    // none.
    let checkout_root = crate::git::checkout_root(start).ok();
    // One `worktree list` answers both of the questions below. Asking through
    // `main_checkout_from` and `non_bare_main` would run the listing twice for
    // one config load.
    let main_worktree = crate::git::worktrees(start)
        .ok()
        .and_then(|all| all.into_iter().next())
        .filter(|w| !w.bare);
    let main_checkout = match (&main_worktree, checkout_root.as_deref()) {
        (Some(w), Some(here)) if !crate::git::same_path(&w.path, here) => Some(w.path.clone()),
        _ => None,
    };
    // Keyed off the main worktree alone: a bare repository has no directory to
    // put a `_worktrees` sibling beside, and falling back to the caller's own
    // checkout would give every linked worktree a different root.
    let derived = main_worktree
        .as_ref()
        .and_then(|w| crate::git::derived_worktree_root(&w.path));
    let resolved = devkit_config::resolve(
        explicit,
        start,
        main_checkout.as_deref(),
        checkout_root.as_deref(),
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
