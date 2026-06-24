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
    let tmp = dir.join(format!("pr.json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(pr)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("devkit-info-cache-{}-{}", std::process::id(), tag))
    }

    #[test]
    fn write_then_read_round_trips() {
        let wt = scratch("rt");
        let _ = std::fs::remove_dir_all(&wt);
        std::fs::create_dir_all(&wt).unwrap();
        let pr = CachedPr {
            number: 123,
            state: "OPEN".into(),
            url: "https://x/pr/123".into(),
        };
        write(&wt, &pr).unwrap();
        assert_eq!(read(&wt), Some(pr));
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn read_missing_is_none() {
        let wt = scratch("missing");
        let _ = std::fs::remove_dir_all(&wt);
        assert_eq!(read(&wt), None);
    }

    #[test]
    fn read_corrupt_is_none() {
        let wt = scratch("corrupt");
        let _ = std::fs::remove_dir_all(&wt);
        std::fs::create_dir_all(wt.join(".devkit")).unwrap();
        std::fs::write(wt.join(".devkit").join("pr.json"), b"not json").unwrap();
        assert_eq!(read(&wt), None);
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn write_leaves_no_temp_file() {
        let wt = scratch("notmp");
        let _ = std::fs::remove_dir_all(&wt);
        std::fs::create_dir_all(&wt).unwrap();
        write(
            &wt,
            &CachedPr {
                number: 1,
                state: "MERGED".into(),
                url: "u".into(),
            },
        )
        .unwrap();
        let leftover: Vec<_> = std::fs::read_dir(wt.join(".devkit"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftover.is_empty(), "temp file left behind: {leftover:?}");
        let _ = std::fs::remove_dir_all(&wt);
    }
}
