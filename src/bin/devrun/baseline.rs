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

    #[test]
    fn head_at_true_only_when_head_equals_ref() {
        let tmp = std::env::temp_dir().join(format!("headat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let p = tmp.to_str().unwrap();
        git(&["init", "-q"], p).unwrap();
        git(&["config", "user.email", "t@t"], p).unwrap();
        git(&["config", "user.name", "t"], p).unwrap();
        git(&["config", "commit.gpgsign", "false"], p).unwrap();
        std::fs::write(tmp.join("f"), "a").unwrap();
        git(&["add", "-A"], p).unwrap();
        git(&["commit", "-qm", "init"], p).unwrap();
        git(&["branch", "target"], p).unwrap();

        // HEAD and `target` point at the same commit.
        assert!(head_at(p, "target"));

        // Move HEAD forward; `target` stays behind.
        std::fs::write(tmp.join("f"), "b").unwrap();
        git(&["commit", "-aqm", "second"], p).unwrap();
        assert!(!head_at(p, "target"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
