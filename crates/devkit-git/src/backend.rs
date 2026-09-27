//! Git behind [`VersionControl`].

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use devkit_vcs::{Changes, DETACHED, NewWorktree, Ownership, VersionControl, Worktree};

use super::{Git, SLOW_TIMEOUT};

/// Holds no state: every call spawns git against the directory it is given.
#[derive(Debug, Clone, Copy, Default)]
pub struct GitBackend;

impl VersionControl for GitBackend {
    /// A `.git` entry at or above `dir`, or `dir` inside a bare repository.
    fn claims(&self, dir: &Path) -> bool {
        dir.ancestors().any(|d| {
            d.join(".git").exists() || (d.join("HEAD").is_file() && d.join("objects").is_dir())
        })
    }

    fn root(&self, dir: &Path) -> Result<Option<PathBuf>> {
        let out = Git::at(dir).args(["rev-parse", "--show-toplevel"]).wait()?;
        if !out.status.success() {
            return Ok(None);
        }
        Ok(Some(PathBuf::from(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        )))
    }

    fn worktrees(&self, dir: &Path) -> Result<Option<Vec<Worktree>>> {
        let out = Git::at(dir)
            .args(["worktree", "list", "--porcelain"])
            .wait()?;
        if !out.status.success() {
            return Ok(None);
        }
        Ok(Some(parse_porcelain(&String::from_utf8_lossy(&out.stdout))))
    }

    fn ownership(&self, dir: &Path) -> Ownership {
        gitdir_ownership(dir)
    }

    fn branch(&self, dir: &Path) -> Result<String> {
        let branch = Git::at(dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()?
            .trim()
            .to_string();
        Ok(if branch == "HEAD" {
            DETACHED.to_string()
        } else {
            branch
        })
    }

    fn revision(&self, dir: &Path) -> Result<String> {
        Ok(Git::at(dir)
            .args(["rev-parse", "HEAD"])
            .output()?
            .trim()
            .to_string())
    }

    fn ahead(&self, dir: &Path, base: &str) -> Result<u32> {
        let range = format!("{base}..HEAD");
        let out = Git::at(dir)
            .args(["rev-list", "--count", "--end-of-options", &range])
            .output()?;
        out.trim()
            .parse()
            .with_context(|| format!("`git rev-list --count {range}` printed {out:?}"))
    }

    fn has_branch(&self, repo: &Path, name: &str) -> Result<bool> {
        Git::at(repo)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{name}"),
            ])
            .success()
    }

    fn delete_branch(&self, repo: &Path, name: &str) -> Result<()> {
        Git::at(repo).args(["branch", "-D", name]).output()?;
        Ok(())
    }

    /// From the `origin/HEAD` symbolic ref. `git clone` sets it; `git init`
    /// plus a manually added remote does not.
    fn default_branch(&self, repo: &Path) -> Result<String> {
        let out = Git::at(repo)
            .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
            .output()?;
        let s = out.trim();
        anyhow::ensure!(!s.is_empty(), "origin/HEAD names no branch");
        Ok(s.to_string())
    }

    fn dirty(&self, dir: &Path, changes: Changes) -> Result<bool> {
        let git = Git::at(dir).args(["status", "--porcelain"]);
        // The tracked-only read gates a baseline removal, and gets the bound
        // that removal has.
        let git = match changes {
            Changes::All => git,
            Changes::Tracked => git.args(["--untracked-files=no"]).timeout(SLOW_TIMEOUT),
        };
        Ok(!git.output()?.trim().is_empty())
    }

    fn fork_point(&self, dir: &Path, target: &str) -> Result<String> {
        let out = Git::at(dir).args(["merge-base", "HEAD", target]).output()?;
        let sha = out.trim();
        anyhow::ensure!(
            !sha.is_empty(),
            "`git merge-base HEAD {target}` named no commit"
        );
        Ok(sha.to_string())
    }

    fn changed_paths(&self, dir: &Path, base: &str) -> Result<Vec<String>> {
        Ok(Git::at(dir)
            .args(["diff", "--name-only", &format!("{base}...HEAD")])
            .output()?
            .lines()
            .map(str::to_string)
            .collect())
    }

    fn create_worktree(&self, new: &NewWorktree<'_>) -> Result<()> {
        let path = utf8(new.path)?;
        let mut args = vec!["worktree", "add"];
        match new.branch {
            None => args.push("--detach"),
            // A remote-tracking start would otherwise become the new branch's
            // upstream, and a plain `git push` then refuses on the name
            // mismatch.
            Some(branch) => args.extend(["--no-track", "-b", branch]),
        }
        args.extend([path, new.start]);
        Git::at(new.main).args(args).network().output()?;
        Ok(())
    }

    fn remove_worktree(&self, main: &Path, path: &Path, force: bool) -> Result<()> {
        let path = utf8(path)?;
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(path);
        Git::at(main).args(args).timeout(SLOW_TIMEOUT).output()?;
        Ok(())
    }

    fn prune(&self, main: &Path) -> Result<()> {
        Git::at(main).args(["worktree", "prune"]).output()?;
        Ok(())
    }

    fn fetch(&self, repo: &Path, remote: &str) -> Result<()> {
        Git::at(repo).args(["fetch", remote]).network().output()?;
        Ok(())
    }

    fn push(&self, dir: &Path, remote: &str, branch: &str) -> Result<()> {
        Git::at(dir)
            .args(["push", "-u", remote, branch])
            .network()
            .output()?;
        Ok(())
    }

    fn pushed(&self, dir: &Path) -> Result<bool> {
        Ok(!Git::at(dir)
            .args(["branch", "--remotes", "--contains", "HEAD"])
            .output()?
            .trim()
            .is_empty())
    }

    fn checkout_remote_ref(
        &self,
        dir: &Path,
        remote: &str,
        remote_ref: &str,
        branch: &str,
    ) -> Result<()> {
        Git::at(dir)
            .args(["fetch", remote, remote_ref])
            .network()
            .output()
            .with_context(|| format!("fetching {remote_ref} from {remote}"))?;
        Git::at(dir)
            .args(["checkout", "-B", branch, "FETCH_HEAD"])
            .output()
            .with_context(|| format!("checking out {branch}"))?;
        Ok(())
    }

    fn remote_url(&self, dir: &Path, remote: &str) -> Result<String> {
        Ok(Git::at(dir)
            .args(["remote", "get-url", remote])
            .output()?
            .trim()
            .to_string())
    }

    fn user_email(&self, dir: &Path) -> Result<String> {
        Ok(Git::at(dir)
            .args(["config", "user.email"])
            .output()?
            .trim()
            .to_string())
    }
}

/// A path handed to git as an argument. A lossy spelling would aim the call,
/// a removal included, at some other directory.
fn utf8(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("path not UTF-8: {}", path.display()))
}

/// Classify `dir`'s `.git`. Every failure that is not "the thing is not there"
/// is [`Ownership::Unknown`]: the error set is open — permissions, a broken
/// mount, a file this process may unlink but not read — and no member of it
/// proves the directory is unowned.
fn gitdir_ownership(dir: &Path) -> Ownership {
    let dot = dir.join(".git");
    let md = match std::fs::metadata(&dot) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ownership::Unowned,
        Err(_) => return Ownership::Unknown,
    };
    if md.is_dir() {
        return Ownership::Owned;
    }
    let Ok(body) = std::fs::read(&dot) else {
        return Ownership::Unknown;
    };
    let Some(named) = body
        .split(|b| *b == b'\n')
        .find_map(|line| line.strip_prefix(b"gitdir:"))
    else {
        // A `.git` file that names no git directory is not a pointer at
        // anything, whatever else it holds.
        return Ownership::Unowned;
    };
    // A path this build cannot spell is a path it cannot check.
    let Ok(named) = std::str::from_utf8(named) else {
        return Ownership::Unknown;
    };
    // git strips only the trailing newline when it reads this file back, so
    // every other space belongs to the directory name. Rather than reproduce
    // git's parse on a path that decides a deletion, anything still carrying
    // whitespace at either end after the separator is left unproven.
    let named = named.trim_end_matches(['\r', '\n']);
    let named = named.strip_prefix(' ').unwrap_or(named);
    if named != named.trim() {
        return Ownership::Unknown;
    }
    let named = Path::new(named);
    let target = if named.is_absolute() {
        named.to_path_buf()
    } else {
        dir.join(named)
    };
    // `Path::exists` folds a permission error into `false`, which is the whole
    // bug this function exists to avoid.
    match std::fs::symlink_metadata(&target) {
        Ok(_) => Ownership::Owned,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ownership::Unowned,
        Err(_) => Ownership::Unknown,
    }
}

/// Parse `git worktree list --porcelain`. Git lists the main worktree first,
/// which is what `main_checkout` relies on.
pub fn parse_porcelain(out: &str) -> Vec<Worktree> {
    let mut all = Vec::new();
    let mut path: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut bare = false;
    let mut locked = false;

    fn flush(
        p: &mut Option<String>,
        b: &mut Option<String>,
        bare: &mut bool,
        locked: &mut bool,
        v: &mut Vec<Worktree>,
    ) {
        if let Some(pp) = p.take() {
            v.push(Worktree {
                path: PathBuf::from(pp),
                branch: b.take().unwrap_or_else(|| DETACHED.into()),
                bare: std::mem::take(bare),
                locked: std::mem::take(locked),
            });
        }
    }

    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            flush(&mut path, &mut branch, &mut bare, &mut locked, &mut all);
            path = Some(p.to_string());
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            branch = Some(b.to_string());
        } else if line.trim() == "bare" {
            bare = true;
        } else if line.trim() == "locked" || line.starts_with("locked ") {
            // The reason is optional and free text, so only its presence is
            // read.
            locked = true;
        }
    }
    flush(&mut path, &mut branch, &mut bare, &mut locked, &mut all);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str], cwd: &Path) -> Result<String> {
        Git::fixture(cwd).args(args.iter().copied()).output()
    }

    /// Builds a repo with one commit; returns the guard so the caller keeps the
    /// directory alive for as long as it uses the path.
    fn repo_with_commit() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        Git::fixture(dir.path())
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        Git::fixture(dir.path())
            .args(["config", "user.email", "t@example.com"])
            .output()
            .unwrap();
        Git::fixture(dir.path())
            .args(["config", "user.name", "Test"])
            .output()
            .unwrap();
        std::fs::write(dir.path().join("f.txt"), "x").unwrap();
        Git::fixture(dir.path())
            .args(["add", "."])
            .output()
            .unwrap();
        Git::fixture(dir.path())
            .args(["commit", "-qm", "init"])
            .output()
            .unwrap();
        dir
    }

    #[test]
    fn parse_porcelain_marks_a_bare_first_entry() {
        let parsed = parse_porcelain("worktree /x/b.git\nbare\n");
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].bare);
    }

    /// The lock is what makes git refuse a removal, and the reason git prints
    /// after it is optional free text.
    #[test]
    fn parse_porcelain_reads_the_lock_with_or_without_a_reason() {
        let parsed = parse_porcelain(
            "worktree /x/a\nbranch refs/heads/a\nlocked\n\n\
             worktree /x/b\nbranch refs/heads/b\nlocked being restored\n\n\
             worktree /x/c\nbranch refs/heads/c\n",
        );
        let locked: Vec<bool> = parsed.iter().map(|w| w.locked).collect();
        assert_eq!(locked, vec![true, true, false], "{parsed:?}");
    }

    #[test]
    fn branch_names_a_checked_out_branch() {
        let repo = repo_with_commit();
        assert_eq!(GitBackend.branch(repo.path()).unwrap(), "main");
    }

    #[test]
    fn branch_is_detached_with_no_branch_checked_out() {
        let repo = repo_with_commit();
        run(&["checkout", "-q", "--detach"], repo.path()).unwrap();
        assert_eq!(GitBackend.branch(repo.path()).unwrap(), "DETACHED");
    }

    /// `devrun up` picks apps by the directory a changed path starts with,
    /// so a long path has to come back whole, never shortened with `...`.
    #[test]
    fn changed_paths_names_a_long_path_whole() {
        let repo = repo_with_commit();
        let long = format!("apps/api/{}x.ts", "deep/".repeat(30));
        let file = repo.path().join(&long);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "x").unwrap();
        run(&["checkout", "-qb", "side"], repo.path()).unwrap();
        run(&["add", "."], repo.path()).unwrap();
        run(&["commit", "-qm", "long"], repo.path()).unwrap();

        assert_eq!(
            GitBackend.changed_paths(repo.path(), "main").unwrap(),
            vec![long]
        );
    }

    #[test]
    fn the_default_remote_branch_comes_from_origin_head() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        Git::fixture(p)
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        std::fs::write(p.join("f"), "x").unwrap();
        Git::fixture(p).args(["add", "."]).output().unwrap();
        Git::fixture(p)
            .args(["commit", "-qm", "init"])
            .output()
            .unwrap();
        Git::fixture(p)
            .args([
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ])
            .output()
            .unwrap();
        assert_eq!(GitBackend.default_branch(p).unwrap(), "origin/main");
    }

    #[test]
    fn a_repo_without_origin_head_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        Git::fixture(p).args(["init", "-q"]).output().unwrap();
        assert!(GitBackend.default_branch(p).is_err());
    }

    /// The mark of a tree some repository still owns. An abandoned baseline
    /// keeps its `.git` file while the git directory it names is gone, and that
    /// is the tree a sweep reclaims as a plain directory.
    #[test]
    fn a_gitdir_resolves_only_while_something_stands_behind_it() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        assert!(
            matches!(gitdir_ownership(&tree), Ownership::Unowned),
            "no .git"
        );

        let admin = dir.path().join("admin");
        std::fs::write(tree.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        assert!(
            matches!(gitdir_ownership(&tree), Ownership::Unowned),
            "the target does not exist"
        );

        std::fs::create_dir_all(&admin).unwrap();
        assert!(
            matches!(gitdir_ownership(&tree), Ownership::Owned),
            "the target exists"
        );

        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert!(
            matches!(gitdir_ownership(&repo), Ownership::Owned),
            "a .git directory is a repository"
        );
    }
}
