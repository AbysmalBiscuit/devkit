use devkit_issue::status::IssueWorktree;
use std::path::Path;

/// True when `sel` names a worktree by issue id, branch, worktree basename, or
/// full path, all compared case-insensitively. This is the half of the matcher
/// that needs nothing but local git facts, so callers that never fetch PR or
/// tracker state can select without paying for the network.
pub fn matches_parts(worktree_path: &str, branch: &str, issue_id: &str, sel: &str) -> bool {
    let s = sel.to_lowercase();
    let base = Path::new(worktree_path)
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_lowercase();
    [
        issue_id.to_lowercase(),
        branch.to_lowercase(),
        base,
        worktree_path.to_lowercase(),
    ]
    .contains(&s)
}

/// True when `sel` names this worktree by any of `matches_parts`' names or by
/// the PR number (`3124` or `#3124`).
pub fn matches(row: &IssueWorktree, sel: &str) -> bool {
    if matches_parts(&row.worktree, &row.branch, &row.issue_id, sel) {
        return true;
    }
    let s = sel.to_lowercase();
    row.pr
        .number()
        .is_some_and(|pr| s.strip_prefix('#').unwrap_or(&s) == pr.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use devkit_issue::status::PrStatus;

    fn row() -> IssueWorktree {
        IssueWorktree {
            worktree: "/home/u/wt/eng-7-fix".into(),
            branch: "lev/eng-7-fix".into(),
            issue_id: "ENG-7".into(),
            dirty: false,
            pr: PrStatus::None,
            state: None,
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

    fn with_pr_number(n: u64) -> IssueWorktree {
        IssueWorktree {
            pr: PrStatus::Unique {
                number: n,
                state: "OPEN".into(),
                url: "".into(),
            },
            ..row()
        }
    }

    #[test]
    fn matches_by_pr_number() {
        let r = with_pr_number(3124);
        assert!(matches(&r, "3124"));
        assert!(matches(&r, "#3124"));
    }

    #[test]
    fn rejects_wrong_pr_number_and_pr_when_absent() {
        let r = with_pr_number(3124);
        assert!(!matches(&r, "3125"));
        // A row with no PR never matches a bare number.
        assert!(!matches(&row(), "3124"));
    }
}
