use devkit_common::tracker::StateKind;
use devkit_common::ui;
use devkit_issue::status::{IssueWorktree, PrStatus, StatusReport};

fn pr_label(row: &IssueWorktree) -> String {
    match &row.pr {
        PrStatus::None => "no PR".into(),
        PrStatus::Unique {
            number,
            state,
            is_draft,
            ..
        } => {
            let word = if *is_draft { "DRAFT" } else { state };
            format!("{word} #{number}")
        }
        PrStatus::Ambiguous { candidates } => format!("ambiguous ({})", candidates.len()),
        PrStatus::Unknown { .. } => "unknown".into(),
    }
}

/// Branch is secondary — the issue id identifies the worktree — so cap it with
/// an ellipsis, letting the PR/STATE/VERDICT columns survive a narrow terminal.
/// `issue setup` fits the branches it creates to this same width.
const BRANCH_MAX: usize = ui::BRANCH_DISPLAY_MAX;

/// Column headers shared by the final render and the live table.
pub(crate) const HEADERS: [&str; 6] = ["ISSUE", "BRANCH", "TREE", "PR", "STATE", "VERDICT"];

/// The issue id, linked when the tracker offers a link base and actually knew
/// the issue — an id the tracker never answered for has nothing to link to.
pub(crate) fn issue_cell(row: &IssueWorktree, link_base: Option<&str>) -> String {
    let linked = match link_base {
        Some(base) if row.state.is_some() => {
            ui::link(&row.issue_id, &format!("{base}{}", row.issue_id))
        }
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
    let drafting = matches!(row.pr, PrStatus::Unique { is_draft: true, .. });
    let colored = if drafting {
        ui::dim(&label)
    } else {
        match row.pr.state_label() {
            "MERGED" => ui::green(&label),
            "OPEN" => ui::yellow(&label),
            "CLOSED" => ui::red(&label),
            _ => ui::dim(&label), // NO_PR | AMBIGUOUS | UNKNOWN
        }
    };
    match row.pr.url() {
        Some(u) => ui::link(&colored, u),
        None => colored,
    }
}

pub(crate) fn state_cell(row: &IssueWorktree, ready: bool) -> String {
    match row.state.as_ref() {
        None => ui::dim(if ready { "unknown" } else { "no tracker" }),
        Some(s) => match s.kind {
            StateKind::Completed => ui::green(&s.name),
            StateKind::Started => ui::yellow(&s.name),
            StateKind::Canceled => ui::red(&s.name),
            StateKind::Triage | StateKind::Backlog | StateKind::Unstarted => ui::dim(&s.name),
        },
    }
}

pub(crate) fn verdict_cell(row: &IssueWorktree, offline: bool) -> String {
    if offline {
        ui::dim("—")
    } else if row.finished {
        ui::bold_green("FINISHED")
    } else {
        // The only "ball in your court" reason is a dirty tree; flag it
        // yellow, leave the rest (waiting on PR/tracker) dim.
        match row.reason_not_finished.as_deref() {
            Some(r) if r.contains("dirty") => ui::yellow(r),
            Some(r) => ui::dim(r),
            None => ui::dim(""),
        }
    }
}

/// Render the worktree triage table. When `offline`, the STATE and VERDICT
/// columns show `—`: both depend on a tracker fetch that the caller skipped, so
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
        let state_disp = if offline {
            ui::dim("—")
        } else {
            state_cell(row, report.tracker.ready)
        };
        t.add_row(vec![
            issue_cell(row, report.tracker.link_base.as_deref()),
            branch_cell(&row.branch),
            tree_cell(row.dirty),
            pr_cell(row),
            state_disp,
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

    fn row_with(pr: PrStatus) -> IssueWorktree {
        IssueWorktree {
            worktree: "/w".into(),
            branch: "lev/eng-1-x".into(),
            issue_id: "ENG-1".into(),
            dirty: false,
            pr,
            state: None,
            finished: false,
            reason_not_finished: None,
        }
    }

    fn row(pr_state: &str) -> IssueWorktree {
        let pr = if pr_state == "NO_PR" {
            PrStatus::None
        } else {
            PrStatus::Unique {
                number: 7,
                state: pr_state.into(),
                url: "".into(),
                is_draft: false,
            }
        };
        row_with(pr)
    }

    fn pr_ref(n: u64) -> devkit_common::tracker::PrRef {
        devkit_common::tracker::PrRef {
            url: format!("https://github.com/o/r/pull/{n}"),
            number: n,
        }
    }

    // Expectations are built from the same `ui::` helpers the cells use, so
    // they pin the colour each state is painted rather than merely the text,
    // and hold whether or not the ambient environment enables ANSI output.
    #[test]
    fn pr_cell_colors_by_state() {
        // Every `Unique` PR carries a url, so its cell is linked; `NO_PR` isn't.
        for (state, paint) in [
            ("MERGED", ui::green as fn(&str) -> String),
            ("OPEN", ui::yellow),
            ("CLOSED", ui::red),
            ("UNKNOWN", ui::dim),
        ] {
            let want = ui::link(&paint(&format!("{state} #7")), "");
            assert_eq!(pr_cell(&row(state)), want, "{state}");
        }
        assert_eq!(pr_cell(&row("NO_PR")), ui::dim("no PR"));
    }

    #[test]
    fn an_ambiguous_row_never_renders_a_pr_number() {
        // `format!("{} #{}", pr_state, pr_number.unwrap_or(0))` printed
        // `AMBIGUOUS #0` — a PR that does not exist, in the column read before
        // deleting a worktree.
        let row = row_with(PrStatus::Ambiguous {
            candidates: vec![pr_ref(7), pr_ref(8)],
        });
        let label = pr_label(&row);
        assert!(!label.contains('#'), "{label}");
        assert!(label.contains('2'), "{label} should say how many");

        assert_eq!(pr_label(&row_with(PrStatus::None)), "no PR");
        assert_eq!(
            pr_label(&row_with(PrStatus::Unique {
                number: 12,
                state: "MERGED".into(),
                url: "u".into(),
                is_draft: false,
            })),
            "MERGED #12"
        );
    }

    #[test]
    fn tree_cell_states() {
        assert_eq!(tree_cell(false), ui::dim("clean"));
        assert_eq!(tree_cell(true), ui::red("dirty"));
    }

    #[test]
    fn state_cell_no_tracker_vs_unknown() {
        assert_eq!(state_cell(&row("OPEN"), false), ui::dim("no tracker"));
        assert_eq!(state_cell(&row("OPEN"), true), ui::dim("unknown"));
        let mut r = row("OPEN");
        r.state = Some(devkit_common::tracker::State {
            kind: StateKind::Completed,
            name: "Done".into(),
            color: None,
        });
        assert_eq!(state_cell(&r, true), ui::green("Done"));
    }

    #[test]
    fn verdict_cell_variants() {
        assert_eq!(verdict_cell(&row("OPEN"), true), ui::dim("—"));
        let mut r = row("MERGED");
        r.finished = true;
        assert_eq!(verdict_cell(&r, false), ui::bold_green("FINISHED"));
        let mut r = row("OPEN");
        r.reason_not_finished = Some("PR not merged, dirty".into());
        assert_eq!(
            verdict_cell(&r, false),
            ui::yellow("PR not merged, dirty"),
            "a dirty tree is the one reason that needs you, so it reads yellow"
        );
    }

    #[test]
    fn issue_cell_dims_an_unknown_id() {
        let mut r = row("OPEN");
        r.issue_id = "UNKNOWN".into();
        assert_eq!(issue_cell(&r, None), ui::dim("UNKNOWN"));
        // The tracker never answered for this row, so the link base goes unused.
        assert_eq!(
            issue_cell(&row("OPEN"), Some("https://linear.app/acme/issue/")),
            ui::cyan("ENG-1")
        );
    }

    #[test]
    fn branch_cell_truncates() {
        let long = "x".repeat(60);
        let fitted = ui::truncate(&long, BRANCH_MAX);
        assert_eq!(fitted.chars().count(), BRANCH_MAX);
        assert_eq!(branch_cell(&long), ui::dim(&fitted));
    }

    #[test]
    fn a_draft_labels_as_draft() {
        let row = row_with(PrStatus::Unique {
            number: 7,
            state: "OPEN".into(),
            url: "u7".into(),
            is_draft: true,
        });
        assert_eq!(pr_label(&row), "DRAFT #7");
    }

    #[test]
    fn a_ready_pr_keeps_its_state_label() {
        let row = row_with(PrStatus::Unique {
            number: 7,
            state: "OPEN".into(),
            url: "u7".into(),
            is_draft: false,
        });
        assert_eq!(pr_label(&row), "OPEN #7");
    }
}
