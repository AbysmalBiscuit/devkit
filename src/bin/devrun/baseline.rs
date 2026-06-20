use anyhow::{Result, bail};
use devkit_common::cmd::git;
use std::path::Path;

/// Ensure `path` is a worktree at a fresh `git_ref` (e.g. origin/staging).
/// Creates it if missing; otherwise fetches and hard-resets — but refuses if the
/// tree is dirty, so no real work is lost.
pub fn ensure_fresh(main_repo: &str, path: &str, git_ref: &str) -> Result<()> {
    let (remote, _) = git_ref.split_once('/').unwrap_or(("origin", git_ref));
    if !Path::new(path).exists() {
        git(&["fetch", remote], main_repo)?;
        git(&["worktree", "add", "--detach", path, git_ref], main_repo)?;
        return Ok(());
    }
    let dirty = !git(&["status", "--porcelain"], path)?.trim().is_empty();
    if dirty {
        bail!(
            "baseline worktree {path} is dirty — refusing to reset --hard. Clean it or remove it."
        );
    }
    git(&["fetch", remote], path)?;
    git(&["reset", "--hard", git_ref], path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refuses_dirty_baseline() {
        let tmp = std::env::temp_dir().join(format!("bl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let p = tmp.to_str().unwrap();
        git(&["init", "-q"], p).unwrap();
        std::fs::write(tmp.join("f"), "x").unwrap();
        // dirty (untracked) tree → guard trips
        let err = ensure_fresh(p, p, "origin/staging").unwrap_err();
        assert!(err.to_string().contains("dirty"));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
