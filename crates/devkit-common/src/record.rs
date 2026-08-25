use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Per-worktree record written by `issue setup`, carrying the setup-time
/// context that is otherwise unavailable later: the authoritative issue id, and
/// the slug, apps and summary path that `issue review` and `issue end` need.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueRecord {
    pub issue: String,
    pub slug: String,
    pub apps: Vec<String>,
    /// The summary file `issue setup --summary` wrote, so `issue end` removes
    /// the file that actually exists rather than re-deriving a path from a
    /// template that may have changed since. Absent on records written before
    /// the summary existed, and on setups that asked for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The pull request this worktree's work belongs to, written by
    /// `checkout-pr` and by `issue review request` whenever either resolves
    /// one. The locator identifies both repository and number, so a PR outside
    /// `pr_repo` is still findable. Absent on records written before it existed
    /// and on an `issue setup` worktree whose PR does not exist yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<crate::github::PrLocator>,
}

/// `<worktree>/.devkit/issue.toml`.
fn path(worktree: &Path) -> std::path::PathBuf {
    worktree.join(".devkit").join("issue.toml")
}

/// Write the record under `<worktree>/.devkit/`, creating the directory.
pub fn write(worktree: &Path, rec: &IssueRecord) -> Result<()> {
    let p = path(worktree);
    let dir = p.parent().expect("path has a parent");
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    crate::gitignore::write_self_ignore(dir);
    let body = toml::to_string(rec).context("serializing issue record")?;
    std::fs::write(&p, body).with_context(|| format!("writing {}", p.display()))
}

/// Read the record from `<worktree>/.devkit/issue.toml`, or `None` if absent or
/// unparseable.
pub fn read(worktree: &Path) -> Option<IssueRecord> {
    let body = std::fs::read_to_string(path(worktree)).ok()?;
    toml::from_str(&body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let rec = IssueRecord {
            issue: "ABC-123".into(),
            slug: "fix-login".into(),
            apps: vec!["web".into(), "api".into()],
            summary: Some("/w/ISSUE_SUMMARY_ABC-123.md".into()),
            pr: Some(crate::github::PrLocator {
                repo: Some("acme/web".into()),
                number: 42,
            }),
        };
        write(dir.path(), &rec).unwrap();
        assert_eq!(read(dir.path()), Some(rec));
    }

    #[test]
    fn a_record_written_before_summaries_existed_still_reads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".devkit")).unwrap();
        std::fs::write(
            dir.path().join(".devkit").join("issue.toml"),
            "issue = \"ABC-1\"\nslug = \"fix\"\napps = []\n",
        )
        .unwrap();
        let got = read(dir.path()).expect("legacy record parses");
        assert_eq!(got.issue, "ABC-1");
        assert!(got.summary.is_none());
        assert!(got.pr.is_none());
    }

    #[test]
    fn read_missing_is_none() {
        let dir = std::env::temp_dir().join("devkit-rec-does-not-exist-xyz");
        assert_eq!(read(&dir), None);
    }

    #[test]
    fn an_old_record_with_no_pr_field_still_deserializes() {
        let rec: IssueRecord = toml::from_str("issue = 'ENG-1'\nslug = 'x'\napps = []\n").unwrap();
        assert_eq!(rec.pr, None);
    }
}
