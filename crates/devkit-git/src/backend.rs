//! Git behind [`VersionControl`].

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use devkit_vcs::{
    Changes, DETACHED, FileChange, NewWorktree, Ownership, Selection, VersionControl, Worktree,
};

use super::{Git, SLOW_TIMEOUT};

/// Holds no state: every call spawns git against the directory it is given.
#[derive(Debug, Clone, Copy, Default)]
pub struct GitBackend;

impl VersionControl for GitBackend {
    fn name(&self) -> &'static str {
        "git"
    }

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

    fn committed_file(&self, dir: &Path, path: &str) -> Result<Option<Vec<u8>>> {
        let out = Git::at(dir)
            .args(["cat-file", "blob", &format!("HEAD:{path}")])
            .wait()?;
        Ok(out.status.success().then_some(out.stdout))
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

    fn merge_base(&self, dir: &Path, a: &str, b: &str) -> Result<String> {
        let out = Git::at(dir)
            .args(["merge-base", "--end-of-options", a, b])
            .output()?;
        let sha = out.trim();
        anyhow::ensure!(!sha.is_empty(), "`git merge-base {a} {b}` named no commit");
        Ok(sha.to_string())
    }

    fn has_revision(&self, dir: &Path, rev: &str) -> Result<bool> {
        Git::at(dir)
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &format!("{rev}^{{commit}}"),
            ])
            .success()
    }

    fn changed_files(&self, dir: &Path, from: &str, to: &str) -> Result<Vec<FileChange>> {
        let out = Git::at(dir)
            .args([
                "diff",
                "--name-status",
                "-z",
                "-M",
                "--no-ext-diff",
                "--end-of-options",
                from,
                to,
            ])
            .output()?;
        parse_name_status(&out)
    }

    fn file_at(&self, dir: &Path, rev: &str, path: &str) -> Result<Option<String>> {
        let object = format!("{rev}:{path}");
        // The batch protocol reads one object name per line.
        if object.contains('\n') {
            return file_at_named(dir, &object);
        }
        let out = Git::at(dir)
            .args(["cat-file", "--batch"])
            .input(format!("{object}\n"))
            .output()?;
        let (header, rest) = out.split_once('\n').unwrap_or((&out, ""));
        if header.ends_with(" missing") || header.ends_with(" ambiguous") {
            return Ok(None);
        }
        let kind = header.split(' ').nth(1);
        anyhow::ensure!(
            kind == Some("blob"),
            "`{object}` names a {}, not a file",
            kind.unwrap_or("nothing git reports")
        );
        Ok(Some(rest.strip_suffix('\n').unwrap_or(rest).to_string()))
    }

    fn files_containing(&self, dir: &Path, rev: &str, needle: &str) -> Result<Vec<String>> {
        let out = Git::at(dir)
            .args([
                "grep",
                "-l",
                "-z",
                "--full-name",
                "-F",
                "-e",
                needle,
                rev,
                "--",
                ":/",
            ])
            .wait()?;
        match out.status.code() {
            Some(0) => {}
            Some(1) => return Ok(Vec::new()),
            _ => anyhow::bail!(
                "`git grep {needle} {rev}` failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        }
        let prefix = format!("{rev}:");
        Ok(String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(|p| p.strip_prefix(&prefix).unwrap_or(p).to_string())
            .collect())
    }

    fn restore_paths(&self, dir: &Path, rev: &str, paths: &[String]) -> Result<()> {
        // Chunked to stay under Windows' command-line length limit.
        for chunk in paths.chunks(100) {
            Git::at(dir)
                .args(["--literal-pathspecs", "checkout", rev, "--"])
                .args(chunk.iter().map(String::as_str))
                .network()
                .output()?;
        }
        Ok(())
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

    fn fetch_commits(&self, repo: &Path, remote: &str, refs: &[String]) -> Result<()> {
        Git::at(repo)
            .args(["fetch", "--no-write-fetch-head", "--no-tags", remote])
            .args(refs.iter().map(String::as_str))
            .network()
            .output()?;
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
        let fetched = Git::at(dir)
            .args(["rev-parse", "FETCH_HEAD"])
            .output()?
            .trim()
            .to_string();
        if self.has_branch(dir, branch)? {
            let local = Git::at(dir)
                .args(["rev-parse", &format!("refs/heads/{branch}")])
                .output()?
                .trim()
                .to_string();
            anyhow::ensure!(
                local == fetched,
                "local branch `{branch}` is at {local}, not at {remote_ref} ({fetched}); \
                 rename or delete it to check this out"
            );
        }
        Git::at(dir)
            .args(["checkout", "-B", branch, &fetched])
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

    fn author(&self, dir: &Path) -> Result<Option<(String, String)>> {
        // GIT_AUTHOR_* wins, and one half set alone does not mix with
        // config. author.* overrides user.*.
        let from_env = ["GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL"]
            .map(|key| std::env::var(key).ok().filter(|v| !v.trim().is_empty()));
        let [name, email] = match from_env {
            [Some(name), Some(email)] => [name, email],
            [None, None] => ["name", "email"].map(|half| {
                for key in [format!("author.{half}"), format!("user.{half}")] {
                    if let Ok(value) = Git::at(dir).args(["config", &key]).output() {
                        let value = value.trim().to_string();
                        if !value.is_empty() {
                            return value;
                        }
                    }
                }
                String::new()
            }),
            _ => return Ok(None),
        };
        Ok((!name.trim().is_empty() && !email.trim().is_empty()).then_some((name, email)))
    }

    fn head_author(&self, dir: &Path) -> Result<Option<(String, String)>> {
        let out = Git::at(dir)
            .args(["log", "-1", "--format=%an%x00%ae"])
            .wait()?;
        if !out.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut parts = text.trim_end_matches(['\n', '\r']).splitn(2, '\0');
        let name = parts.next().unwrap_or_default().to_string();
        let email = parts.next().unwrap_or_default().to_string();
        Ok((!name.trim().is_empty() && !email.trim().is_empty()).then_some((name, email)))
    }

    fn commit(&self, dir: &Path, selection: &Selection<'_>, message: &str) -> Result<String> {
        super::commit::commit(dir, selection, message)
    }
}

/// [`VersionControl::file_at`] for an object name the batch protocol cannot
/// carry: whether it exists, then what it holds.
fn file_at_named(dir: &Path, object: &str) -> Result<Option<String>> {
    if !Git::at(dir)
        .args(["cat-file", "-e", "--end-of-options", object])
        .success()?
    {
        return Ok(None);
    }
    let out = Git::at(dir).args(["cat-file", "blob", object]).wait()?;
    anyhow::ensure!(
        out.status.success(),
        "`git cat-file blob {object}` failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
}

/// `git diff --name-status -z` output: a status field, then one path, or two
/// for a rename or copy.
fn parse_name_status(out: &str) -> Result<Vec<FileChange>> {
    let mut fields = out.split('\0').filter(|f| !f.is_empty());
    let mut changes = Vec::new();
    while let Some(status) = fields.next() {
        let mut path = || {
            fields
                .next()
                .map(str::to_string)
                .with_context(|| format!("`git diff --name-status` status {status} has no path"))
        };
        let change = match status.chars().next() {
            Some('A') => FileChange {
                old: None,
                new: Some(path()?),
            },
            Some('D') => FileChange {
                old: Some(path()?),
                new: None,
            },
            Some('R') => FileChange {
                old: Some(path()?),
                new: Some(path()?),
            },
            Some('C') => {
                let _source = path()?;
                FileChange {
                    old: None,
                    new: Some(path()?),
                }
            }
            _ => {
                let p = path()?;
                FileChange {
                    old: Some(p.clone()),
                    new: Some(p),
                }
            }
        };
        changes.push(change);
    }
    Ok(changes)
}

/// A path handed to git as an argument. A lossy spelling would aim the call,
/// a removal included, at some other directory.
pub(crate) fn utf8(path: &Path) -> Result<&str> {
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

    #[test]
    fn changed_files_pairs_a_rename_and_file_at_reads_either_side() {
        let repo = repo_with_commit();
        std::fs::write(repo.path().join("keep.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        std::fs::write(repo.path().join("gone.txt"), "bye").unwrap();
        run(&["add", "."], repo.path()).unwrap();
        run(&["commit", "-qm", "base"], repo.path()).unwrap();
        run(&["checkout", "-qb", "side"], repo.path()).unwrap();
        run(&["mv", "keep.txt", "moved.txt"], repo.path()).unwrap();
        run(&["rm", "-q", "gone.txt"], repo.path()).unwrap();
        std::fs::write(repo.path().join("f.txt"), "y").unwrap();
        std::fs::write(repo.path().join("new.txt"), "n").unwrap();
        run(&["add", "."], repo.path()).unwrap();
        run(&["commit", "-qm", "side"], repo.path()).unwrap();

        let base = GitBackend.merge_base(repo.path(), "main", "HEAD").unwrap();
        let mut changes = GitBackend
            .changed_files(repo.path(), &base, "HEAD")
            .unwrap();
        changes.sort_by(|a, b| a.new.cmp(&b.new).then(a.old.cmp(&b.old)));
        let change = |old: Option<&str>, new: Option<&str>| FileChange {
            old: old.map(String::from),
            new: new.map(String::from),
        };
        assert_eq!(changes, vec![
            change(Some("gone.txt"), None),
            change(Some("f.txt"), Some("f.txt")),
            change(Some("keep.txt"), Some("moved.txt")),
            change(None, Some("new.txt")),
        ]);
        assert_eq!(
            GitBackend
                .file_at(repo.path(), &base, "keep.txt")
                .unwrap()
                .as_deref(),
            Some("one\ntwo\nthree\nfour\n")
        );
        assert_eq!(
            GitBackend.file_at(repo.path(), "HEAD", "keep.txt").unwrap(),
            None
        );
        assert_eq!(
            GitBackend.file_at(repo.path(), "nope", "f.txt").unwrap(),
            None
        );
        assert!(GitBackend.file_at(repo.path(), "HEAD", "").is_err());
        assert!(GitBackend.has_revision(repo.path(), "main").unwrap());
        assert!(!GitBackend.has_revision(repo.path(), "nope").unwrap());
    }

    #[test]
    fn files_containing_names_the_files_at_a_revision_holding_a_needle() {
        let repo = repo_with_commit();
        std::fs::create_dir_all(repo.path().join("src/deep")).unwrap();
        std::fs::write(repo.path().join("src/deep/a.rs"), "#[path = \"x.rs\"]\n").unwrap();
        std::fs::write(repo.path().join("b.rs"), "#[path = \"y.rs\"]\n").unwrap();
        std::fs::write(repo.path().join("c.rs"), "mod c;\n").unwrap();
        run(&["add", "."], repo.path()).unwrap();
        run(&["commit", "-qm", "files"], repo.path()).unwrap();
        std::fs::write(repo.path().join("c.rs"), "#[path = \"z.rs\"]\n").unwrap();

        let sub = repo.path().join("src");
        assert_eq!(
            GitBackend.files_containing(&sub, "HEAD", "#[path").unwrap(),
            vec!["b.rs".to_string(), "src/deep/a.rs".to_string()]
        );
        assert_eq!(
            GitBackend
                .files_containing(repo.path(), "HEAD", "nowhere")
                .unwrap(),
            Vec::<String>::new()
        );
        assert!(
            GitBackend
                .files_containing(repo.path(), "nope", "#[path")
                .is_err()
        );
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
