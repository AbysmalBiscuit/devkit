use anyhow::{Context, Result, bail};
use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Run a command with extra environment variables, capture stdout; the error
/// includes stderr on non-zero exit. The variables reach the child only:
/// `std::env::set_var` mutates a process other threads are reading, which is
/// why edition 2024 made it `unsafe`.
pub fn capture_env(
    program: &str,
    args: &[&str],
    cwd: Option<&str>,
    env: &[(&str, &str)],
) -> Result<String> {
    let _span = crate::timing::subprocess_span(program, args).entered();
    let mut c = Command::new(program);
    c.args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let out = c
        .output()
        .with_context(|| format!("failed to spawn `{program}`"))?;
    if !out.status.success() {
        bail!(
            "`{program} {}` failed ({}):\n{}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run a command, capture stdout; error includes stderr on non-zero exit.
pub fn capture(program: &str, args: &[&str], cwd: Option<&str>) -> Result<String> {
    capture_env(program, args, cwd, &[])
}

/// Run a command with a deadline, returning stdout only on success and `None`
/// on any failure — a missing program, a non-zero exit, or a run that outlives
/// `timeout`. For a probe whose answer is optional but whose cost is not: the
/// caller has a working fallback, and a wedged child must not become a wedged
/// caller.
///
/// stdout and stderr drain on their own threads rather than after the child
/// exits, so a child that fills the OS pipe buffer cannot block in `write()`
/// while this thread only polls exit status.
pub fn capture_bounded(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let _span = crate::timing::subprocess_span(program, args).entered();
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    let mut stdout_pipe = child.stdout.take()?;
    let mut stderr_pipe = child.stderr.take()?;
    let reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let draining_stderr = thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut sink);
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
        if Instant::now() >= deadline {
            // Kill first so the reader threads see EOF instead of blocking on
            // a pipe that would never close.
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            let _ = draining_stderr.join();
            return None;
        }
        thread::sleep(Duration::from_millis(1));
    };

    let stdout = reader.join().ok()?;
    let _ = draining_stderr.join();
    status
        .success()
        .then(|| String::from_utf8_lossy(&stdout).into_owned())
}

/// `gh <args...>` parsed as JSON.
pub fn gh_json<T: serde::de::DeserializeOwned>(args: &[&str], cwd: &str) -> Result<T> {
    let out = capture("gh", args, Some(cwd))?;
    let trimmed = out.trim();
    let raw = if trimmed.is_empty() { "[]" } else { trimmed };
    serde_json::from_str(raw).with_context(|| "parsing gh JSON output")
}

/// `<args...> --repo github.com/<slug>`, the argument vector `gh_json_in` and
/// `gh_capture` both run. Split out so the repository/host scoping is
/// testable without spawning `gh`.
fn gh_args(args: &[&str], repo: &crate::github::Repo) -> Vec<String> {
    let mut v: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    v.push("--repo".to_string());
    v.push(repo.qualified());
    v
}

/// `gh <args...> --repo github.com/<slug>` as JSON. Every repository-scoped
/// `gh` invocation goes through here so no call can be left to pick its
/// repository from the ambient `GH_REPO`.
pub fn gh_json_in<T: serde::de::DeserializeOwned>(
    args: &[&str],
    repo: &crate::github::Repo,
    cwd: &str,
) -> Result<T> {
    let v = gh_args(args, repo);
    let refs: Vec<&str> = v.iter().map(String::as_str).collect();
    gh_json(&refs, cwd)
}

/// `gh <args...> --repo github.com/<slug>`, capturing stdout. The mutating
/// counterpart to `gh_json_in`, scoped the same way and for the same reason:
/// no repository-scoped `gh` call is left for `GH_REPO` to redirect.
pub fn gh_capture(args: &[&str], repo: &crate::github::Repo, cwd: &str) -> Result<String> {
    let v = gh_args(args, repo);
    let refs: Vec<&str> = v.iter().map(String::as_str).collect();
    capture("gh", &refs, Some(cwd))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_reports_stderr_on_failure() {
        let err = capture("sh", &["-c", "echo boom >&2; exit 3"], None).unwrap_err();
        assert!(err.to_string().contains("boom"));
    }
    #[test]
    fn capture_returns_stdout() {
        assert_eq!(capture("echo", &["hi"], None).unwrap().trim(), "hi");
    }

    #[test]
    fn capture_env_reaches_the_child_without_touching_this_process() {
        let out = if cfg!(windows) {
            capture_env(
                "cmd",
                &["/C", "echo %DEVKIT_TEST_ENV%"],
                None,
                &[("DEVKIT_TEST_ENV", "on")],
            )
        } else {
            capture_env(
                "sh",
                &["-c", "printf %s \"$DEVKIT_TEST_ENV\""],
                None,
                &[("DEVKIT_TEST_ENV", "on")],
            )
        }
        .unwrap();
        assert_eq!(out.trim(), "on");
        // Setting a variable for a child must not mutate this process: `set_var` is
        // `unsafe` in edition 2024 precisely because other threads read the
        // environment concurrently, and devkit runs progress threads.
        assert!(std::env::var("DEVKIT_TEST_ENV").is_err());
    }

    #[test]
    fn capture_bounded_returns_stdout() {
        let out = capture_bounded("echo", &["hi"], Duration::from_secs(5));
        assert_eq!(out.as_deref().map(str::trim), Some("hi"));
    }

    // `sleep` and `sh` are not on a bare Windows PATH.
    #[cfg(unix)]
    #[test]
    fn capture_bounded_gives_up_on_a_child_that_never_exits() {
        assert_eq!(
            capture_bounded("sleep", &["30"], Duration::from_millis(50)),
            None
        );
    }

    // `sleep` and `sh` are not on a bare Windows PATH.
    #[cfg(unix)]
    #[test]
    fn capture_bounded_reports_a_failed_exit_as_no_answer() {
        assert_eq!(
            capture_bounded("sh", &["-c", "echo out; exit 1"], Duration::from_secs(5)),
            None
        );
    }

    #[test]
    fn capture_bounded_reports_a_missing_program_as_no_answer() {
        assert_eq!(
            capture_bounded("devkit-no-such-program", &[], Duration::from_secs(5)),
            None
        );
    }

    #[test]
    fn gh_json_in_always_names_the_repository_and_host() {
        let repo = crate::github::Repo { slug: "o/r".into() };
        // Asserted on the argument vector, not on behavior: the point is that
        // neither GH_REPO nor GH_HOST can redirect the call, and behavior alone
        // cannot distinguish "no ambient variable set" from "flag present".
        assert_eq!(
            gh_args(&["pr", "list"], &repo),
            vec!["pr", "list", "--repo", "github.com/o/r"]
        );
    }
}
