use anyhow::{Context, Result};
use devkit_common::cmd::{gh_json, git};
use devkit_common::linear;
use devkit_common::worktree;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
struct Pr {
    number: u64,
    state: String, // MERGED | OPEN | CLOSED
    url: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
}

fn state_rank(s: &str) -> u8 {
    match s {
        "MERGED" => 3,
        "OPEN" => 2,
        "CLOSED" => 1,
        _ => 0,
    }
}

/// Best PR for a head branch: prefer MERGED > OPEN > CLOSED, then higher number.
fn best_pr<'a>(prs: &'a [Pr], head: &str) -> Option<&'a Pr> {
    prs.iter()
        .filter(|p| p.head_ref_name == head)
        .max_by_key(|p| (state_rank(&p.state), p.number))
}

/// One issue worktree with its PR + Linear state and the finished verdict.
#[derive(Debug, Clone, Serialize)]
pub struct IssueWorktree {
    pub worktree: String,
    pub branch: String,
    pub issue_id: String,
    pub dirty: bool,
    pub pr_number: Option<u64>,
    pub pr_state: String, // MERGED|OPEN|CLOSED|NO_PR
    pub pr_url: Option<String>,
    pub linear_kind: Option<String>,
    pub linear_name: Option<String>,
    pub finished: bool,
    pub reason_not_finished: Option<String>,
}

/// The full status snapshot for a set of worktrees.
#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    pub worktrees: Vec<IssueWorktree>,
    pub finished_count: usize,
    pub has_linear_key: bool,
    pub linear_workspace: Option<String>,
}

/// Discover worktrees and their best PR. Returns an empty vec (and skips the
/// `gh` round-trip) when the repo has no non-main worktrees.
fn build_rows(start: &str) -> Result<Vec<IssueWorktree>> {
    let (main, others) = worktree::discover(start)?;
    if others.is_empty() {
        return Ok(Vec::new());
    }
    let main_s = main.to_str().context("main repo path not UTF-8")?;
    let prs: Vec<Pr> = gh_json(
        &[
            "pr",
            "list",
            "--state",
            "all",
            "--limit",
            "500",
            "--json",
            "number,state,url,headRefName",
        ],
        main_s,
    )?;
    let mut rows = Vec::new();
    for wt in &others {
        let path = wt.path.to_string_lossy().into_owned();
        let dirty = !git(&["status", "--porcelain"], &path)
            .unwrap_or_default()
            .trim()
            .is_empty();
        let iid = worktree::issue_id_of(&wt.branch, &wt.path);
        let pr = if wt.branch != "DETACHED" {
            best_pr(&prs, &wt.branch)
        } else {
            None
        };
        let (pr_number, pr_state, pr_url) = match pr {
            Some(p) => (Some(p.number), p.state.clone(), Some(p.url.clone())),
            None => (None, "NO_PR".to_string(), None),
        };
        rows.push(IssueWorktree {
            worktree: path,
            branch: wt.branch.clone(),
            issue_id: iid,
            dirty,
            pr_number,
            pr_state,
            pr_url,
            linear_kind: None,
            linear_name: None,
            finished: false,
            reason_not_finished: None,
        });
    }
    Ok(rows)
}

/// None when finished; otherwise a short reason it is not. With `pr_only`, the
/// Linear gate is dropped (finished = PR merged + clean).
pub fn reason_not_finished(wt: &IssueWorktree, has_key: bool, pr_only: bool) -> Option<String> {
    if wt.issue_id == "UNKNOWN" {
        return Some("not an issue worktree".into());
    }
    let mut bits: Vec<String> = Vec::new();
    if wt.pr_state != "MERGED" {
        bits.push(if wt.pr_state != "NO_PR" {
            "PR not merged".into()
        } else {
            "no PR".into()
        });
    }
    if !pr_only {
        match wt.linear_kind.as_deref() {
            None => bits.push(if has_key {
                "Linear unknown".into()
            } else {
                "no Linear key".into()
            }),
            Some(kind) if kind != "completed" => bits.push(format!(
                "Linear {}",
                wt.linear_name.as_deref().unwrap_or("")
            )),
            _ => {}
        }
    }
    if wt.dirty {
        bits.push("dirty".into());
    }
    if bits.is_empty() {
        None
    } else {
        Some(bits.join(", "))
    }
}

/// Discover worktrees, attach Linear state, and compute the finished verdict.
///
/// The per-row `finished`/`reason_not_finished` is the full gate (`pr_only =
/// false`). Callers wanting the `--pr-only` gate must re-call
/// `reason_not_finished` with `pr_only = true` rather than trust `wt.finished`.
pub fn gather(start: &str, ids: &[String]) -> Result<StatusReport> {
    let mut rows = build_rows(start)?;
    let key = std::env::var("LINEAR_API_KEY").ok();
    let has_key = key.is_some();
    if rows.is_empty() {
        return Ok(StatusReport {
            worktrees: Vec::new(),
            finished_count: 0,
            has_linear_key: has_key,
            linear_workspace: None,
        });
    }
    if !ids.is_empty() {
        let wanted: Vec<String> = ids.iter().map(|s| s.to_uppercase()).collect();
        rows.retain(|r| wanted.contains(&r.issue_id));
    }
    let issue_ids: Vec<String> = rows
        .iter()
        .filter(|r| r.issue_id != "UNKNOWN")
        .map(|r| r.issue_id.clone())
        .collect();
    let states = linear::states(&issue_ids, key.as_deref());
    let linear_workspace = linear::workspace_url_key();
    let mut finished_count = 0;
    for wt in &mut rows {
        if let Some(st) = states.get(&wt.issue_id) {
            wt.linear_kind = Some(st.kind.clone());
            wt.linear_name = Some(st.name.clone());
        }
        let reason = reason_not_finished(wt, has_key, false);
        wt.finished = reason.is_none();
        if wt.finished {
            finished_count += 1;
        }
        wt.reason_not_finished = reason;
    }
    Ok(StatusReport {
        worktrees: rows,
        finished_count,
        has_linear_key: has_key,
        linear_workspace,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(n: u64, state: &str, head: &str) -> Pr {
        Pr {
            number: n,
            state: state.into(),
            url: format!("https://x/{n}"),
            head_ref_name: head.into(),
        }
    }

    fn wt(issue_id: &str, pr_state: &str, dirty: bool, linear_kind: Option<&str>) -> IssueWorktree {
        IssueWorktree {
            worktree: "/w".into(),
            branch: "b".into(),
            issue_id: issue_id.into(),
            dirty,
            pr_number: Some(1),
            pr_state: pr_state.into(),
            pr_url: None,
            linear_kind: linear_kind.map(String::from),
            linear_name: linear_kind.map(|_| "Done".to_string()),
            finished: false,
            reason_not_finished: None,
        }
    }

    #[test]
    fn best_pr_prefers_merged_over_open() {
        let prs = vec![
            pr(1, "OPEN", "feat"),
            pr(2, "MERGED", "feat"),
            pr(3, "CLOSED", "feat"),
        ];
        assert_eq!(best_pr(&prs, "feat").unwrap().number, 2);
    }

    #[test]
    fn best_pr_higher_number_within_same_state() {
        let prs = vec![pr(5, "OPEN", "feat"), pr(9, "OPEN", "feat")];
        assert_eq!(best_pr(&prs, "feat").unwrap().number, 9);
    }

    #[test]
    fn best_pr_none_for_unknown_head() {
        let prs = vec![pr(1, "MERGED", "feat")];
        assert!(best_pr(&prs, "other").is_none());
    }

    #[test]
    fn finished_when_merged_done_clean() {
        assert!(
            reason_not_finished(
                &wt("ENG-1", "MERGED", false, Some("completed")),
                true,
                false
            )
            .is_none()
        );
    }

    #[test]
    fn not_finished_when_dirty() {
        assert_eq!(
            reason_not_finished(&wt("ENG-1", "MERGED", true, Some("completed")), true, false)
                .as_deref(),
            Some("dirty")
        );
    }

    #[test]
    fn pr_only_ignores_linear() {
        // No Linear entry, no key, but pr_only drops the Linear gate.
        assert!(reason_not_finished(&wt("ENG-1", "MERGED", false, None), false, true).is_none());
    }

    #[test]
    fn verdict_combinations() {
        // Unknown id is never an issue worktree.
        assert_eq!(
            reason_not_finished(
                &wt("UNKNOWN", "MERGED", false, Some("completed")),
                true,
                false
            )
            .as_deref(),
            Some("not an issue worktree")
        );
        // No PR + no Linear key, all reasons join with ", ".
        assert_eq!(
            reason_not_finished(&wt("ENG-2", "NO_PR", false, None), false, false).as_deref(),
            Some("no PR, no Linear key")
        );
        // Open PR + started Linear + dirty.
        assert_eq!(
            reason_not_finished(&wt("ENG-3", "OPEN", true, Some("started")), true, false)
                .as_deref(),
            Some("PR not merged, Linear Done, dirty")
        );
        // Has key but no Linear entry → "Linear unknown".
        assert_eq!(
            reason_not_finished(&wt("ENG-4", "MERGED", false, None), true, false).as_deref(),
            Some("Linear unknown")
        );
    }
}
