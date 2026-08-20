use devkit_common::ui;
use devkit_issue::status::{IssueWorktree, StatusReport};

fn pr_label(row: &IssueWorktree) -> String {
    if row.pr_state == "NO_PR" {
        "no PR".into()
    } else {
        format!("{} #{}", row.pr_state, row.pr_number.unwrap_or(0))
    }
}

/// Branch is secondary — the issue id identifies the worktree — so cap it with
/// an ellipsis, letting the PR/LINEAR/VERDICT columns survive a narrow terminal.
/// `issue setup` fits the branches it creates to this same width.
const BRANCH_MAX: usize = ui::BRANCH_DISPLAY_MAX;

/// Column headers shared by the final render and the live table.
pub(crate) const HEADERS: [&str; 6] = ["ISSUE", "BRANCH", "TREE", "PR", "LINEAR", "VERDICT"];

pub(crate) fn issue_cell(row: &IssueWorktree, workspace: Option<&str>) -> String {
    let linked = match workspace {
        Some(k) if row.linear_kind.is_some() => ui::link(
            &row.issue_id,
            &format!("https://linear.app/{k}/issue/{}", row.issue_id),
        ),
        _ => row.issue_id.clone(),
    };
    if row.issue_id == "UNKNOWN" {
        ui::dim(&linked)
    } else {
        ui::cyan(&linked)
    }
}

pub(crate) fn branch_cell(branch: &str) -> String {
    ui::dim(&ui::truncate(branch, BRANCH_MAX))
}

pub(crate) fn tree_cell(dirty: bool) -> String {
    if dirty {
        ui::red("dirty")
    } else {
        ui::dim("clean")
    }
}

pub(crate) fn pr_cell(row: &IssueWorktree) -> String {
    let label = pr_label(row);
    let colored = match row.pr_state.as_str() {
        "MERGED" => ui::green(&label),
        "OPEN" => ui::yellow(&label),
        "CLOSED" => ui::red(&label),
        _ => ui::dim(&label), // NO_PR
    };
    match &row.pr_url {
        Some(u) => ui::link(&colored, u),
        None => colored,
    }
}

pub(crate) fn linear_cell(row: &IssueWorktree, has_key: bool) -> String {
    match row.linear_kind.as_deref() {
        None => ui::dim(if has_key { "unknown" } else { "no key" }),
        Some(kind) => {
            let name = row.linear_name.as_deref().unwrap_or("");
            match kind {
                "completed" => ui::green(name),
                "started" => ui::yellow(name),
                "canceled" => ui::red(name),
                _ => ui::dim(name),
            }
        }
    }
}

pub(crate) fn verdict_cell(row: &IssueWorktree, offline: bool) -> String {
    if offline {
        ui::dim("—")
    } else if row.finished {
        ui::bold_green("FINISHED")
    } else {
        // The only "ball in your court" reason is a dirty tree; flag it
        // yellow, leave the rest (waiting on PR/Linear) dim.
        match row.reason_not_finished.as_deref() {
            Some(r) if r.contains("dirty") => ui::yellow(r),
            Some(r) => ui::dim(r),
            None => ui::dim(""),
        }
    }
}

/// Render the worktree triage table. When `offline`, the LINEAR and VERDICT
/// columns show `—`: both depend on a Linear fetch that the caller skipped, so
/// any computed value would be stale (e.g. `issue info --cache-only`).
pub(crate) fn render(report: &StatusReport, offline: bool) -> usize {
    println!("{}", ui::bold_cyan("ISSUE WORKTREES"));
    if report.worktrees.is_empty() {
        println!("  {}", ui::dim("(none)"));
        return 0;
    }
    let mut sorted: Vec<&IssueWorktree> = report.worktrees.iter().collect();
    sorted.sort_by(|a, b| a.issue_id.cmp(&b.issue_id));
    let mut t = ui::table(&HEADERS);
    for row in sorted {
        let linear_disp = if offline {
            ui::dim("—")
        } else {
            linear_cell(row, report.has_linear_key)
        };
        t.add_row(vec![
            issue_cell(row, report.linear_workspace.as_deref()),
            branch_cell(&row.branch),
            tree_cell(row.dirty),
            pr_cell(row),
            linear_disp,
            verdict_cell(row, offline),
        ]);
    }
    println!("{t}");
    report.finished_count
}

#[cfg(test)]
mod tests {
    use super::*;
    use devkit_issue::status::IssueWorktree;

    fn row(pr_state: &str) -> IssueWorktree {
        IssueWorktree {
            worktree: "/w".into(),
            branch: "lev/eng-1-x".into(),
            issue_id: "ENG-1".into(),
            dirty: false,
            pr_number: Some(7),
            pr_state: pr_state.into(),
            pr_url: None,
            linear_kind: None,
            linear_name: None,
            finished: false,
            reason_not_finished: None,
        }
    }

    // Off-TTY the colour helpers pass text through, so assertions are plain.
    #[test]
    fn pr_cell_labels() {
        assert_eq!(pr_cell(&row("MERGED")), "MERGED #7");
        let mut r = row("NO_PR");
        r.pr_number = None;
        assert_eq!(pr_cell(&r), "no PR");
    }

    #[test]
    fn tree_cell_states() {
        assert_eq!(tree_cell(false), "clean");
        assert_eq!(tree_cell(true), "dirty");
    }

    #[test]
    fn linear_cell_no_key_vs_unknown() {
        assert_eq!(linear_cell(&row("OPEN"), false), "no key");
        assert_eq!(linear_cell(&row("OPEN"), true), "unknown");
        let mut r = row("OPEN");
        r.linear_kind = Some("completed".into());
        r.linear_name = Some("Done".into());
        assert_eq!(linear_cell(&r, true), "Done");
    }

    #[test]
    fn verdict_cell_variants() {
        assert_eq!(verdict_cell(&row("OPEN"), true), "—");
        let mut r = row("MERGED");
        r.finished = true;
        assert_eq!(verdict_cell(&r, false), "FINISHED");
        let mut r = row("OPEN");
        r.reason_not_finished = Some("PR not merged, dirty".into());
        assert_eq!(verdict_cell(&r, false), "PR not merged, dirty");
    }

    #[test]
    fn issue_cell_unknown_is_plain() {
        let mut r = row("OPEN");
        r.issue_id = "UNKNOWN".into();
        // No workspace / no linear state → bare id (colour is a passthrough here).
        assert_eq!(issue_cell(&r, None), "UNKNOWN");
        assert_eq!(issue_cell(&row("OPEN"), Some("acme")), "ENG-1");
    }

    #[test]
    fn branch_cell_truncates() {
        let long = "x".repeat(60);
        assert_eq!(branch_cell(&long).chars().count(), BRANCH_MAX);
    }
}
