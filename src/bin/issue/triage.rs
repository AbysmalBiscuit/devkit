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
const BRANCH_MAX: usize = 46;

pub(crate) fn render(report: &StatusReport) -> usize {
    println!("{}", ui::bold_cyan("ISSUE WORKTREES"));
    if report.worktrees.is_empty() {
        println!("  {}", ui::dim("(none)"));
        return 0;
    }
    let mut sorted: Vec<&IssueWorktree> = report.worktrees.iter().collect();
    sorted.sort_by(|a, b| a.issue_id.cmp(&b.issue_id));
    let mut t = ui::table(&["ISSUE", "BRANCH", "TREE", "PR", "LINEAR", "VERDICT"]);
    for row in sorted {
        let verdict_disp = if row.finished {
            ui::bold_green("FINISHED")
        } else {
            // The only "ball in your court" reason is a dirty tree; flag it
            // yellow, leave the rest (waiting on PR/Linear) dim.
            match row.reason_not_finished.as_deref() {
                Some(r) if r.contains("dirty") => ui::yellow(r),
                Some(r) => ui::dim(r),
                None => ui::dim(""),
            }
        };
        let issue_disp = {
            let linked = match report.linear_workspace.as_deref() {
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
        };
        let pr_disp = {
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
        };
        let linear_disp = match row.linear_kind.as_deref() {
            None => ui::dim(if report.has_linear_key {
                "unknown"
            } else {
                "no key"
            }),
            Some(kind) => {
                let name = row.linear_name.as_deref().unwrap_or("");
                match kind {
                    "completed" => ui::green(name),
                    "started" => ui::yellow(name),
                    "canceled" => ui::red(name),
                    _ => ui::dim(name),
                }
            }
        };
        let tree_disp = if row.dirty {
            ui::red("dirty")
        } else {
            ui::dim("clean")
        };
        t.add_row(vec![
            issue_disp,
            ui::dim(&ui::truncate(&row.branch, BRANCH_MAX)),
            tree_disp,
            pr_disp,
            linear_disp,
            verdict_disp,
        ]);
    }
    println!("{t}");
    report.finished_count
}
