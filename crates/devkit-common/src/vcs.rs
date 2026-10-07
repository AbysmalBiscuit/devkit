//! The project repository, reached through whichever backend keeps it. Every
//! question devkit asks about the repository it coordinates goes through here;
//! [`devkit_git::Git`] is only for what is git by definition.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use ambassador::Delegate;
use anyhow::{Context, Result};
use devkit_git::GitBackend;
/// The hidden verb `devkit commit`'s hook wrappers run, and its body.
pub use devkit_git::{COMMIT_HOOK_VERB, run_commit_hook};
pub use devkit_vcs::{
    Changes, DETACHED, NewWorktree, Ownership, Selection, VersionControl, Worktree,
    ambassador_impl_VersionControl,
};
use strum::{EnumIter, IntoEnumIterator};

use crate::paths::same_path;

/// Every backend devkit can drive, in the order they claim a directory.
#[derive(Debug, Clone, Copy, Delegate, EnumIter)]
#[delegate(VersionControl)]
pub enum Vcs {
    Git(GitBackend),
}

impl Vcs {
    /// The backend for `dir`: the first that claims it, else git, so a
    /// directory in no repository gets git's own error.
    pub fn at(dir: &Path) -> Self {
        Self::iter()
            .find(|vcs| vcs.claims(dir))
            .unwrap_or(Self::Git(GitBackend))
    }
}

/// The checkout containing `start`. Errors when `start` is not in a
/// repository; a caller wanting a fallback declares one.
pub fn checkout_root(start: &Path) -> Result<PathBuf> {
    checkout_root_opt(start)?
        .with_context(|| format!("not inside a repository: {}", start.display()))
}

/// The checkout containing `start`, with [`VersionControl::root`]'s split
/// between "no repository here" (`Ok(None)`) and "could not ask" (`Err`).
pub fn checkout_root_opt(start: &Path) -> Result<Option<PathBuf>> {
    Vcs::at(start).root(start)
}

/// Every worktree of `start`'s repository, main first.
pub fn worktrees(start: &Path) -> Result<Vec<Worktree>> {
    Vcs::at(start)
        .worktrees(start)?
        .with_context(|| format!("not inside a repository: {}", start.display()))
}

/// `start`'s repository's main checkout, or `None` when `start` is already in
/// it and when the main worktree is bare. The backend names the main worktree
/// itself: the parent of git's common directory cannot tell a real main
/// worktree from a bare repository at `/x/.git`.
pub fn main_checkout(start: &Path) -> Result<Option<PathBuf>> {
    // A bare repository has no checkout root to ask for, so the listing
    // answers first.
    let Some(main) = non_bare_main(start)? else {
        return Ok(None);
    };
    let here = checkout_root(start)?;
    Ok((!same_path(&main, &here)).then_some(main))
}

/// [`main_checkout`] for a caller that already knows `start`'s checkout root,
/// which costs a subprocess to resolve again.
pub fn main_checkout_from(start: &Path, here: &Path) -> Result<Option<PathBuf>> {
    Ok(non_bare_main(start)?.filter(|main| !same_path(main, here)))
}

/// The repository's primary checkout as seen from `start`: its main worktree
/// when `start` is a linked worktree, else `start`'s own checkout root. This
/// is the directory worktrees are added to and removed from, fetched into, and
/// backfilled from, so every caller that needs "the checkout the worktrees
/// hang off" asks for it here rather than deriving it from a configured path
/// or a directory name.
///
/// Errors when `start` is not inside a repository, inheriting
/// `checkout_root`'s message. A bare main worktree has no working tree to
/// return, so `start`'s own checkout answers.
pub fn primary_checkout(start: &Path) -> Result<PathBuf> {
    match main_checkout(start)? {
        Some(main) => Ok(main),
        None => checkout_root(start),
    }
}

/// The repository's main worktree, or `None` when it is bare. Distinct from
/// [`primary_checkout`], which falls back to the caller's own checkout: from a
/// linked worktree of a bare repository that fallback names the linked worktree
/// itself, so anything derived per-repository must not use it.
pub fn non_bare_main(start: &Path) -> Result<Option<PathBuf>> {
    Ok(worktrees(start)?
        .into_iter()
        .next()
        .filter(|w| !w.bare)
        .map(|w| w.path))
}

/// The conventional worktree directory for a checkout: its own name plus
/// `_worktrees`, beside it. The underscore separates the suffix from a project
/// name, which commonly contains hyphens.
pub fn derived_worktree_root(primary: &Path) -> Option<PathBuf> {
    let name = primary.file_name()?.to_str()?;
    Some(primary.parent()?.join(format!("{name}_worktrees")))
}

/// One directory's answer to the two questions every hook-path helper asks:
/// which worktree contains it, and where the repository's main worktree is.
/// Both come from one worktree listing, so a hook invocation asks the backend
/// once.
///
/// Resolution is lazy, so a caller that turns out not to need a checkout
/// spawns nothing. It is per-value and never process-wide, because `devkitd`
/// and the MCP server outlive the worktrees they serve.
#[derive(Debug, Clone)]
pub struct Checkout {
    start: PathBuf,
    resolved: OnceLock<Resolved>,
}

#[derive(Debug, Clone)]
struct Resolved {
    vcs: Vcs,
    /// Every worktree of `start`'s repository, main first. Empty when the
    /// backend reported no repository, and when it could not be run.
    worktrees: Vec<Worktree>,
    /// Index into `worktrees` of the one containing `start`.
    here: Option<usize>,
    /// Why the backend could not be run, which a caller must not fold into
    /// "no repository".
    error: Option<String>,
}

impl Checkout {
    /// A checkout resolved from `start` when something first asks for it.
    pub fn at(start: &Path) -> Self {
        Self {
            start: start.to_path_buf(),
            resolved: OnceLock::new(),
        }
    }

    /// The directory this was resolved from.
    pub fn dir(&self) -> &Path {
        &self.start
    }

    fn resolved(&self) -> &Resolved {
        self.resolved.get_or_init(|| {
            let vcs = Vcs::at(&self.start);
            let (worktrees, error) = match vcs.worktrees(&self.start) {
                Ok(listed) => (listed.unwrap_or_default(), None),
                Err(e) => (Vec::new(), Some(format!("{e:#}"))),
            };
            let here = longest_containing(&worktrees, &self.start);
            Resolved {
                vcs,
                worktrees,
                here,
                error,
            }
        })
    }

    /// The working tree containing the directory this was resolved from, or
    /// `None` outside one. The counterpart of [`checkout_root_opt`].
    pub fn root(&self) -> Option<&Path> {
        let r = self.resolved();
        r.here.map(|i| r.worktrees[i].path.as_path())
    }

    /// Why the backend could not be run, when that is why [`Checkout::root`]
    /// is `None`. `None` here means it ran: either it found a checkout or it
    /// reported none.
    pub fn error(&self) -> Option<&str> {
        self.resolved().error.as_deref()
    }

    /// The repository's main worktree, or `None` when it is bare and when the
    /// backend could not answer. The counterpart of [`non_bare_main`].
    pub fn main_worktree(&self) -> Option<&Path> {
        self.resolved()
            .worktrees
            .first()
            .filter(|w| !w.bare)
            .map(|w| w.path.as_path())
    }

    /// The main worktree as [`main_checkout`] reports it: `None` when the
    /// directory this was resolved from is already in it. Read off the
    /// listing's index rather than by comparing paths, so two spellings of
    /// one directory cannot read as two checkouts.
    pub fn main_checkout(&self) -> Option<&Path> {
        (self.resolved().here != Some(0))
            .then(|| self.main_worktree())
            .flatten()
    }

    /// Every worktree of the repository, main first. Empty outside one and
    /// when the backend could not be run.
    pub fn worktrees(&self) -> &[Worktree] {
        &self.resolved().worktrees
    }

    /// The worktree containing the directory this was resolved from.
    pub fn here(&self) -> Option<&Worktree> {
        let r = self.resolved();
        r.here.map(|i| &r.worktrees[i])
    }

    /// The working tree containing `dir`, which can sit in any worktree of
    /// this repository or in none of them.
    ///
    /// `None` means the listing does not settle it and the caller must ask
    /// about `dir` itself: `dir` is outside every listed worktree, or inside a
    /// nested repository such as a submodule, which the listing does not
    /// describe.
    pub fn checkout_of(&self, dir: &Path) -> Option<&Path> {
        let r = self.resolved();
        let root = r
            .worktrees
            .get(longest_containing(&r.worktrees, dir)?)?
            .path
            .as_path();
        let (parent, child) = containment(root, dir, std::fs::canonicalize(dir).ok().as_deref())?;
        child
            .ancestors()
            .take_while(|d| *d != parent)
            .all(|d| r.vcs.ownership(d) == Ownership::Unowned)
            .then_some(root)
    }
}

/// The index of the deepest non-bare worktree containing `dir`, measuring
/// depth by how far `dir` sits below it so that two listings in different
/// spellings still compare.
fn longest_containing(all: &[Worktree], dir: &Path) -> Option<usize> {
    let canon = std::fs::canonicalize(dir).ok();
    let mut best: Option<(usize, usize)> = None;
    for (i, w) in all.iter().enumerate() {
        if w.bare {
            continue;
        }
        let Some((parent, child)) = containment(&w.path, dir, canon.as_deref()) else {
            continue;
        };
        let below = child
            .strip_prefix(&parent)
            .map_or(usize::MAX, |rel| rel.components().count());
        if best.is_none_or(|(_, seen)| below < seen) {
            best = Some((i, below));
        }
    }
    best.map(|(i, _)| i)
}

/// `parent` and `child` in one spelling that shows the containment, or `None`
/// when `parent` does not contain `child`. A lexical answer settles the common
/// case without touching the filesystem; a symlinked working directory — which
/// git reports resolved, since it reads the directory rather than the spelling
/// used to reach it — needs both sides resolved before they compare.
fn containment(
    parent: &Path,
    child: &Path,
    child_canon: Option<&Path>,
) -> Option<(PathBuf, PathBuf)> {
    if child.starts_with(parent) {
        return Some((parent.to_path_buf(), child.to_path_buf()));
    }
    let resolved_parent = std::fs::canonicalize(parent).ok()?;
    let resolved_child = child_canon?;
    resolved_child
        .starts_with(&resolved_parent)
        .then(|| (resolved_parent, resolved_child.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use devkit_git::Git;

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
    fn main_checkout_is_none_in_the_main_checkout() {
        let repo = repo_with_commit();
        assert_eq!(main_checkout(repo.path()).unwrap(), None);
    }

    #[test]
    fn linked_worktree_resolves_its_main_checkout() {
        let repo = repo_with_commit();
        let holder = tempfile::tempdir().unwrap();
        let linked = holder.path().join("wt");
        run(
            &[
                "worktree",
                "add",
                "-q",
                linked.to_str().unwrap(),
                "-b",
                "side",
            ],
            repo.path(),
        )
        .unwrap();

        let found = main_checkout(&linked).unwrap().expect("a main checkout");
        assert_eq!(
            std::fs::canonicalize(found).unwrap(),
            std::fs::canonicalize(repo.path()).unwrap()
        );
    }

    #[test]
    fn primary_checkout_of_the_main_checkout_is_itself() {
        let repo = repo_with_commit();
        assert_eq!(
            std::fs::canonicalize(primary_checkout(repo.path()).unwrap()).unwrap(),
            std::fs::canonicalize(repo.path()).unwrap()
        );
    }

    /// The directory name carries no meaning: a linked worktree resolves to
    /// whatever git names as the main worktree.
    #[test]
    fn primary_checkout_of_a_linked_worktree_is_the_main_one() {
        let repo = repo_with_commit();
        let holder = tempfile::tempdir().unwrap();
        let linked = holder.path().join("wt");
        run(
            &[
                "worktree",
                "add",
                "-q",
                linked.to_str().unwrap(),
                "-b",
                "side",
            ],
            repo.path(),
        )
        .unwrap();

        assert_eq!(
            std::fs::canonicalize(primary_checkout(&linked).unwrap()).unwrap(),
            std::fs::canonicalize(repo.path()).unwrap()
        );
    }

    #[test]
    fn primary_checkout_errors_outside_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        assert!(primary_checkout(dir.path()).is_err());
    }

    /// A bare repository has no main working tree, so there is no checkout to
    /// inherit config from.
    #[test]
    fn bare_main_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("b.git");
        Git::fixture(dir.path())
            .args(["init", "-q", "--bare", bare.to_str().unwrap()])
            .output()
            .unwrap();
        assert_eq!(main_checkout(&bare).unwrap(), None);
    }

    #[test]
    fn checkout_root_errors_outside_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        assert!(checkout_root(dir.path()).is_err());
    }

    /// Outside a repository git runs and answers "no repository here" —
    /// distinct from git failing to run at all, which surfaces as `Err`
    /// instead. Forcing the `Err` arm would mean making the `git` spawn
    /// itself fail (missing binary, broken `PATH`), which this suite has no
    /// clean way to do without mutating process-wide environment state that
    /// other tests running concurrently in the same binary would also see.
    #[test]
    fn checkout_root_opt_distinguishes_no_repository_from_a_git_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(checkout_root_opt(dir.path()).unwrap(), None);
    }

    /// One listing has to answer what `rev-parse --show-toplevel` and
    /// `worktree list` used to answer separately, in both directions.
    #[test]
    fn checkout_answers_root_and_main_from_one_listing() {
        let repo = repo_with_commit();
        let holder = tempfile::tempdir().unwrap();
        let linked = holder.path().join("wt");
        run(
            &[
                "worktree",
                "add",
                "-q",
                linked.to_str().unwrap(),
                "-b",
                "side",
            ],
            repo.path(),
        )
        .unwrap();

        let here = Checkout::at(repo.path());
        assert_eq!(
            std::fs::canonicalize(here.root().unwrap()).unwrap(),
            std::fs::canonicalize(repo.path()).unwrap()
        );
        assert_eq!(here.main_checkout(), None);

        let there = Checkout::at(&linked);
        assert_eq!(
            std::fs::canonicalize(there.root().unwrap()).unwrap(),
            std::fs::canonicalize(&linked).unwrap()
        );
        assert_eq!(
            std::fs::canonicalize(there.main_checkout().unwrap()).unwrap(),
            std::fs::canonicalize(repo.path()).unwrap()
        );
    }

    #[test]
    fn checkout_lists_the_main_worktree_first_and_names_the_one_here() {
        let repo = repo_with_commit();
        let holder = tempfile::tempdir().unwrap();
        let linked = holder.path().join("wt");
        run(
            &[
                "worktree",
                "add",
                "-q",
                linked.to_str().unwrap(),
                "-b",
                "side",
            ],
            repo.path(),
        )
        .unwrap();
        let there = Checkout::at(&linked);
        assert_eq!(
            std::fs::canonicalize(&there.worktrees()[0].path).unwrap(),
            std::fs::canonicalize(repo.path()).unwrap()
        );
        assert_eq!(there.here().unwrap().branch, "side");
        let outside = tempfile::tempdir().unwrap();
        assert!(Checkout::at(outside.path()).here().is_none());
    }

    /// A directory below the checkout resolves to the checkout, not to itself.
    #[test]
    fn checkout_resolves_a_nested_directory_to_its_working_tree() {
        let repo = repo_with_commit();
        let nested = repo.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            std::fs::canonicalize(Checkout::at(&nested).root().unwrap()).unwrap(),
            std::fs::canonicalize(repo.path()).unwrap()
        );
    }

    /// git reports the directory it resolved, not the spelling used to reach
    /// it, so a symlinked start compares only once both sides are resolved.
    #[cfg(unix)]
    #[test]
    fn checkout_resolves_a_symlinked_start() {
        let repo = repo_with_commit();
        let holder = tempfile::tempdir().unwrap();
        let link = holder.path().join("link");
        std::os::unix::fs::symlink(repo.path(), &link).unwrap();

        let checkout = Checkout::at(&link);
        assert_eq!(
            std::fs::canonicalize(checkout.root().unwrap()).unwrap(),
            std::fs::canonicalize(repo.path()).unwrap()
        );
        assert_eq!(checkout.main_checkout(), None);
    }

    /// A bare main worktree has no working tree to name, and it is not a
    /// candidate for the enclosing checkout either.
    #[test]
    fn checkout_of_a_linked_worktree_of_a_bare_repository_has_no_main() {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("b.git");
        Git::fixture(dir.path())
            .args(["init", "-q", "--bare", bare.to_str().unwrap()])
            .output()
            .unwrap();
        // A bare repository has no commit to branch from until one is pushed
        // into it, so the fixture clones a populated repository instead.
        let src = repo_with_commit();
        Git::fixture(&bare)
            .args(["fetch", src.path().to_str().unwrap(), "main:main"])
            .output()
            .unwrap();
        let linked = dir.path().join("wt");
        Git::fixture(&bare)
            .args(["worktree", "add", "-q", linked.to_str().unwrap(), "main"])
            .output()
            .unwrap();

        let checkout = Checkout::at(&linked);
        assert_eq!(
            std::fs::canonicalize(checkout.root().unwrap()).unwrap(),
            std::fs::canonicalize(&linked).unwrap()
        );
        assert_eq!(checkout.main_worktree(), None);
        assert_eq!(checkout.main_checkout(), None);
    }

    /// Outside a repository git runs and answers nothing, which is not the
    /// same as git failing to run — `error` stays empty.
    #[test]
    fn checkout_outside_a_repository_is_empty_without_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let checkout = Checkout::at(dir.path());
        assert_eq!(checkout.root(), None);
        assert_eq!(checkout.main_worktree(), None);
        assert_eq!(checkout.error(), None);
    }

    /// The listing answers for any directory inside the worktrees it names,
    /// so a write into a sibling worktree costs no second git call.
    #[test]
    fn checkout_of_answers_for_a_sibling_worktree() {
        let repo = repo_with_commit();
        let holder = tempfile::tempdir().unwrap();
        let linked = holder.path().join("wt");
        run(
            &[
                "worktree",
                "add",
                "-q",
                linked.to_str().unwrap(),
                "-b",
                "side",
            ],
            repo.path(),
        )
        .unwrap();
        let nested = linked.join("a");
        std::fs::create_dir_all(&nested).unwrap();

        let checkout = Checkout::at(repo.path());
        assert_eq!(
            std::fs::canonicalize(checkout.checkout_of(&nested).unwrap()).unwrap(),
            std::fs::canonicalize(&linked).unwrap()
        );
    }

    /// A submodule is a repository of its own that git discovers before the
    /// enclosing worktree, and this listing does not describe it. Answering
    /// with the enclosing checkout would scope its files to the wrong root, so
    /// the question is handed back to the caller.
    #[test]
    fn checkout_of_declines_a_nested_repository() {
        let repo = repo_with_commit();
        let inner = repo.path().join("vendor/lib");
        std::fs::create_dir_all(&inner).unwrap();
        Git::fixture(&inner)
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();

        let checkout = Checkout::at(repo.path());
        assert_eq!(checkout.checkout_of(&inner), None);
        assert_eq!(checkout.checkout_of(&inner.join("src")), None);
        // The directory above the nested repository is still answerable.
        assert_eq!(
            std::fs::canonicalize(checkout.checkout_of(&repo.path().join("vendor")).unwrap())
                .unwrap(),
            std::fs::canonicalize(repo.path()).unwrap()
        );
    }

    #[test]
    fn checkout_of_declines_a_directory_outside_every_worktree() {
        let repo = repo_with_commit();
        let elsewhere = tempfile::tempdir().unwrap();
        assert_eq!(
            Checkout::at(repo.path()).checkout_of(elsewhere.path()),
            None
        );
    }

    #[test]
    fn the_derived_worktree_root_is_the_underscore_sibling() {
        let got = derived_worktree_root(Path::new("/home/lev/Git/lev/devkit"));
        assert_eq!(
            got,
            Some(PathBuf::from("/home/lev/Git/lev/devkit_worktrees"))
        );
    }

    #[test]
    fn a_path_with_no_parent_derives_nothing() {
        assert_eq!(derived_worktree_root(Path::new("/")), None);
    }

    #[test]
    fn a_bare_main_worktree_has_no_non_bare_main() {
        let tmp = tempfile::tempdir().unwrap();
        let bare = tmp.path().join("origin.git");
        let seed = tmp.path().join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        let git = |cwd: &Path, args: &[&str]| {
            Git::fixture(cwd)
                .args(args.iter().copied())
                .output()
                .unwrap()
        };
        git(&seed, &["init", "-q", "-b", "main"]);
        std::fs::write(seed.join("f"), "x").unwrap();
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-qm", "init"]);
        git(tmp.path(), &[
            "clone",
            "-q",
            "--bare",
            seed.to_str().unwrap(),
            bare.to_str().unwrap(),
        ]);

        // A linked worktree of a bare repository: `checkout_root` succeeds and
        // names this worktree, so deriving from it would give every worktree
        // its own root. `non_bare_main` is the value that must stay
        // empty.
        let wt = tmp.path().join("wt");
        git(&bare, &[
            "worktree",
            "add",
            "--detach",
            wt.to_str().unwrap(),
        ]);
        assert_eq!(non_bare_main(&wt).unwrap(), None);
    }
}
