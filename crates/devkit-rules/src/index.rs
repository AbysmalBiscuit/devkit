//! Where the index lives, and reading it.
//!
//! The path is the one `repo-rules-agent` writes, so an index built by the
//! extractor is found with no configuration. The hashed repository path is the
//! main worktree rather than the current checkout: every devkit branch lives in
//! its own worktree, and an index built once in the main checkout would
//! otherwise be invisible from every worktree where the work happens.

use std::path::{Path, PathBuf};

use crate::model::RuleIndex;

/// The cache directory name for a repository, as `rules/paths.py` builds it.
pub fn cache_dir_name(repo: &Path) -> String {
    // Python hashes `str(Path.resolve())`. `canonicalize` agrees on Unix and
    // does not on Windows, where it returns a `\\?\C:\...` verbatim path that
    // `Path.resolve()` never produces, so the digest would differ for every
    // repository. Strip the prefix before hashing.
    let resolved = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let resolved = strip_verbatim(&resolved);
    let digest = ring::digest::digest(&ring::digest::SHA256, resolved.to_string_lossy().as_bytes());
    let hex: String = digest.as_ref()[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let raw = resolved
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // `re.sub(r"[^a-zA-Z0-9._-]+", "-", name)`: one dash per run of disallowed
    // characters, never merged with a neighbouring literal dash. Collapsing
    // `a-!b` to `a-b` where Python gives `a--b` names a different directory,
    // and the index is then silently not found.
    let mut basename = String::with_capacity(raw.len());
    let mut in_run = false;
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
            basename.push(c);
            in_run = false;
        } else if !in_run {
            basename.push('-');
            in_run = true;
        }
    }
    let basename = basename.trim_matches('-').to_lowercase();
    let basename = if basename.is_empty() {
        "repo"
    } else {
        &basename
    };
    format!("{basename}-{hex}")
}

/// A Windows `\\?\C:\...` path as `C:\...`. A no-op everywhere else.
fn strip_verbatim(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path.to_path_buf(),
    }
}

/// The cache root platformdirs gives `repo-rules`.
///
/// On Windows, platformdirs lays out
/// `%LOCALAPPDATA%\<appauthor>\<appname>\Cache` with appauthor defaulting to
/// appname, hence the `repo-rules\repo-rules\Cache` nesting below. The Unix
/// arms are the documented XDG and Apple locations.
fn cache_root() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        devkit_common::paths::home().join("Library/Caches/repo-rules")
    }
    #[cfg(target_os = "windows")]
    {
        match std::env::var_os("LOCALAPPDATA") {
            Some(x) if !x.is_empty() => PathBuf::from(x).join("repo-rules\\repo-rules\\Cache"),
            _ => devkit_common::paths::home().join("AppData\\Local\\repo-rules\\repo-rules\\Cache"),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        match std::env::var_os("XDG_CACHE_HOME") {
            Some(x) if !x.is_empty() => PathBuf::from(x).join("repo-rules"),
            _ => devkit_common::paths::home().join(".cache/repo-rules"),
        }
    }
}

/// Where the extractor would have written this repository's index.
pub fn default_index_path(repo: &Path) -> PathBuf {
    cache_root().join(cache_dir_name(repo)).join("index.json")
}

/// The index at `path` without its removed rules, or `None`. A file that exists
/// and does not parse earns one stderr line: that is a breakage rather than an
/// absence, and every other failure is silence.
pub fn load(path: &Path) -> Option<RuleIndex> {
    let raw = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<RuleIndex>(&raw) {
        Ok(mut index) => {
            index.rules.retain(|rule| !rule.removed);
            Some(index)
        }
        Err(e) => {
            let _ = std::io::Write::write_fmt(
                &mut std::io::stderr(),
                format_args!(
                    "devkit: rules index at {} did not parse: {e}\n",
                    path.display()
                ),
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ported from `repo-rules-agent` `rules/paths.py`: basename with every run
    /// of disallowed characters collapsed to `-`, trimmed, lowercased, plus the
    /// first eight hex characters of the SHA-256 of the absolute path.
    #[test]
    fn the_cache_dir_name_matches_the_extractors() {
        let name = cache_dir_name(Path::new("/srv/checkouts/devkit"));
        let (basename, digest) = name.rsplit_once('-').unwrap();
        assert_eq!(basename, "devkit");
        assert_eq!(digest.len(), 8);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_basename_with_disallowed_characters_collapses() {
        assert!(cache_dir_name(Path::new("/tmp/My Repo!!")).starts_with("my-repo-"));
        assert!(cache_dir_name(Path::new("/tmp/--weird--")).starts_with("weird-"));
    }

    /// An index whose top-level shape drifted injects nothing and does not
    /// panic.
    #[test]
    fn a_drifted_index_loads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.json");
        std::fs::write(&path, r#"{"rules": "not an array"}"#).unwrap();
        assert!(load(&path).is_none());
    }

    #[test]
    fn a_missing_index_loads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(&dir.path().join("absent.json")).is_none());
    }

    /// A repository path that cannot be canonicalized still yields a name
    /// rather than panicking, so the lookup simply misses.
    #[test]
    fn a_vanished_repo_path_still_names_a_cache_dir() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("removed-worktree");
        let name = cache_dir_name(&gone);
        assert!(name.starts_with("removed-worktree-"), "got {name}");
    }
}
