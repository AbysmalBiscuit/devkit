use anyhow::{Context, Result, bail};
use std::process::Command;

/// Run a command, capture stdout; error includes stderr on non-zero exit.
pub fn capture(program: &str, args: &[&str], cwd: Option<&str>) -> Result<String> {
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
}
