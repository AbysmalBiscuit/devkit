//! What devkit asks of a project's repository, whichever version control keeps
//! it. A backend implements [`VersionControl`] and joins the dispatch enum in
//! `devkit_common::vcs`.
//!
//! A worktree is one working copy of a repository: a git worktree, a jj
//! workspace. A branch is the name work lands on: a git branch, a jj bookmark.

// ambassador copies the trait's signatures verbatim into the dispatching
// crate, so they use absolute paths that must resolve here too.
extern crate self as devkit_vcs;

use std::path::{Path, PathBuf};

/// What [`Worktree::branch`] and [`VersionControl::branch`] read when no
/// branch is checked out.
pub const DETACHED: &str = "DETACHED";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// [`DETACHED`] when the worktree has no branch checked out.
    pub branch: String,
    /// The repository has no working copy here, as a bare git repository has,
    /// so it holds no config.
    pub bare: bool,
    /// Locked against removal. Git refuses to remove a locked worktree for a
    /// single `--force`, so a caller that removes worktrees must ask.
    pub locked: bool,
}

/// Which changes [`VersionControl::dirty`] counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Changes {
    /// Modified, staged and untracked files.
    All,
    /// Modified and staged files. Untracked files are ignored.
    Tracked,
}

/// Whether a repository still stands behind a directory. Only
/// [`Ownership::Unowned`] may reach a deletion, so every read failure is
/// `Unknown`, never `Unowned`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    /// The directory is a repository, or a worktree whose repository is there.
    Owned,
    /// No repository marker, or one pointing at a repository that is gone.
    Unowned,
    /// The marker could not be read or resolved.
    Unknown,
}

/// A worktree for [`VersionControl::create_worktree`] to make.
#[derive(Debug, Clone, Copy)]
pub struct NewWorktree<'a> {
    /// The primary checkout the worktree hangs off.
    pub main: &'a Path,
    /// Where the new worktree goes. The backend creates it.
    pub path: &'a Path,
    /// The revision it starts from, e.g. `origin/main` or a commit id.
    pub start: &'a str,
    /// The new branch it takes, or `None` for a worktree that occupies no
    /// branch name. A new branch tracks nothing, so a plain push publishes it
    /// under its own name.
    pub branch: Option<&'a str>,
}

/// What [`VersionControl::commit`] records.
#[derive(Debug, Clone, Copy)]
pub enum Selection<'a> {
    /// These paths as the working tree has them, new and deleted files
    /// included. A relative path is taken from the directory `commit` is
    /// given.
    Paths(&'a [PathBuf]),
    /// The hunks of a patch against the current revision, whatever the
    /// working tree holds.
    Patch(&'a Path),
    /// No change: a new message for the last commit.
    Amend,
}

/// Every question and action devkit puts to a project's repository. Each call
/// is time-bounded, and none prompts.
#[ambassador::delegatable_trait]
pub trait VersionControl {
    /// The backend's name as messages print it, e.g. `git`.
    fn name(&self) -> &'static str;

    /// Whether `dir` sits in this backend's repository, by a filesystem look
    /// alone. May answer true when unsure. The backend's own tool has the
    /// final word.
    fn claims(&self, dir: &::std::path::Path) -> bool;

    /// The worktree containing `dir`. `Ok(None)` means the tool ran and found
    /// no repository. `Err` means it could not run, which a caller must not
    /// read as "no repository": that scopes it to the wrong root whenever the
    /// tool is merely unavailable.
    fn root(
        &self,
        dir: &::std::path::Path,
    ) -> ::anyhow::Result<::std::option::Option<::std::path::PathBuf>>;

    /// Every worktree of `dir`'s repository, main first, split into
    /// `Ok(None)` and `Err` as [`VersionControl::root`] is.
    fn worktrees(
        &self,
        dir: &::std::path::Path,
    ) -> ::anyhow::Result<::std::option::Option<::std::vec::Vec<::devkit_vcs::Worktree>>>;

    /// Whether a repository stands behind `dir` itself, not behind an
    /// ancestor of it. A filesystem read, never a process.
    fn ownership(&self, dir: &::std::path::Path) -> ::devkit_vcs::Ownership;

    /// The branch checked out at `dir`, or [`DETACHED`].
    fn branch(&self, dir: &::std::path::Path) -> ::anyhow::Result<::std::string::String>;

    /// The full id of the commit checked out at `dir`.
    fn revision(&self, dir: &::std::path::Path) -> ::anyhow::Result<::std::string::String>;

    /// How many commits at `dir`'s head `base` does not reach. `Err` when
    /// `base` is not a commit the local repository has.
    fn ahead(&self, dir: &::std::path::Path, base: &str) -> ::anyhow::Result<u32>;

    fn has_branch(&self, repo: &::std::path::Path, name: &str) -> ::anyhow::Result<bool>;

    /// Deletes `name` whether or not it has landed anywhere.
    fn delete_branch(&self, repo: &::std::path::Path, name: &str) -> ::anyhow::Result<()>;

    /// The remote's default branch as a revision, e.g. `origin/main`.
    fn default_branch(&self, repo: &::std::path::Path) -> ::anyhow::Result<::std::string::String>;

    fn dirty(
        &self,
        dir: &::std::path::Path,
        changes: ::devkit_vcs::Changes,
    ) -> ::anyhow::Result<bool>;

    /// The commit `dir`'s head forked from `target` at. Reads local history
    /// only, so no fetch is needed.
    fn fork_point(
        &self,
        dir: &::std::path::Path,
        target: &str,
    ) -> ::anyhow::Result<::std::string::String>;

    /// Paths changed on `dir`'s head since it forked from `base`, relative to
    /// the repository root.
    fn changed_paths(
        &self,
        dir: &::std::path::Path,
        base: &str,
    ) -> ::anyhow::Result<::std::vec::Vec<::std::string::String>>;

    /// Creates a worktree. Populating it can fetch the objects it needs.
    fn create_worktree(&self, new: &::devkit_vcs::NewWorktree<'_>) -> ::anyhow::Result<()>;

    /// Removes the worktree at `path` from `main`'s repository. Without
    /// `force` it refuses over uncommitted work.
    fn remove_worktree(
        &self,
        main: &::std::path::Path,
        path: &::std::path::Path,
        force: bool,
    ) -> ::anyhow::Result<()>;

    /// Forgets worktrees whose directories are gone.
    fn prune(&self, main: &::std::path::Path) -> ::anyhow::Result<()>;

    fn fetch(&self, repo: &::std::path::Path, remote: &str) -> ::anyhow::Result<()>;

    /// Publishes `branch` to `remote` and tracks it there. Never forces.
    fn push(&self, dir: &::std::path::Path, remote: &str, branch: &str) -> ::anyhow::Result<()>;

    /// Whether the commit checked out at `dir` is on some remote branch, as of
    /// the last fetch.
    fn pushed(&self, dir: &::std::path::Path) -> ::anyhow::Result<bool>;

    /// Fetches `remote_ref` from `remote` and checks it out at `dir` as
    /// `branch`. A local branch of that name already at the fetched commit is
    /// reused; one anywhere else is an error naming it, since moving it could
    /// orphan unpushed work. The ref need not sit under a branch namespace, as
    /// a forge's `refs/pull/7/head` does not.
    fn checkout_remote_ref(
        &self,
        dir: &::std::path::Path,
        remote: &str,
        remote_ref: &str,
        branch: &str,
    ) -> ::anyhow::Result<()>;

    fn remote_url(
        &self,
        dir: &::std::path::Path,
        remote: &str,
    ) -> ::anyhow::Result<::std::string::String>;

    /// The email the repository records authors under. `Err` when none is
    /// configured.
    fn user_email(&self, dir: &::std::path::Path) -> ::anyhow::Result<::std::string::String>;

    /// Commits `selection` in `dir`'s repository with `message`, leaving the
    /// working tree, and changes anyone else staged, as they were. Refuses
    /// rather than committing part of the selection. Returns the backend's
    /// report of the new commit.
    ///
    /// The default refuses, naming the backend: one without a staging area
    /// may have no use for this.
    fn commit(
        &self,
        dir: &::std::path::Path,
        selection: &::devkit_vcs::Selection<'_>,
        message: &str,
    ) -> ::anyhow::Result<::std::string::String> {
        let _ = (dir, selection, message);
        ::anyhow::bail!(
            "the {} backend cannot commit: `devkit commit` is unavailable in this repository",
            self.name()
        )
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// A backend that answers nothing but its name.
    struct Mute;

    impl VersionControl for Mute {
        fn name(&self) -> &'static str {
            "mute"
        }

        fn claims(&self, _: &Path) -> bool {
            unimplemented!()
        }

        fn root(&self, _: &Path) -> anyhow::Result<Option<PathBuf>> {
            unimplemented!()
        }

        fn worktrees(&self, _: &Path) -> anyhow::Result<Option<Vec<Worktree>>> {
            unimplemented!()
        }

        fn ownership(&self, _: &Path) -> Ownership {
            unimplemented!()
        }

        fn branch(&self, _: &Path) -> anyhow::Result<String> {
            unimplemented!()
        }

        fn revision(&self, _: &Path) -> anyhow::Result<String> {
            unimplemented!()
        }

        fn ahead(&self, _: &Path, _: &str) -> anyhow::Result<u32> {
            unimplemented!()
        }

        fn has_branch(&self, _: &Path, _: &str) -> anyhow::Result<bool> {
            unimplemented!()
        }

        fn delete_branch(&self, _: &Path, _: &str) -> anyhow::Result<()> {
            unimplemented!()
        }

        fn default_branch(&self, _: &Path) -> anyhow::Result<String> {
            unimplemented!()
        }

        fn dirty(&self, _: &Path, _: Changes) -> anyhow::Result<bool> {
            unimplemented!()
        }

        fn fork_point(&self, _: &Path, _: &str) -> anyhow::Result<String> {
            unimplemented!()
        }

        fn changed_paths(&self, _: &Path, _: &str) -> anyhow::Result<Vec<String>> {
            unimplemented!()
        }

        fn create_worktree(&self, _: &NewWorktree<'_>) -> anyhow::Result<()> {
            unimplemented!()
        }

        fn remove_worktree(&self, _: &Path, _: &Path, _: bool) -> anyhow::Result<()> {
            unimplemented!()
        }

        fn prune(&self, _: &Path) -> anyhow::Result<()> {
            unimplemented!()
        }

        fn fetch(&self, _: &Path, _: &str) -> anyhow::Result<()> {
            unimplemented!()
        }

        fn push(&self, _: &Path, _: &str, _: &str) -> anyhow::Result<()> {
            unimplemented!()
        }

        fn pushed(&self, _: &Path) -> anyhow::Result<bool> {
            unimplemented!()
        }

        fn checkout_remote_ref(&self, _: &Path, _: &str, _: &str, _: &str) -> anyhow::Result<()> {
            unimplemented!()
        }

        fn remote_url(&self, _: &Path, _: &str) -> anyhow::Result<String> {
            unimplemented!()
        }

        fn user_email(&self, _: &Path) -> anyhow::Result<String> {
            unimplemented!()
        }
    }

    #[test]
    fn a_backend_without_commit_refuses_naming_itself() {
        for selection in [
            Selection::Paths(&[]),
            Selection::Patch(Path::new("p.diff")),
            Selection::Amend,
        ] {
            let err = Mute
                .commit(Path::new("."), &selection, "feat: x")
                .unwrap_err()
                .to_string();
            assert!(err.contains("mute backend"), "{err}");
        }
    }
}
