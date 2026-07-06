use devkit_issue::status::IssueWorktree;
use std::path::Path;

/// True when `sel` names this worktree by issue id, branch, worktree basename,
/// full path (all compared case-insensitively), or the PR number (`3124` or
/// `#3124`).
pub fn matches(row: &IssueWorktree, sel: &str) -> bool {
    let s = sel.to_lowercase();
    let base = Path::new(&row.worktree)
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_lowercase();
    if [
        row.issue_id.to_lowercase(),
        row.branch.to_lowercase(),
        base,
        row.worktree.to_lowercase(),
    ]
    .contains(&s)
    {
        return true;
    }
    row.pr_number
        .is_some_and(|pr| s.strip_prefix('#').unwrap_or(&s) == pr.to_string())
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

    #[test]
    fn matches_by_pr_number() {
        let r = IssueWorktree {
            pr_number: Some(3124),
            ..row()
        };
        assert!(matches(&r, "3124"));
        assert!(matches(&r, "#3124"));
    }

    #[test]
    fn rejects_wrong_pr_number_and_pr_when_absent() {
        let r = IssueWorktree {
            pr_number: Some(3124),
            ..row()
        };
        assert!(!matches(&r, "3125"));
        // A row with no PR never matches a bare number.
        assert!(!matches(&row(), "3124"));
    }
}
