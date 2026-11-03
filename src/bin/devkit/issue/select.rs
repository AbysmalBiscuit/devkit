use devkit_ticket::status::IssueWorktree;
pub use devkit_ticket::status::matches_parts;

/// True when `sel` names this worktree by any of `matches_parts`' names or by
/// the PR number (`3124` or `#3124`).
pub fn matches(row: &IssueWorktree, sel: &str) -> bool {
    if matches_parts(&row.worktree, &row.branch, &row.issue_id.to_string(), sel) {
        return true;
    }
    let s = sel.to_lowercase();
    row.pr
        .number()
        .is_some_and(|pr| s.strip_prefix('#').unwrap_or(&s) == pr.to_string())
}

#[cfg(test)]
mod tests {
    use devkit_ticket::status::PrStatus;

    use super::*;

    fn row() -> IssueWorktree {
        IssueWorktree {
            worktree: "/home/u/wt/eng-7-fix".into(),
            branch: "lev/eng-7-fix".into(),
            issue_id: "ENG-7".parse().unwrap(),
            record_unreadable: false,
            tree: devkit_ticket::status::Tree::Clean,
            pr: PrStatus::None,
            state: None,
            verdict: Default::default(),
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

    /// The rows carry the spelling git reports and the caller types their own.
    /// Two names for one directory must still select it.
    #[test]
    fn a_path_selector_matches_another_spelling_of_one_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("wt").join("eng-7-fix");
        std::fs::create_dir_all(&real).unwrap();
        let indirect = tmp.path().join("wt").join(".").join("..").join("wt");
        let r = IssueWorktree {
            worktree: real.to_string_lossy().into_owned(),
            ..row()
        };
        assert!(matches(&r, &indirect.join("eng-7-fix").to_string_lossy()));
        assert!(!matches(
            &r,
            &indirect.join("eng-8-other").to_string_lossy()
        ));
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
                is_draft: false,
                ahead: None,
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
