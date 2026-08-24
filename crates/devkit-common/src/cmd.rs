use anyhow::{Context, Result, bail};
use std::process::Command;

/// Run a command, capture stdout; error includes stderr on non-zero exit.
pub fn capture(program: &str, args: &[&str], cwd: Option<&str>) -> Result<String> {
    let _span = crate::timing::subprocess_span(program, args).entered();
    let mut c = Command::new(program);
    c.args(args);
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

/// `git -C <cwd> <args...>`.
pub fn git(args: &[&str], cwd: &str) -> Result<String> {
    let mut full = vec!["-C", cwd];
    full.extend_from_slice(args);
    capture("git", &full, None)
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
/// counterpart to `gh_json_in` — `pr create`, `pr edit`, `pr checkout`, and
/// the `pr view` existence probe — scoped the same way and for the same
/// reason: no repository-scoped `gh` call is left for `GH_REPO` to redirect.
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
    fn gh_json_in_always_names_the_repository_and_host() {
        let repo = crate::github::Repo {
            slug: "o/r".into(),
            origin: crate::github::Origin::Defaulted,
        };
        // Asserted on the argument vector, not on behavior: the point is that
        // neither GH_REPO nor GH_HOST can redirect the call, and behavior alone
        // cannot distinguish "no ambient variable set" from "flag present".
        assert_eq!(
            gh_args(&["pr", "list"], &repo),
            vec!["pr", "list", "--repo", "github.com/o/r"]
        );
    }

    #[test]
    fn gh_capture_shares_the_same_repository_scoped_vector() {
        let repo = crate::github::Repo {
            slug: "o/r".into(),
            origin: crate::github::Origin::Defaulted,
        };
        // `gh_capture` (the mutating path: `pr create`, `pr edit`, `pr
        // checkout`, `pr view`) is built on the same `gh_args` as the read
        // path, so it is asserted the same way: on the vector's repository,
        // not on an exact argv string or on behavior — an ambient GH_REPO
        // cannot change which repository this names, because the vector
        // always carries an explicit `--repo`.
        let args = gh_args(&["pr", "create", "--title", "x"], &repo);
        assert!(args.contains(&"--repo".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("github.com/o/r"));
    }
}
