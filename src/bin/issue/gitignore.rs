use anyhow::{Context, Result};
use devkit_ports::config::expand_tilde;
use std::path::{Path, PathBuf};

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

/// True when `.devkit/` (or `.devkit`) is not already an ignore line.
fn needs_devkit(contents: &str) -> bool {
    !contents
        .lines()
        .map(str::trim)
        .any(|l| l == ".devkit/" || l == ".devkit")
}

/// Ensure `.devkit/` is in the global excludes file. Idempotent; append-only.
/// Returns an error on IO failure — the caller decides whether to ignore it.
pub fn ensure_devkit_ignored() -> Result<()> {
    let configured = devkit_common::cmd::capture("git", &["config", "--global", "core.excludesfile"], None)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let home = std::env::var("HOME").context("HOME not set")?;
    let xdg = std::env::var("XDG_CONFIG_HOME").ok();
    let path = resolve_excludes_path(configured.as_deref(), &home, xdg.as_deref());

    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if !needs_devkit(&existing) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut body = existing;
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(".devkit/\n");
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    println!("added .devkit/ to {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prefers_configured_path() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let p = resolve_excludes_path(Some("~/custom/ignore"), "/home/u", None);
        assert_eq!(p, PathBuf::from(format!("{home}/custom/ignore")));
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
    fn needs_devkit_detects_presence() {
        assert!(needs_devkit(""));
        assert!(needs_devkit("node_modules/\n.other\n"));
        assert!(!needs_devkit("node_modules/\n.devkit/\n"));
        assert!(!needs_devkit(".devkit\n"));
    }
}
