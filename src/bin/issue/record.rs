use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Per-worktree record written by `issue setup` and read by `issue review`,
/// carrying the setup-time context that is otherwise unavailable at review.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueRecord {
    pub issue: String,
    pub slug: String,
    pub apps: Vec<String>,
}

/// `<worktree>/.devkit/issue.toml`.
fn path(worktree: &Path) -> std::path::PathBuf {
    worktree.join(".devkit").join("issue.toml")
}

/// Write the record under `<worktree>/.devkit/`, creating the directory.
pub fn write(worktree: &Path, rec: &IssueRecord) -> Result<()> {
    let p = path(worktree);
    std::fs::create_dir_all(p.parent().expect("path has a parent"))
        .with_context(|| format!("creating {}", p.parent().unwrap().display()))?;
    let body = toml::to_string(rec).context("serializing issue record")?;
    std::fs::write(&p, body).with_context(|| format!("writing {}", p.display()))
}

/// Read the record from `<worktree>/.devkit/issue.toml`, or `None` if absent or
/// unparseable.
#[allow(dead_code)]
pub fn read(worktree: &Path) -> Option<IssueRecord> {
    let body = std::fs::read_to_string(path(worktree)).ok()?;
    toml::from_str(&body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_round_trips() {
        let dir = std::env::temp_dir().join(format!("devkit-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rec = IssueRecord {
            issue: "ABC-123".into(),
            slug: "fix-login".into(),
            apps: vec!["web".into(), "api".into()],
        };
        write(&dir, &rec).unwrap();
        assert_eq!(read(&dir), Some(rec));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_missing_is_none() {
        let dir = std::env::temp_dir().join("devkit-rec-does-not-exist-xyz");
        assert_eq!(read(&dir), None);
    }
}
