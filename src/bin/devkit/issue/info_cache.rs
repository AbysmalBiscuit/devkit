use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The cached PR for a worktree: written after a live `issue info`, read by
/// `issue info --cache-only`. A PR number is immutable once assigned, so this
/// needs no TTL — a live run overwrites it and the cache self-heals.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedPr {
    pub number: u64,
    pub state: String,
    pub url: String,
    /// Defaulted because `read` treats a parse failure as a cache miss, so a
    /// required field would silently invalidate every cache written earlier.
    #[serde(default)]
    pub is_draft: bool,
}

/// `<worktree>/.devkit/pr.json`.
fn path(worktree: &Path) -> PathBuf {
    worktree.join(".devkit").join("pr.json")
}

/// Read the cached PR, or `None` if the file is absent or unparseable.
pub fn read(worktree: &Path) -> Option<CachedPr> {
    let body = std::fs::read_to_string(path(worktree)).ok()?;
    serde_json::from_str(&body).ok()
}

/// Write the PR cache atomically (temp file + rename) under `<worktree>/.devkit/`,
/// creating the directory. Best-effort: callers may ignore the error since a
/// cache miss is never fatal.
pub fn write(worktree: &Path, pr: &CachedPr) -> Result<()> {
    let p = path(worktree);
    let dir = p.parent().expect("pr.json path has a parent");
    std::fs::create_dir_all(dir)?;
    devkit_common::gitignore::write_self_ignore(dir);
    let tmp = dir.join(format!("pr.json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(pr)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_round_trips() {
        let wt = tempfile::tempdir().unwrap();
        let pr = CachedPr {
            number: 123,
            state: "OPEN".into(),
            url: "https://x/pr/123".into(),
            is_draft: false,
        };
        write(wt.path(), &pr).unwrap();
        assert_eq!(read(wt.path()), Some(pr));
    }

    #[test]
    fn read_missing_is_none() {
        let wt = tempfile::tempdir().unwrap();
        assert_eq!(read(wt.path()), None);
    }

    #[test]
    fn read_corrupt_is_none() {
        let wt = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(wt.path().join(".devkit")).unwrap();
        std::fs::write(wt.path().join(".devkit").join("pr.json"), b"not json").unwrap();
        assert_eq!(read(wt.path()), None);
    }

    #[test]
    fn write_leaves_no_temp_file() {
        let wt = tempfile::tempdir().unwrap();
        write(
            wt.path(),
            &CachedPr {
                number: 1,
                state: "MERGED".into(),
                url: "u".into(),
                is_draft: false,
            },
        )
        .unwrap();
        let leftover: Vec<_> = std::fs::read_dir(wt.path().join(".devkit"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftover.is_empty(), "temp file left behind: {leftover:?}");
    }

    #[test]
    fn a_cache_without_is_draft_reads_as_not_draft() {
        let wt = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(wt.path().join(".devkit")).unwrap();
        std::fs::write(
            wt.path().join(".devkit").join("pr.json"),
            br#"{"number":1,"state":"OPEN","url":"u"}"#,
        )
        .unwrap();
        let got = read(wt.path()).expect("a cache predating is_draft still reads");
        assert!(!got.is_draft);
    }
}
