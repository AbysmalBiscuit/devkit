//! The single door to git. Every git invocation in the workspace is built here
//! so that three properties hold everywhere rather than nowhere: the
//! environment cannot redirect the call to another repository or inject
//! config into it, a git that stops responding cannot block its caller
//! forever, and large output cannot deadlock the caller either.
//!
//! The inner `Command` is private on purpose. Handing one back would let a
//! caller finish it with `output()`, which has no timeout — and this runs on
//! the write path.

use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Default backstop for a git that never returns. Long enough that no healthy
/// call reaches it, short enough that a wedged one fails instead of hanging a
/// write through the PreToolUse hook. Override with `.timeout()`.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Timeout for a git call that is slow by nature rather than by accident:
/// anything that reaches the network (`clone`, `fetch`, `push`), and anything
/// that writes a whole working tree (`worktree add`/`remove`, `checkout`).
/// The two overlap — `worktree add` against a
/// `--filter=blob:none` clone fetches every blob the new working tree needs
/// at that commit, so it is a network call as well as a bulk one, and belongs
/// in this tier for both reasons.
pub const SLOW_TIMEOUT: Duration = Duration::from_secs(600);

/// Variables that repoint git at a different repository or inject config into
/// every invocation. Left in place, any of them silently changes which
/// `devkit.toml` devkit reads — and that file carries `[apps] launch` and
/// `[tasks] run`, which devkit executes. `GIT_CONFIG_COUNT` is the one that
/// matters most here: without it git ignores any `GIT_CONFIG_KEY_n` /
/// `GIT_CONFIG_VALUE_n` pair, including one that sets `core.fsmonitor` to an
/// arbitrary command. `GIT_CEILING_DIRECTORIES` is stripped alongside it
/// because it can turn off repository discovery entirely, turning a
/// legitimate call into a false "not a git repository".
const REDIRECTING_VARS: [&str; 6] = [
    "GIT_DIR",
    "GIT_COMMON_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_CONFIG_COUNT",
    "GIT_CEILING_DIRECTORIES",
];

/// A git invocation under construction. Configure freely; the only way to run
/// one is a terminal method here, which is what makes the timeout unskippable.
pub struct Git {
    command: Command,
    args: Vec<String>,
    timeout: Duration,
}

impl Git {
    /// Run git against `cwd`. The working directory is taken before any
    /// argument because it decides which git to spawn, not merely where to run
    /// it — see the module docs on WSL.
    ///
    /// The path goes on the `Command` as the raw `OsStr`; only a lossy copy
    /// goes into the args vector kept for error messages and the timing span.
    /// A non-UTF-8 path is real on Linux, and `to_string_lossy` rewrites its
    /// bytes to U+FFFD — sending that to git points `-C` at a path that does
    /// not exist.
    pub fn at(cwd: &Path) -> Self {
        let mut git = Self::bare();
        git.command.arg("-C").arg(cwd);
        git.args.push("-C".to_string());
        git.args.push(cwd.to_string_lossy().into_owned());
        git
    }

    /// Run git with no working directory — `clone`, which has no repository to
    /// run inside yet.
    pub fn bare() -> Self {
        let mut command = Command::new("git");
        for var in REDIRECTING_VARS {
            command.env_remove(var);
        }
        // A credential helper that prompts on an inherited stdin would block
        // forever against an unreachable or auth-required remote, and in an
        // interactive caller would swallow the next line the user types.
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Self {
            command,
            args: Vec::new(),
            timeout: TIMEOUT,
        }
    }

    /// A git invocation for building a test fixture: the developer's real
    /// global and system config are ignored, and identity comes from the
    /// environment rather than `git config`.
    ///
    /// Production never scrubs git config — credential helpers, aliases, and
    /// signing keys belong to the user and have to work. Identity is set
    /// through the environment because a `git config` call that loses its
    /// working directory writes into whatever repository it lands in.
    #[cfg(any(test, feature = "test-support"))]
    pub fn fixture(cwd: &Path) -> Self {
        Self::at(cwd)
            .env("GIT_CONFIG_GLOBAL", NULL_DEVICE)
            .env("GIT_CONFIG_SYSTEM", NULL_DEVICE)
            .env("GIT_AUTHOR_NAME", "devkit test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "devkit test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
    }

    pub fn args<'a>(mut self, args: impl IntoIterator<Item = &'a str>) -> Self {
        for arg in args {
            self.command.arg(arg);
            self.args.push(arg.to_string());
        }
        self
    }

    /// Set one of git's own behavior variables, e.g. `GIT_NO_LAZY_FETCH`.
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.command.env(key, value);
        self
    }

    /// Override the default timeout. Pass `SLOW_TIMEOUT` for a call that
    /// reaches the network or writes a whole working tree; every quick query
    /// (`rev-parse`, `status`, `config`, and the like) keeps the default — it
    /// is what protects the PreToolUse hook path from a wedged git, and
    /// widening it there defeats that.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Run it, returning stdout. A non-zero exit is an error carrying stderr.
    pub fn output(self) -> Result<String> {
        let command_line = self.command_line();
        let out = self.wait()?;
        if !out.status.success() {
            bail!(
                "`{command_line}` failed ({}):\n{}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Run it, reporting only whether it succeeded. For probes where a non-zero
    /// exit is an answer rather than a fault.
    pub fn success(self) -> Result<bool> {
        Ok(self.wait()?.status.success())
    }

    fn command_line(&self) -> String {
        format!("git {}", self.args.join(" "))
    }

    /// Spawn and wait, bounded by `self.timeout`. Polling `try_wait` rather
    /// than blocking because `wait_with_output` has no timeout and this runs
    /// on the write path.
    ///
    /// stdout and stderr are drained on their own threads rather than after
    /// the child exits: a child whose output fills the OS pipe buffer (git
    /// listing many refs, most obviously) blocks in `write()` until something
    /// reads, and nothing would read while this thread is only polling exit
    /// status — the wait would then hit the timeout on a child that was never
    /// actually wedged.
    fn wait(mut self) -> Result<Output> {
        let command_line = self.command_line();
        let arg_refs: Vec<&str> = self.args.iter().map(String::as_str).collect();
        let _span = crate::timing::subprocess_span("git", &arg_refs).entered();

        let mut child = self
            .command
            .spawn()
            .with_context(|| format!("failed to spawn `{command_line}`"))?;

        let mut stdout_pipe = child.stdout.take().expect("stdout is piped in `bare`");
        let mut stderr_pipe = child.stderr.take().expect("stderr is piped in `bare`");
        let stdout_reader = thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stdout_pipe.read_to_end(&mut buf);
            buf
        });
        let stderr_reader = thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr_pipe.read_to_end(&mut buf);
            buf
        });

        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e).with_context(|| format!("waiting for `{command_line}`"));
                }
            }
            if Instant::now() >= deadline {
                // Kill first so the reader threads see EOF and stop blocking
                // on a pipe that would otherwise never close; their output is
                // discarded below, not joined into an `Output`.
                let _ = child.kill();
                let _ = child.wait();
                bail!("`{command_line}` did not finish within {:?}", self.timeout);
            }
            thread::sleep(Duration::from_millis(1));
        };

        let stdout = stdout_reader
            .join()
            .map_err(|_| anyhow::anyhow!("git stdout reader thread panicked"))?;
        let stderr = stderr_reader
            .join()
            .map_err(|_| anyhow::anyhow!("git stderr reader thread panicked"))?;

        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }
}

/// The path that makes git skip a config level entirely.
#[cfg(any(test, feature = "test-support"))]
const NULL_DEVICE: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// `DETACHED` when the worktree has no branch checked out.
    pub branch: String,
    /// A bare repository has no working tree, so it holds no config.
    pub bare: bool,
    /// `git worktree lock`ed. Git refuses to remove one for a single `--force`
    /// — only a repeated `-f -f` overrides a lock — so a caller that removes
    /// worktrees must ask.
    pub locked: bool,
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
                branch: b.take().unwrap_or_else(|| "DETACHED".into()),
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
            // The reason is optional and free text, so only its presence is read.
            locked = true;
        }
    }
    flush(&mut path, &mut branch, &mut bare, &mut locked, &mut all);
    all
}

/// The checkout containing `start`. Errors when `start` is not in a
/// repository; a caller wanting a fallback declares one.
pub fn checkout_root(start: &Path) -> Result<PathBuf> {
    checkout_root_opt(start)?
        .with_context(|| format!("not inside a git repository: {}", start.display()))
}

/// The checkout containing `start`, distinguishing "there is no repository
/// here" from "git could not answer". `Ok(None)` means git ran and reported no
/// repository. `Err` means git itself could not be run — missing binary,
/// timeout, spawn failure — which is not the same answer and must not be
/// treated as one: a caller that folds both into "no repository" scopes
/// itself to the wrong root the moment git is merely unavailable rather than
/// genuinely outside a checkout.
pub fn checkout_root_opt(start: &Path) -> Result<Option<PathBuf>> {
    let out = Git::at(start)
        .args(["rev-parse", "--show-toplevel"])
        .wait()?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )))
}

/// Every worktree of `start`'s repository, main first.
pub fn worktrees(start: &Path) -> Result<Vec<Worktree>> {
    Ok(parse_porcelain(
        &Git::at(start)
            .args(["worktree", "list", "--porcelain"])
            .output()?,
    ))
}

/// `start`'s repository's main checkout, or `None` when `start` is already in
/// it and when the main worktree is bare. Git names the main worktree itself,
/// so no path is derived from the git directory's location: the parent of the
/// common directory cannot tell a real main worktree from a bare repository at
/// `/x/.git` or a `--separate-git-dir=/x/.git` clone.
pub fn main_checkout(start: &Path) -> Result<Option<PathBuf>> {
    let all = worktrees(start)?;
    let Some(main) = all.first() else {
        return Ok(None);
    };
    if main.bare {
        return Ok(None);
    }
    let here = checkout_root(start)?;
    Ok((!same_path(&main.path, &here)).then(|| main.path.clone()))
}

/// [`main_checkout`] for a caller that already knows `start`'s checkout root.
/// `rev-parse --show-toplevel` is a subprocess, so a caller needing both the
/// root and the main checkout resolves the root once and passes it here
/// instead of paying for a second one.
pub fn main_checkout_from(start: &Path, here: &Path) -> Result<Option<PathBuf>> {
    let all = worktrees(start)?;
    let Some(main) = all.first() else {
        return Ok(None);
    };
    if main.bare {
        return Ok(None);
    }
    Ok((!same_path(&main.path, here)).then(|| main.path.clone()))
}

/// The repository's primary checkout as seen from `start`: its main worktree
/// when `start` is a linked worktree, else `start`'s own checkout root. This
/// is the directory `worktree add`/`remove`, `fetch`, and the include backfill
/// act on, so every caller that needs "the checkout the worktrees hang off"
/// asks for it here rather than deriving it from a configured path or a
/// directory name.
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

/// The conventional worktree directory for a checkout: its own name plus
/// `_worktrees`, beside it. The underscore separates the suffix from a project
/// name, which commonly contains hyphens.
pub fn derived_worktree_root(primary: &Path) -> Option<PathBuf> {
    let name = primary.file_name()?.to_str()?;
    Some(primary.parent()?.join(format!("{name}_worktrees")))
}

/// The repository's main worktree, or `None` when it is bare. Distinct from
/// [`primary_checkout`], which falls back to the caller's own checkout: from a
/// linked worktree of a bare repository that fallback names the linked worktree
/// itself, so anything derived per-repository must not use it.
pub fn non_bare_main(start: &Path) -> Result<Option<PathBuf>> {
    Ok(worktrees(start)?
        .first()
        .filter(|w| !w.bare)
        .map(|w| w.path.clone()))
}

/// The branch checked out at `start`, or `DETACHED` when `start` has no
/// branch checked out.
pub fn branch(start: &Path) -> Result<String> {
    let branch = Git::at(start)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()?
        .trim()
        .to_string();
    Ok(if branch == "HEAD" {
        "DETACHED".to_string()
    } else {
        branch
    })
}

/// The remote's default branch, e.g. `origin/main`, from the `origin/HEAD`
/// symbolic ref. `git clone` sets it; `git init` plus a manually added remote
/// does not, which is why the caller has a fallback to offer.
pub fn default_remote_branch(repo: &Path) -> Result<String> {
    let out = Git::at(repo)
        .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .output()?;
    let s = out.trim();
    if s.is_empty() {
        anyhow::bail!("origin/HEAD names no branch");
    }
    Ok(s.to_string())
}

/// Compare two paths by identity where the filesystem can answer, falling back
/// to a lexical comparison when either does not exist.
pub fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn output_reports_stderr_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let err = Git::fixture(dir.path())
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("rev-parse --show-toplevel"), "{message}");
        assert!(message.contains("128"), "{message}");
    }

    /// `success` answers a question; a non-zero exit is one of the answers.
    #[test]
    fn success_reports_a_failure_as_false() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            !Git::fixture(dir.path())
                .args(["rev-parse", "--show-toplevel"])
                .success()
                .unwrap()
        );
    }

    /// Reading stdout only after the child exits deadlocks once output
    /// exceeds the OS pipe buffer: the child blocks in `write()` while this
    /// thread blocks in `wait()`, and neither yields. Long tag names clear
    /// the buffer with a couple hundred refs rather than thousands — a loose
    /// ref is one file, and thousands of them are slow enough on a Windows
    /// CI runner to make this a bogus timeout instead of an assertion.
    #[test]
    fn output_larger_than_a_pipe_buffer_comes_back_whole() {
        let dir = tempfile::tempdir().unwrap();
        Git::fixture(dir.path())
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        std::fs::write(dir.path().join("f"), "x").unwrap();
        Git::fixture(dir.path())
            .args(["add", "f"])
            .output()
            .unwrap();
        Git::fixture(dir.path())
            .args(["commit", "-q", "-m", "init"])
            .output()
            .unwrap();
        let commit = Git::fixture(dir.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let commit = commit.trim();

        // One `update-ref --stdin` batch beats spawning one `git tag` call
        // per ref; this is fixture setup, not the code under test, so it
        // goes through a raw `Command` rather than `Git`. Count times padding
        // has to push `git tag`'s output past a pipe buffer while each ref's
        // whole path stays inside Windows' 260-character ceiling: git writes a
        // loose ref as a file named after it and locks it through a sibling
        // `<name>.lock`, so the repository's own path counts against that
        // ceiling too, and a longer name buys nothing a larger count does not.
        const TAG_COUNT: usize = 900;
        const NAME_LEN: usize = 80;
        let mut batch = String::new();
        for i in 0..TAG_COUNT {
            let name = format!("{i:0>NAME_LEN$}");
            batch.push_str(&format!("create refs/tags/t{name} {commit}\n"));
        }
        let mut update_ref = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["update-ref", "--stdin"])
            .env("GIT_CONFIG_GLOBAL", NULL_DEVICE)
            .env("GIT_CONFIG_SYSTEM", NULL_DEVICE)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        update_ref
            .stdin
            .take()
            .unwrap()
            .write_all(batch.as_bytes())
            .unwrap();
        assert!(update_ref.wait().unwrap().success());

        let tags = Git::fixture(dir.path()).args(["tag"]).output().unwrap();
        assert_eq!(tags.lines().count(), TAG_COUNT);
        assert!(tags.len() > 64_000, "only {} bytes", tags.len());
    }

    /// `Git::at` has to hand git the exact bytes of `cwd`, not a lossy
    /// rendering of them: a path containing invalid UTF-8 is real on Linux,
    /// and `to_string_lossy` would rewrite it to a path that does not exist.
    ///
    /// This inspects the `Command` `Git` built rather than running git
    /// against a real directory of that name: APFS and HFS+ reject a
    /// non-UTF-8 filename outright (`EILSEQ`), so creating one would panic
    /// this test on macOS for a filesystem limitation that has nothing to do
    /// with the invariant under test — that the bytes reach the `Command`
    /// unchanged. Constructing the non-UTF-8 `OsStr` is still platform
    /// specific, hence the `cfg(unix)` on the test itself.
    #[cfg(unix)]
    #[test]
    fn at_preserves_a_non_utf8_path() {
        use std::os::unix::ffi::OsStrExt;

        let raw = b"br\xffken";
        let cwd = Path::new(OsStr::from_bytes(raw));

        let git = Git::at(cwd);
        let args: Vec<&OsStr> = git.command.get_args().collect();
        assert_eq!(args, [OsStr::new("-C"), OsStr::from_bytes(raw)]);
    }

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
        assert_eq!(branch(repo.path()).unwrap(), "main");
    }

    #[test]
    fn branch_is_detached_with_no_branch_checked_out() {
        let repo = repo_with_commit();
        run(&["checkout", "-q", "--detach"], repo.path()).unwrap();
        assert_eq!(branch(repo.path()).unwrap(), "DETACHED");
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
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                "--bare",
                seed.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        );

        // A linked worktree of a bare repository: `checkout_root` succeeds and
        // names this worktree, so deriving from it would give every worktree its
        // own root. `non_bare_main` is the value that must stay empty.
        let wt = tmp.path().join("wt");
        git(
            &bare,
            &["worktree", "add", "--detach", wt.to_str().unwrap()],
        );
        assert_eq!(non_bare_main(&wt).unwrap(), None);
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
        assert_eq!(default_remote_branch(p).unwrap(), "origin/main");
    }

    #[test]
    fn a_repo_without_origin_head_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        Git::fixture(p).args(["init", "-q"]).output().unwrap();
        assert!(default_remote_branch(p).is_err());
    }
}
