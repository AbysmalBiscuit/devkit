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

/// The commit this worktree forked from: the merge base of its HEAD and
/// `target`. Local refs only, so no fetch is needed — extending a branch does
/// not move its merge base with another branch, and the value changes only
/// when the worktree is rebased.
#[allow(dead_code)]
pub fn pin(worktree: &Path, target: &str) -> Result<String> {
    let out = devkit_common::git::Git::at(worktree)
        .args(["merge-base", "HEAD", target])
        .output()
        .with_context(|| format!("resolving the fork point between HEAD and `{target}`"))?;
    let sha = out.trim();
    anyhow::ensure!(
        !sha.is_empty(),
        "`git merge-base HEAD {target}` named no commit"
    );
    Ok(sha.to_string())
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

    /// Two commits on main, a branch cut from the first, then main advances:
    /// the merge base stays at the fork point.
    #[test]
    fn the_pin_is_the_fork_point_not_the_tip() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        let git = |args: &[&str]| {
            devkit_common::git::Git::fixture(p)
                .args(args.iter().copied())
                .output()
                .unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(p.join("a"), "1").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "one"]);
        let fork = git(&["rev-parse", "HEAD"]).trim().to_string();
        git(&["checkout", "-qb", "feat"]);
        std::fs::write(p.join("b"), "2").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "two"]);
        git(&["checkout", "-q", "main"]);
        std::fs::write(p.join("c"), "3").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "three"]);
        git(&["checkout", "-q", "feat"]);

        let got = pin(p, "main").unwrap();
        assert_eq!(got, fork);
        assert_eq!(got.len(), 40);
    }

    /// Rebasing `feat` onto a `main` that has advanced moves the merge base to
    /// `main`'s new tip: this is what re-resolving after a rebase relies on.
    #[test]
    fn a_rebase_moves_the_pin_to_the_new_tip() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        let git = |args: &[&str]| {
            devkit_common::git::Git::fixture(p)
                .args(args.iter().copied())
                .output()
                .unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(p.join("a"), "1").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "one"]);
        let first_fork = git(&["rev-parse", "HEAD"]).trim().to_string();
        git(&["checkout", "-qb", "feat"]);
        std::fs::write(p.join("b"), "2").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "two"]);
        git(&["checkout", "-q", "main"]);
        std::fs::write(p.join("c"), "3").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "three"]);
        let new_tip = git(&["rev-parse", "HEAD"]).trim().to_string();
        git(&["checkout", "-q", "feat"]);

        assert_eq!(pin(p, "main").unwrap(), first_fork);

        git(&["rebase", "main"]);

        let rebased = pin(p, "main").unwrap();
        assert_eq!(rebased, new_tip);
        assert_ne!(rebased, first_fork);
    }

    #[test]
    fn unrelated_histories_error_naming_both_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        let git = |args: &[&str]| {
            devkit_common::git::Git::fixture(p)
                .args(args.iter().copied())
                .output()
                .unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(p.join("a"), "1").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "one"]);
        git(&["checkout", "-q", "--orphan", "lonely"]);
        git(&["commit", "-qm", "orphan", "--allow-empty"]);

        let err = pin(p, "main").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("HEAD"), "{msg}");
        assert!(msg.contains("main"), "{msg}");
    }
}
