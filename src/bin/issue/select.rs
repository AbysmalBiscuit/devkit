use devkit_issue::status::IssueWorktree;
use std::path::Path;

/// True when `sel` names this worktree by issue id, branch, worktree basename,
/// or full path (all compared case-insensitively).
pub fn matches(row: &IssueWorktree, sel: &str) -> bool {
    let s = sel.to_lowercase();
    let base = Path::new(&row.worktree)
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_lowercase();
    [
        row.issue_id.to_lowercase(),
        row.branch.to_lowercase(),
        base,
        row.worktree.to_lowercase(),
    ]
    .contains(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> IssueWorktree {
        IssueWorktree {
            worktree: "/home/u/wt/eng-7-fix".into(),
            branch: "lev/eng-7-fix".into(),
            issue_id: "ENG-7".into(),
            dirty: false,
            pr_number: None,
            pr_state: "NO_PR".into(),
            pr_url: None,
            linear_kind: None,
            linear_name: None,
            finished: false,
            reason_not_finished: None,
        }
    }

    #[test]
    fn matches_by_id_branch_basename_and_path() {
        let r = row();
        assert!(matches(&r, "eng-7"));
        assert!(matches(&r, "ENG-7"));
        assert!(matches(&r, "lev/eng-7-fix"));
        assert!(matches(&r, "eng-7-fix"));
        assert!(matches(&r, "/home/u/wt/eng-7-fix"));
    }

    #[test]
    fn rejects_non_match() {
        assert!(!matches(&row(), "eng-8"));
    }
}
