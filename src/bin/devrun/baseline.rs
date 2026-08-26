use anyhow::{Result, bail};
use devkit_common::cmd::git;
use devkit_common::gitfetch;
use std::path::Path;

/// Ensure `path` is a worktree at a fresh `git_ref` (e.g. origin/staging).
/// Creates it if missing; otherwise fetches and hard-resets — but refuses if the
/// tree is dirty, so no real work is lost.
pub fn ensure_fresh(main_repo: &str, path: &str, git_ref: &str) -> Result<()> {
    let (remote, _) = git_ref.split_once('/').unwrap_or(("origin", git_ref));
    if !Path::new(path).exists() {
        gitfetch::fetch(remote, main_repo)?;
        git(&["worktree", "add", "--detach", path, git_ref], main_repo)?;
        return Ok(());
    }
    let dirty = !git(&["status", "--porcelain"], path)?.trim().is_empty();
    if dirty {
        bail!(
            "baseline worktree {path} is dirty — refusing to reset --hard. Clean it or remove it."
        );
    }
    gitfetch::fetch(remote, path)?;
    if head_at(path, git_ref) {
        return Ok(());
    }
    git(&["reset", "--hard", git_ref], path)?;
    Ok(())
}

/// True when `path`'s HEAD already resolves to the same commit as `git_ref`.
/// The tree is clean by the time this is reached, so a matching HEAD means a
/// `reset --hard git_ref` would be a no-op and can be skipped.
fn head_at(path: &str, git_ref: &str) -> bool {
    let rev = |r: &str| {
        git(&["rev-parse", r], path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    match (rev("HEAD"), rev(git_ref)) {
        (Some(head), Some(target)) => head == target,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    /// Git ignores the developer's real global/system config, so a fixture
    /// commit can't inherit ambient settings like `commit.gpgsign`.
    fn git_test(args: &[&str], cwd: &str) -> Result<String> {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .context("failed to spawn `git`")?;
        anyhow::ensure!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    #[test]
    fn refuses_dirty_baseline() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().to_str().unwrap();
        git_test(&["init", "-q"], p).unwrap();
        std::fs::write(tmp.path().join("f"), "x").unwrap();
        // dirty (untracked) tree → guard trips
        let err = ensure_fresh(p, p, "origin/staging").unwrap_err();
        assert!(err.to_string().contains("dirty"));
    }

    #[test]
    fn head_at_true_only_when_head_equals_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().to_str().unwrap();
        git_test(&["init", "-q"], p).unwrap();
        git_test(&["config", "user.email", "t@t"], p).unwrap();
        git_test(&["config", "user.name", "t"], p).unwrap();
        std::fs::write(tmp.path().join("f"), "a").unwrap();
        git_test(&["add", "-A"], p).unwrap();
        git_test(&["commit", "-qm", "init"], p).unwrap();
        git_test(&["branch", "target"], p).unwrap();

        // HEAD and `target` point at the same commit.
        assert!(head_at(p, "target"));

        // Move HEAD forward; `target` stays behind.
        std::fs::write(tmp.path().join("f"), "b").unwrap();
        git_test(&["commit", "-aqm", "second"], p).unwrap();
        assert!(!head_at(p, "target"));
    }
}
