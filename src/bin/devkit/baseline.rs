//! Where the baseline comparison starts. Every consumer of `baseline_ref`
//! resolves through here, so a project that declares nothing still gets the
//! remote's default branch.

use anyhow::{Context, Result};
use devkit_config::Config;
use std::path::Path;

/// The ref a worktree's baseline is measured against: the configured
/// `baseline_ref`, else the remote's default branch.
pub fn target(cfg: &Config, repo: &Path) -> Result<String> {
    if !cfg.defaults.baseline_ref.is_empty() {
        return Ok(cfg.defaults.baseline_ref.clone());
    }
    devkit_common::git::default_remote_branch(repo).context(
        "no baseline target: set `defaults.baseline_ref`, \
         or run `git remote set-head origin -a` so origin/HEAD names one",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configured_ref_wins_over_detection() {
        let mut cfg = devkit_config::Config::default();
        cfg.defaults.baseline_ref = "origin/release".into();
        // Detection would fail in a non-repo path; the configured ref means it is
        // never consulted.
        let got = target(&cfg, std::path::Path::new("/nonexistent")).unwrap();
        assert_eq!(got, "origin/release");
    }

    #[test]
    fn an_undetectable_target_names_both_fixes() {
        let tmp = tempfile::tempdir().unwrap();
        devkit_common::git::Git::fixture(tmp.path())
            .args(["init", "-q"])
            .output()
            .unwrap();
        let err = target(&devkit_config::Config::default(), tmp.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("baseline_ref"), "{msg}");
        assert!(msg.contains("git remote set-head"), "{msg}");
    }
}
