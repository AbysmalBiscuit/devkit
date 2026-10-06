//! The JSON index `repo-rules-agent` writes, the rule source over it, and the
//! cache directory where the extractor keeps a repository's index.
//!
//! The cache directory is the one `repo-rules-agent` writes, so an index built
//! by the extractor is found with no configuration. The hashed repository path
//! is the main worktree rather than the current checkout: every devkit branch
//! lives in its own worktree, and an index built once in the main checkout
//! would otherwise be invisible from every worktree where the work happens.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};

use crate::{
    edit::{self, Fields},
    model::RuleIndex,
    source::RuleSource,
};

/// The rule source over one JSON index file.
pub struct FileSource {
    path: PathBuf,
    /// Whether an edit may create the file. One in the extractor's cache
    /// directory may not: the extractor writes a SQLite store there, which
    /// then wins over a JSON index devkit created and hides what was added.
    creates: bool,
}

impl FileSource {
    /// The index at `path`, which an edit creates when it is missing.
    pub fn at(path: PathBuf) -> FileSource {
        FileSource {
            path,
            creates: true,
        }
    }

    /// The index at `path` in the extractor's cache directory, which only the
    /// extractor creates.
    pub fn cached(path: PathBuf) -> FileSource {
        FileSource {
            path,
            creates: false,
        }
    }

    fn update<T>(&self, f: impl FnOnce(&mut edit::IndexDocument) -> Result<T>) -> Result<T> {
        if !self.creates && !self.path.exists() {
            let dir = self.path.parent().unwrap_or(&self.path);
            bail!(
                "no rules index in {}; build it with `repo-rules index` first",
                dir.display()
            );
        }
        edit::update(&self.path, f)
    }
}

impl RuleSource for FileSource {
    fn read(&self) -> Result<Option<RuleIndex>> {
        read(&self.path)
    }

    fn add(&self, repo: &str, fields: Fields) -> Result<String> {
        self.update(|doc| doc.add(repo, fields))
    }

    fn edit(&self, id: &str, fields: Fields) -> Result<()> {
        self.update(|doc| doc.edit(id, fields))
    }

    fn remove(&self, id: &str) -> Result<()> {
        self.update(|doc| doc.remove(id))
    }

    fn location(&self) -> String {
        self.path.display().to_string()
    }

    fn kind(&self) -> &'static str {
        "json"
    }
}

/// The cache directory name for a repository, as `rules/paths.py` builds it.
pub fn cache_dir_name(repo: &Path) -> String {
    let resolved = resolved(repo);
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

/// `path` as Python's `Path.resolve()` spells it, which is how the extractor
/// names a repository.
///
/// `canonicalize` agrees on Unix and does not on Windows, where it returns a
/// `\\?\C:\...` verbatim path that `Path.resolve()` never produces, so the
/// prefix is stripped.
pub(crate) fn resolved(path: &Path) -> PathBuf {
    strip_verbatim(&path.canonicalize().unwrap_or_else(|_| path.to_path_buf()))
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

/// The directory where the extractor keeps this repository's index.
pub fn cache_dir(repo: &Path) -> PathBuf {
    cache_root().join(cache_dir_name(repo))
}

/// The index at `path` without its removed rules, or `None` when there is no
/// file. A path that exists and cannot be read or parsed is an error: that is
/// a breakage rather than an absence.
pub(crate) fn read(path: &Path) -> Result<Option<RuleIndex>> {
    let raw = match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(anyhow!(
                "rules index at {} could not be read: {e}",
                path.display()
            ));
        }
        Ok(raw) => raw,
    };
    let mut index = serde_json::from_str::<RuleIndex>(&raw)
        .map_err(|e| anyhow!("rules index at {} did not parse: {e}", path.display()))?;
    index.rules.retain(|rule| !rule.removed);
    Ok(Some(index))
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

    /// An index whose top-level shape drifted is an error, not a panic.
    #[test]
    fn a_drifted_index_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.json");
        std::fs::write(&path, r#"{"rules": "not an array"}"#).unwrap();
        assert!(read(&path).is_err());
    }

    #[test]
    fn a_missing_index_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(&dir.path().join("absent.json")).unwrap().is_none());
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
