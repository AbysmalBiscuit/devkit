//! The single door to git. Every git invocation in the workspace is built here
//! so that three properties hold everywhere rather than nowhere: the
//! environment cannot redirect the call to another repository or inject
//! config into it, a git that stops responding cannot block its caller
//! forever, and large output cannot deadlock the caller either.
//!
//! The inner `Command` is private on purpose. Handing one back would let a
//! caller finish it with `output()`, which has no timeout — and this runs on
//! the write path.
//!
//! The project's repository is reached through `devkit_common::vcs` instead.

mod backend;
mod commit;

use std::{
    ffi::OsStr,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
pub use backend::{GitBackend, parse_porcelain};
pub use commit::{HOOK_VERB as COMMIT_HOOK_VERB, run_hook as run_commit_hook};

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
        let _span = devkit_timing::subprocess_span("git", &arg_refs).entered();

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

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

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
}
