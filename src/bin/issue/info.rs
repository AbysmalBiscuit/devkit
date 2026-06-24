use crate::triage::render;
use anyhow::Result;
use devkit_common::cmd::git;
use devkit_issue::status::{self as st, IssueWorktree, StatusReport};
use std::path::Path;

/// Index of the worktree the command targets: the one matching `selector`, or —
/// when `selector` is `None` — the one whose path equals `current_top`.
fn pick_index(
    rows: &[IssueWorktree],
    selector: Option<&str>,
    current_top: Option<&str>,
) -> Option<usize> {
    match selector {
        Some(sel) => rows.iter().position(|r| crate::select::matches(r, sel)),
        None => {
            let top = current_top?;
            rows.iter().position(|r| same_path(&r.worktree, top))
        }
    }
}

/// Path equality that tolerates symlinks/normalization by canonicalizing both
/// sides; falls back to a string compare when a path cannot be canonicalized.
fn same_path(a: &str, b: &str) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

/// The current worktree's root (`git rev-parse --show-toplevel`), trimmed.
fn current_top(start: &str) -> Option<String> {
    git(&["rev-parse", "--show-toplevel"], start)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn run(start: &str, selector: Option<&str>, json: bool, cache_only: bool) -> Result<()> {
    let report = if cache_only {
        st::gather_local(start, &[])?
    } else {
        st::gather(start, &[])?
    };

    let top = current_top(start);
    let Some(i) = pick_index(&report.worktrees, selector, top.as_deref()) else {
        match selector {
            Some(sel) => anyhow::bail!("no worktree matches '{sel}'"),
            None => anyhow::bail!("not in an issue worktree"),
        }
    };

    let mut row = report.worktrees[i].clone();

    if cache_only {
        if let Some(pr) = crate::info_cache::read(Path::new(&row.worktree)) {
            row.pr_number = Some(pr.number);
            row.pr_state = pr.state;
            row.pr_url = Some(pr.url);
        }
    } else if let (Some(number), Some(url)) = (row.pr_number, row.pr_url.clone()) {
        // gather sets pr_number and pr_url together, so both-Some is the normal
        // PR case; a PR-less row simply leaves the cache untouched.
        let _ = crate::info_cache::write(
            Path::new(&row.worktree),
            &crate::info_cache::CachedPr {
                number,
                state: row.pr_state.clone(),
                url,
            },
        );
    }

    if json {
        println!("{}", serde_json::to_string(&row)?);
    } else {
        let one = StatusReport {
            finished_count: usize::from(row.finished),
            has_linear_key: report.has_linear_key,
            linear_workspace: report.linear_workspace.clone(),
            worktrees: vec![row],
        };
        render(&one);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(worktree: &str, branch: &str, id: &str) -> IssueWorktree {
        IssueWorktree {
            worktree: worktree.into(),
            branch: branch.into(),
            issue_id: id.into(),
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
    fn selector_picks_by_id() {
        let rows = vec![
            row("/a", "lev/eng-1-x", "ENG-1"),
            row("/b", "lev/eng-2-y", "ENG-2"),
        ];
        assert_eq!(pick_index(&rows, Some("eng-2"), None), Some(1));
    }

    #[test]
    fn no_selector_picks_current_top() {
        let rows = vec![
            row("/a", "lev/eng-1-x", "ENG-1"),
            row("/b", "lev/eng-2-y", "ENG-2"),
        ];
        assert_eq!(pick_index(&rows, None, Some("/b")), Some(1));
    }

    #[test]
    fn no_match_is_none() {
        let rows = vec![row("/a", "lev/eng-1-x", "ENG-1")];
        assert_eq!(pick_index(&rows, Some("eng-9"), None), None);
        assert_eq!(pick_index(&rows, None, Some("/elsewhere")), None);
        assert_eq!(pick_index(&rows, None, None), None);
    }
}
