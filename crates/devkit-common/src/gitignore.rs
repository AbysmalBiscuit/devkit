use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use devkit_config::expand_tilde;

/// Resolve git's global excludes file. A configured `core.excludesfile` wins
/// (tilde-expanded); otherwise `$XDG_CONFIG_HOME/git/ignore`, else
/// `<home>/.config/git/ignore` — the path git reads by default.
fn resolve_excludes_path(configured: Option<&str>, home: &str, xdg: Option<&str>) -> PathBuf {
    if let Some(c) = configured.map(str::trim).filter(|c| !c.is_empty()) {
        return expand_tilde(c);
    }
    let base = match xdg.map(str::trim).filter(|x| !x.is_empty()) {
        Some(x) => PathBuf::from(x),
        None => Path::new(home).join(".config"),
    };
    base.join("git").join("ignore")
}

/// Drop a self-ignoring `.gitignore` (`*`) into a `.devkit/` directory so the
/// whole directory — the cache files and this `.gitignore` itself — stays
/// untracked in any repo, with no dependence on the global excludes file. The
/// `*` pattern matches `.gitignore` too, so the file never shows up in
/// `git status`. Best-effort and idempotent: an existing file is left untouched
/// and any IO error is swallowed, since failing to write it must never break a
/// command that only meant to update a cache.
pub fn write_self_ignore(devkit_dir: &Path) {
    let f = devkit_dir.join(".gitignore");
    if !f.exists() {
        let _ = std::fs::write(f, "*\n");
    }
}

/// The lines devkit wants in git's global excludes file: its per-checkout
/// state directory, and the `*.local` files its config layers read, such as
/// `devkit.local.toml`.
pub const IGNORE_PATTERNS: [&str; 3] = [".devkit/", "*.local", "*.local.*"];

/// The patterns `contents` lacks, in `IGNORE_PATTERNS` order. A bare `.devkit`
/// line counts as `.devkit/`, since it ignores the same directory.
fn missing_patterns(contents: &str) -> Vec<&'static str> {
    let lines: Vec<&str> = contents.lines().map(str::trim).collect();
    IGNORE_PATTERNS
        .into_iter()
        .filter(|p| {
            !lines
                .iter()
                .any(|l| l == p || (*p == ".devkit/" && *l == ".devkit"))
        })
        .collect()
}

/// The global excludes file git reads, resolved the way git resolves it.
pub fn excludes_path() -> Result<PathBuf> {
    let configured = devkit_git::Git::bare()
        .args(["config", "--global", "core.excludesfile"])
        .output()
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let home = std::env::var("HOME").context("HOME not set")?;
    let xdg = std::env::var("XDG_CONFIG_HOME").ok();
    Ok(resolve_excludes_path(
        configured.as_deref(),
        &home,
        xdg.as_deref(),
    ))
}

/// The excludes file's text, empty when it does not exist. Any other read
/// error, such as a file that is not UTF-8, is returned, so a caller never
/// rewrites lines it could not read.
fn read_excludes(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(body) => Ok(body),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// The global excludes file and the `IGNORE_PATTERNS` it lacks. A file that
/// does not exist lacks every pattern.
pub fn missing_from_excludes() -> Result<(PathBuf, Vec<&'static str>)> {
    let path = excludes_path()?;
    let existing = read_excludes(&path)?;
    let missing = missing_patterns(&existing);
    Ok((path, missing))
}

/// Append each of `IGNORE_PATTERNS` the global excludes file lacks, and return
/// the file's path and the patterns added.
/// Idempotent and append-only: existing lines are kept as they were.
pub fn ensure_ignored() -> Result<(PathBuf, Vec<&'static str>)> {
    let path = excludes_path()?;
    let mut body = read_excludes(&path)?;
    let missing = missing_patterns(&body);
    if missing.is_empty() {
        return Ok((path, missing));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    for pattern in &missing {
        body.push_str(pattern);
        body.push('\n');
    }
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok((path, missing))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prefers_configured_path() {
        // A configured excludesfile wins over the xdg/home fallback and is
        // tilde-expanded the same way every other config path is.
        let p = resolve_excludes_path(Some("~/custom/ignore"), "/home/u", Some("/home/u/.xdg"));
        assert_eq!(p, expand_tilde("~/custom/ignore"));
    }

    #[test]
    fn resolve_uses_xdg_when_unset() {
        let p = resolve_excludes_path(None, "/home/u", Some("/home/u/.xdg"));
        assert_eq!(p, PathBuf::from("/home/u/.xdg/git/ignore"));
    }

    #[test]
    fn resolve_falls_back_to_home() {
        let p = resolve_excludes_path(None, "/home/u", None);
        assert_eq!(p, PathBuf::from("/home/u/.config/git/ignore"));
    }

    #[test]
    fn write_self_ignore_creates_then_preserves() {
        let dir = tempfile::tempdir().unwrap();
        write_self_ignore(dir.path());
        let f = dir.path().join(".gitignore");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "*\n");
        // Idempotent: an existing file is left untouched.
        std::fs::write(&f, "custom\n").unwrap();
        write_self_ignore(dir.path());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "custom\n");
    }

    #[test]
    fn missing_patterns_reports_only_absent_lines() {
        assert_eq!(missing_patterns(""), IGNORE_PATTERNS.to_vec());
        assert_eq!(
            missing_patterns("node_modules/\n.devkit\n  *.local  \n"),
            vec!["*.local.*"]
        );
        assert!(missing_patterns(".devkit/\n*.local\n*.local.*\n").is_empty());
    }
}
