//! The single door to git. Every git invocation in the workspace is built here
//! so that two properties hold everywhere rather than nowhere: the environment
//! cannot redirect the call to another repository, and a git that stops
//! responding cannot block its caller forever.
//!
//! The inner `Command` is private on purpose. Handing one back would let a
//! caller finish it with `output()`, which has no timeout — and this runs on
//! the write path.

use anyhow::{Result, bail};
use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Backstop for a git that never returns. Long enough that no healthy call
/// reaches it, short enough that a wedged one fails instead of hanging a write
/// through the PreToolUse hook.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Variables that repoint git at a different repository. Left in place, any of
/// them silently changes which `devkit.toml` devkit reads — and that file
/// carries `[apps] launch` and `[tasks] run`, which devkit executes.
const REDIRECTING_VARS: [&str; 4] = [
    "GIT_DIR",
    "GIT_COMMON_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
];

/// A git invocation under construction. Configure freely; the only way to run
/// one is a terminal method here, which is what makes the timeout unskippable.
pub struct Git {
    command: Command,
}

impl Git {
    /// Run git against `cwd`. The working directory is taken before any
    /// argument because it decides which git to spawn, not merely where to run
    /// it — see the module docs on WSL.
    pub fn at(cwd: &Path) -> Self {
        let mut git = Self::bare();
        git.command.arg("-C").arg(cwd);
        git
    }

    /// Run git with no working directory — `clone`, which has no repository to
    /// run inside yet.
    pub fn bare() -> Self {
        let mut command = Command::new("git");
        for var in REDIRECTING_VARS {
            command.env_remove(var);
        }
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        Self { command }
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
        self.command.args(args);
        self
    }

    /// Set one of git's own behavior variables, e.g. `GIT_NO_LAZY_FETCH`.
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.command.env(key, value);
        self
    }

    /// Run it, returning stdout. A non-zero exit is an error carrying stderr.
    pub fn output(self) -> Result<String> {
        let out = self.wait()?;
        if !out.status.success() {
            bail!(
                "git failed ({}):\n{}",
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

    /// Spawn and wait, bounded. Polling rather than blocking because
    /// `wait_with_output` has no timeout and this runs on the write path. The
    /// 1ms step keeps a healthy call's overhead below the spawn it already
    /// pays. Output is drained after exit, which is safe for the volumes git
    /// produces here.
    fn wait(mut self) -> Result<Output> {
        let _span = crate::timing::subprocess_span("git", &[]).entered();
        let mut child = self
            .command
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to spawn `git`: {e}"))?;

        let deadline = Instant::now() + TIMEOUT;
        loop {
            if child.try_wait()?.is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                bail!("git did not finish within {TIMEOUT:?}");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(child.wait_with_output()?)
    }
}

/// The path that makes git skip a config level entirely.
#[cfg(any(test, feature = "test-support"))]
const NULL_DEVICE: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

#[cfg(test)]
mod tests {
    use super::*;

    /// An ambient `GIT_DIR` redirects git to another repository. Every call in
    /// the workspace goes through this builder, so stripping it here is what
    /// stops a stranger's config being read as this repository's.
    #[test]
    fn ambient_git_dir_cannot_redirect_a_call() {
        let repo = tempfile::tempdir().unwrap();
        Git::fixture(repo.path())
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();

        let decoy = tempfile::tempdir().unwrap();
        Git::fixture(decoy.path())
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();

        // SAFETY: single-threaded test; the var is removed before asserting.
        unsafe { std::env::set_var("GIT_DIR", decoy.path().join(".git")) };
        let out = Git::at(repo.path())
            .args(["rev-parse", "--show-toplevel"])
            .output();
        unsafe { std::env::remove_var("GIT_DIR") };

        let toplevel = std::fs::canonicalize(out.unwrap().trim()).unwrap();
        assert_eq!(toplevel, std::fs::canonicalize(repo.path()).unwrap());
    }

    #[test]
    fn output_reports_stderr_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let err = Git::at(dir.path())
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .unwrap_err();
        assert!(err.to_string().contains("not a git repository"), "{err}");
    }

    /// `success` answers a question; a non-zero exit is one of the answers.
    #[test]
    fn success_reports_a_failure_as_false() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            !Git::at(dir.path())
                .args(["rev-parse", "--show-toplevel"])
                .success()
                .unwrap()
        );
    }
}
