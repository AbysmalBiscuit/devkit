//! The one door every subcommand's config resolution passes through.

use std::{path::Path, sync::OnceLock};

use anyhow::{Result, bail};
use devkit_config::{Config, Options, PendingLayer, Provenance};

use crate::vcs::Checkout;

static OPTIONS: OnceLock<Options> = OnceLock::new();

/// Set the `--option` values every later resolve in this process merges over
/// its layers. The command line is parsed once, so a second call is a bug.
pub fn set_options(options: Options) -> Result<()> {
    if OPTIONS.set(options).is_err() {
        bail!("the --option values are already set for this process");
    }
    Ok(())
}

/// The `--option` values of this process: empty until [`set_options`] runs.
pub fn options() -> &'static Options {
    OPTIONS.get_or_init(Options::default)
}

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
        .and_then(crate::vcs::derived_worktree_root);
    let resolved = devkit_config::resolve(
        explicit,
        start,
        checkout.main_checkout(),
        checkout.root(),
        derived.as_deref(),
        options(),
    );
    if let Ok((cfg, _)) = &resolved {
        crate::pool::configure(cfg.parallelism.threads);
        crate::github::configure(cfg.github.no_graphql);
    }
    resolved
}

/// Classify the config [`resolve_in`] loads, `--option` values included,
/// without ever failing.
pub fn health_in(checkout: &Checkout, start: &Path) -> devkit_config::Health {
    devkit_config::Health::of(&resolve_in(checkout, None, start))
}

/// [`resolve`] as it would be once `pending` is written, configuring
/// nothing: the config it answers is not yet the one in force.
pub fn resolve_pending(
    explicit: Option<&Path>,
    start: &Path,
    pending: PendingLayer<'_>,
) -> Result<(Config, Provenance)> {
    let checkout = Checkout::at(start);
    let derived = checkout
        .main_worktree()
        .and_then(crate::vcs::derived_worktree_root);
    devkit_config::resolve_pending(
        explicit,
        start,
        checkout.main_checkout(),
        checkout.root(),
        derived.as_deref(),
        pending,
        options(),
    )
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
            devkit_git::Git::fixture(&repo)
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
