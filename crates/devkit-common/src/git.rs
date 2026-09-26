//! The single door to git. Every git invocation in the workspace is built here
//! so that three properties hold everywhere rather than nowhere: the
//! environment cannot redirect the call to another repository or inject
//! config into it, a git that stops responding cannot block its caller
//! forever, and large output cannot deadlock the caller either.
//!
//! The inner `Command` is private on purpose. Handing one back would let a
//! caller finish it with `output()`, which has no timeout — and this runs on
//! the write path.

use std::{
    ffi::OsStr,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

/// Default backstop for a git that never returns. Long enough that no healthy
/// call reaches it, short enough that a wedged one fails instead of hanging a
/// write through the PreToolUse hook. Override with `.timeout()`.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Timeout for a git call that is slow by nature rather than by accident:
/// anything that reaches the network (`clone`, `fetch`, `push`), and anything
/// that writes a whole working tree (`worktree add`/`remove`, `checkout`).
/// The two overlap. Populating a worktree of a partial clone fetches every
/// object the new working tree needs at that commit, so it is a network call
/// as well as a bulk one, and belongs in this tier for both reasons.
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
    cwd: Option<PathBuf>,
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
        git.cwd = Some(cwd.to_path_buf());
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
            cwd: None,
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

    /// Override the default timeout. Pass `SLOW_TIMEOUT` for a call that writes
    /// a whole working tree; use [`Git::network`] for one that can reach a
    /// remote. Every quick query (`rev-parse`, `status`, `config`, and the
    /// like) keeps the default — it
    /// is what protects the PreToolUse hook path from a wedged git, and
    /// widening it there defeats that.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Mark a call that can reach a remote: `fetch`, `push`, `clone`, and a
    /// `worktree add` or `checkout` that fetches missing blobs. It gets
    /// `SLOW_TIMEOUT`, and neither ssh nor git prompts: a locked key or an
    /// unknown host fails with a remedy instead of prompting on the terminal,
    /// where a progress spinner hides the prompt.
    pub fn network(mut self) -> Self {
        self.timeout = SLOW_TIMEOUT;
        if let Some(ssh) = batch_ssh_command(self.cwd.as_deref()) {
            self.command.env("GIT_SSH_COMMAND", ssh);
        }
        self.command.env("GIT_TERMINAL_PROMPT", "0");
        self
    }

    /// Run it, returning stdout. A non-zero exit is an error carrying stderr.
    pub fn output(self) -> Result<String> {
        let command_line = self.command_line();
        let out = self.wait()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stderr = stderr.trim();
            let remedy = ssh_remedy(stderr).map_or(String::new(), |r| format!("\n\n{r}"));
            bail!(
                "`{command_line}` failed ({}):\n{stderr}{remedy}",
                out.status
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

/// The ssh command git would run from `cwd`, in git's own order of
/// precedence, with `-o BatchMode=yes` added. `None` when it is not OpenSSH
/// (plink takes other flags) or `GIT_SSH` names a bare program, and when
/// the lookup fails: the call then runs as configured.
fn batch_ssh_command(cwd: Option<&Path>) -> Option<String> {
    let command = match std::env::var("GIT_SSH_COMMAND") {
        Ok(command) => command,
        Err(_) => {
            let configured = cwd
                .map_or_else(Git::bare, Git::at)
                .args(["config", "--get", "core.sshCommand"])
                .wait()
                .ok()?;
            if configured.status.success() {
                String::from_utf8_lossy(&configured.stdout)
                    .trim()
                    .to_owned()
            } else if std::env::var_os("GIT_SSH").is_some() {
                return None;
            } else {
                "ssh".to_owned()
            }
        }
    };
    with_batch_mode(&command)
}

fn with_batch_mode(ssh_command: &str) -> Option<String> {
    let program = ssh_command
        .split_whitespace()
        .next()?
        .trim_matches(['\'', '"']);
    (Path::new(program).file_stem()? == "ssh").then(|| format!("{ssh_command} -o BatchMode=yes"))
}

/// What to do about an ssh failure that batch mode turns a prompt into, or
/// `None` when `stderr` shows neither.
fn ssh_remedy(stderr: &str) -> Option<&'static str> {
    if stderr.contains("Permission denied (publickey") {
        Some(
            "ssh could not authenticate, most likely because the key is locked and devkit runs \
             ssh in batch mode, which never prompts. Unlock it with `ssh-add` in a terminal \
             (an agent asks the user to), then rerun.",
        )
    } else if stderr.contains("Host key verification failed") {
        Some(
            "ssh does not know this host yet, and devkit runs ssh in batch mode, which never \
             prompts to confirm it. Connect to the remote once with `ssh` in a terminal \
             (an agent asks the user to), then rerun.",
        )
    } else {
        None
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
            // The reason is optional and free text, so only its presence is
            // read.
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

/// One directory's answer to both of the questions every hook-path helper asks
/// about a checkout — where is the working tree containing it, and where is
/// the repository's main worktree — resolved by a single
/// `git worktree list --porcelain` and shared by the callers that used to ask
/// git for themselves.
///
/// Resolution is lazy: a caller that turns out not to need a checkout (an
/// enforcement flag switched off by environment, say) spawns no git at all.
/// It is also per-value and never process-wide, because `devkitd` and the MCP
/// server outlive the worktrees they serve and a cached listing would answer
/// with a checkout that has since been removed.
///
/// Helpers on the hook paths (`devkit hook`, `devkit brief`) take the caller's
/// `Checkout` rather than calling [`checkout_root`] or [`main_checkout`], so
/// an invocation spawns git once.
///
/// The whole listing is kept rather than two paths, so that
/// [`Checkout::checkout_of`] can answer for a sibling worktree of the same
/// repository without a second git call.
#[derive(Debug, Clone)]
pub struct Checkout {
    start: PathBuf,
    resolved: OnceLock<Resolved>,
}

#[derive(Debug, Clone, Default)]
struct Resolved {
    /// Every worktree of `start`'s repository, main first. Empty when git
    /// reported no repository, and when git could not be run.
    worktrees: Vec<Worktree>,
    /// Index into `worktrees` of the one containing `start`.
    here: Option<usize>,
    /// Why git could not be run at all, which is a different answer from git
    /// running and reporting no repository — a caller that folds the two
    /// scopes itself to the wrong root whenever git is merely unavailable.
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
            let out = match Git::at(&self.start)
                .args(["worktree", "list", "--porcelain"])
                .wait()
            {
                Ok(out) => out,
                Err(e) => {
                    return Resolved {
                        error: Some(format!("{e:#}")),
                        ..Resolved::default()
                    };
                }
            };
            // A non-zero exit is git answering "no repository here", the same
            // reading `checkout_root_opt` gives `rev-parse --show-toplevel`.
            if !out.status.success() {
                return Resolved::default();
            }
            let worktrees = parse_porcelain(&String::from_utf8_lossy(&out.stdout));
            let here = longest_containing(&worktrees, &self.start);
            Resolved {
                worktrees,
                here,
                error: None,
            }
        })
    }

    /// The working tree containing the directory this was resolved from, or
    /// `None` outside one. The counterpart of [`checkout_root_opt`].
    pub fn root(&self) -> Option<&Path> {
        let r = self.resolved();
        r.here.map(|i| r.worktrees[i].path.as_path())
    }

    /// Why git could not be run, when that is why [`Checkout::root`] is
    /// `None`. `None` here means git ran: either it found a checkout or it
    /// reported none.
    pub fn error(&self) -> Option<&str> {
        self.resolved().error.as_deref()
    }

    /// The repository's main worktree, or `None` when it is bare and when git
    /// could not answer. The counterpart of [`non_bare_main`].
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

    /// The working tree containing `dir`, for a `dir` that need not be the
    /// one this was resolved from — the file a write claims, say, which can
    /// sit in any worktree of this repository or in none of them.
    ///
    /// `None` means this listing does not settle the question and the caller
    /// must ask git about `dir` itself. That covers `dir` outside every listed
    /// worktree, and `dir` inside a nested repository — a submodule, most of
    /// all — which git would discover before the enclosing worktree and which
    /// this listing does not describe. A nested repository is found by the
    /// `.git` entry at its root, which is git's own discovery rule once the
    /// environment that could override it has been scrubbed.
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
            .all(|d| !d.join(".git").exists())
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

/// Whether two paths name one directory, and whether that could be decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathIdentity {
    Same,
    Different,
    /// Neither answer is established: a resolution failed for a reason other
    /// than the path being absent, so the two may or may not be one directory.
    Unknown,
}

/// Compare two paths by identity, keeping "cannot tell" apart from "not the
/// same".
///
/// A path that is absent is decidably not the path that resolved, so only a
/// resolution that fails for another reason — a permission on some parent, an
/// I/O error — is `Unknown`. When neither path exists there is nothing to
/// resolve and a lexical comparison is the whole of the available answer.
///
/// [`same_path`] folds `Unknown` into `false`, which is the safe reading
/// wherever a mismatch costs a permission. It is the wrong reading wherever a
/// mismatch *grants* one — deciding that no live server holds a directory
/// about to be deleted, most of all — and those callers match on this instead.
pub fn path_identity(a: &Path, b: &Path) -> PathIdentity {
    let decide = |x: &Path, y: &Path| {
        if x == y {
            PathIdentity::Same
        } else {
            PathIdentity::Different
        }
    };
    let missing = |e: &std::io::Error| e.kind() == std::io::ErrorKind::NotFound;
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ra), Ok(rb)) => decide(&ra, &rb),
        (Err(e), Ok(_)) | (Ok(_), Err(e)) if missing(&e) => PathIdentity::Different,
        (Err(ea), Err(eb)) if missing(&ea) && missing(&eb) => decide(a, b),
        _ => PathIdentity::Unknown,
    }
}

/// Whether two paths name one directory, reading "cannot tell" as "no".
///
/// Correct wherever a mismatch costs a permission rather than granting one. A
/// caller for which an undecided answer is dangerous uses [`path_identity`].
pub fn same_path(a: &Path, b: &Path) -> bool {
    path_identity(a, b) == PathIdentity::Same
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_spellings_of_one_directory_are_the_same_path() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let indirect = tmp.path().join("real/./../real");
        assert_eq!(path_identity(&real, &indirect), PathIdentity::Same);
    }

    #[test]
    fn a_path_that_is_absent_is_decidably_not_one_that_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let gone = tmp.path().join("gone");
        assert_eq!(path_identity(&real, &gone), PathIdentity::Different);
        assert_eq!(path_identity(&gone, &real), PathIdentity::Different);
    }

    #[test]
    fn two_absent_paths_fall_back_to_a_lexical_answer() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("gone-a");
        let b = tmp.path().join("gone-b");
        assert_eq!(path_identity(&a, &a), PathIdentity::Same);
        assert_eq!(path_identity(&a, &b), PathIdentity::Different);
    }

    /// A resolution that fails for a reason other than absence establishes
    /// nothing. Folding it into "different" is what lets a deletion past the
    /// live-server refusal that reads this.
    #[cfg(unix)]
    #[test]
    fn a_path_that_cannot_be_resolved_is_unknown_rather_than_different() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let locked = tmp.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        let inside = locked.join("tree");
        std::fs::create_dir(&inside).unwrap();
        let other = tmp.path().join("other");
        std::fs::create_dir(&other).unwrap();

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let verdict = path_identity(&inside, &other);
        // Restored before the assert so a failure cannot leave the tempdir
        // undeletable.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(verdict, PathIdentity::Unknown);
        assert!(
            !same_path(&inside, &other),
            "same_path still reads Unknown as no"
        );
    }

    /// When both resolutions fail, only *absence* on both sides decides
    /// anything; a failure for any other reason leaves the pair undecided
    /// however the other side failed. Reading either of these as the lexical
    /// answer is what lets a deletion past the live-server refusal.
    #[cfg(unix)]
    #[test]
    fn a_double_failure_is_unknown_unless_both_paths_are_merely_absent() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let locked = tmp.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        let a = locked.join("a");
        let b = locked.join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let absent = tmp.path().join("gone");

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let verdicts = [
            path_identity(&a, &b),
            path_identity(&a, &absent),
            path_identity(&absent, &a),
        ];
        // Restored before the asserts so a failure cannot leave the tempdir
        // undeletable.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(
            verdicts,
            [PathIdentity::Unknown; 3],
            "unreadable/unreadable and unreadable/absent are both undecided"
        );
    }
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

    #[test]
    fn an_unknown_host_key_names_the_remedy() {
        let stderr = "Host key verification failed.\nfatal: Could not read from remote repository.";
        assert!(ssh_remedy(stderr).is_some_and(|r| r.contains("`ssh`")));
        assert_eq!(ssh_remedy("fatal: not a git repository"), None);
    }

    #[test]
    fn batch_mode_extends_openssh_and_leaves_other_clients_alone() {
        assert_eq!(
            with_batch_mode("ssh.exe -i key").as_deref(),
            Some("ssh.exe -i key -o BatchMode=yes")
        );
        assert_eq!(
            with_batch_mode("'/usr/bin/ssh'").as_deref(),
            Some("'/usr/bin/ssh' -o BatchMode=yes")
        );
        assert_eq!(with_batch_mode("plink -batch"), None);
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
