use anyhow::{Context, Result};
use devkit_common::cmd::{gh_json, git};
use devkit_common::linear::{self, LinearState};
use devkit_common::worktree;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

/// Local-only discovery: worktrees + dirty placeholders + issue ids + the main
/// repo path. The slow network fetches consume this. Fast — no `gh`/Linear.
pub struct Discovered {
    rows: Vec<IssueWorktree>,
    main_path: String,
    issue_ids: Vec<String>,
}

impl Discovered {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn worktree_paths(&self) -> Vec<String> {
        self.rows.iter().map(|r| r.worktree.clone()).collect()
    }
    pub fn issue_ids(&self) -> &[String] {
        &self.issue_ids
    }
}

/// An opaque GitHub PR list for a set of worktrees.
pub struct Prs(Vec<Pr>);

/// Discover worktrees and their issue ids, filtered to `ids` when non-empty.
/// Rows carry `dirty = false` placeholders; the dirty check is a separate step
/// so callers can drive it with a progress bar.
pub fn discover(start: &str, ids: &[String]) -> Result<Discovered> {
    let (main, others) = worktree::discover(start)?;
    let main_path = main
        .to_str()
        .context("main repo path not UTF-8")?
        .to_string();
    let wanted: Vec<String> = ids.iter().map(|s| s.to_uppercase()).collect();
    let mut rows = Vec::new();
    for wt in &others {
        let iid = worktree::issue_id_of(&wt.branch, &wt.path);
        if !wanted.is_empty() && !wanted.contains(&iid) {
            continue;
        }
        rows.push(IssueWorktree {
            worktree: wt.path.to_string_lossy().into_owned(),
            branch: wt.branch.clone(),
            issue_id: iid,
            dirty: false,
            pr_number: None,
            pr_state: "NO_PR".to_string(),
            pr_url: None,
            linear_kind: None,
            linear_name: None,
            finished: false,
            reason_not_finished: None,
        });
    }
    let issue_ids = rows
        .iter()
        .filter(|r| r.issue_id != "UNKNOWN")
        .map(|r| r.issue_id.clone())
        .collect();
    Ok(Discovered {
        rows,
        main_path,
        issue_ids,
    })
}

/// True when a worktree has uncommitted changes.
pub fn dirty_of(path: &str) -> bool {
    !git(&["status", "--porcelain"], path)
        .unwrap_or_default()
        .trim()
        .is_empty()
}

/// The single `gh pr list` round-trip for every worktree PR. Skips the call
/// entirely when there are no worktrees.
pub fn fetch_prs(d: &Discovered) -> Result<Prs> {
    if d.rows.is_empty() {
        return Ok(Prs(Vec::new()));
    }
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
        &d.main_path,
    )?;
    Ok(Prs(prs))
}

/// Attach dirty flags (in row order), best PR, Linear state, and the finished
/// verdict. `linear_workspace` is carried through to the report for link building.
pub fn assemble(
    d: Discovered,
    dirty: Vec<bool>,
    prs: Prs,
    linear: HashMap<String, LinearState>,
    linear_workspace: Option<String>,
    has_key: bool,
) -> StatusReport {
    let mut rows = d.rows;
    let mut finished_count = 0;
    for (i, wt) in rows.iter_mut().enumerate() {
        wt.dirty = dirty.get(i).copied().unwrap_or(false);
        let pr = if wt.branch != "DETACHED" {
            best_pr(&prs.0, &wt.branch)
        } else {
            None
        };
        if let Some(p) = pr {
            wt.pr_number = Some(p.number);
            wt.pr_state = p.state.clone();
            wt.pr_url = Some(p.url.clone());
        }
        if let Some(st) = linear.get(&wt.issue_id) {
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
    StatusReport {
        worktrees: rows,
        finished_count,
        has_linear_key: has_key,
        linear_workspace,
    }
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

/// Discover worktrees, fetch PRs + Linear state concurrently, and compute the
/// finished verdict. Silent — no progress output (the CLI re-orchestrates the
/// same pieces with bars). Signature unchanged for MCP/dashboard/tests.
pub fn gather(start: &str, ids: &[String]) -> Result<StatusReport> {
    let d = discover(start, ids)?;
    let key = std::env::var("LINEAR_API_KEY").ok();
    let has_key = key.is_some();
    if d.is_empty() {
        return Ok(assemble(
            d,
            Vec::new(),
            Prs(Vec::new()),
            HashMap::new(),
            None,
            has_key,
        ));
    }
    let paths = d.worktree_paths();
    let ids_v: Vec<String> = d.issue_ids().to_vec();
    let (dirty, prs, linear, ws) = std::thread::scope(|s| {
        let dt = s.spawn(|| paths.iter().map(|p| dirty_of(p)).collect::<Vec<bool>>());
        let pt = s.spawn(|| fetch_prs(&d));
        let lt = s.spawn(|| {
            (
                linear::states(&ids_v, key.as_deref()),
                linear::workspace_url_key(),
            )
        });
        let dirty = dt.join().expect("dirty thread panicked");
        let prs = pt.join().expect("prs thread panicked")?;
        let (linear, ws) = lt.join().expect("linear thread panicked");
        Ok::<_, anyhow::Error>((dirty, prs, linear, ws))
    })?;
    Ok(assemble(d, dirty, prs, linear, ws, has_key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // assemble zips dirty flags onto rows in order, attaches the best PR by
    // branch, applies Linear state, and computes the finished verdict — the same
    // result the old monolithic gather produced.
    #[test]
    fn assemble_attaches_pr_dirty_and_verdict() {
        let rows = vec![IssueWorktree {
            worktree: "/w1".into(),
            branch: "lev/eng-1-foo".into(),
            issue_id: "ENG-1".into(),
            dirty: false,
            pr_number: None,
            pr_state: "NO_PR".into(),
            pr_url: None,
            linear_kind: None,
            linear_name: None,
            finished: false,
            reason_not_finished: None,
        }];
        let d = Discovered::for_test(rows, "/main".into(), vec!["ENG-1".into()]);
        let prs = Prs::for_test(vec![pr(7, "MERGED", "lev/eng-1-foo")]);
        let mut linear = HashMap::new();
        linear.insert(
            "ENG-1".to_string(),
            LinearState {
                kind: "completed".into(),
                name: "Done".into(),
            },
        );
        let report = assemble(d, vec![false], prs, linear, Some("acme".into()), true);
        let row = &report.worktrees[0];
        assert_eq!(row.pr_number, Some(7));
        assert_eq!(row.pr_state, "MERGED");
        assert!(!row.dirty);
        assert!(row.finished);
        assert_eq!(report.finished_count, 1);
        assert_eq!(report.linear_workspace.as_deref(), Some("acme"));
    }

    #[test]
    fn assemble_marks_dirty_from_flags() {
        let rows = vec![IssueWorktree {
            worktree: "/w1".into(),
            branch: "lev/eng-2-bar".into(),
            issue_id: "ENG-2".into(),
            dirty: false,
            pr_number: None,
            pr_state: "NO_PR".into(),
            pr_url: None,
            linear_kind: None,
            linear_name: None,
            finished: false,
            reason_not_finished: None,
        }];
        let d = Discovered::for_test(rows, "/main".into(), vec!["ENG-2".into()]);
        let report = assemble(
            d,
            vec![true],
            Prs::for_test(vec![]),
            HashMap::new(),
            None,
            false,
        );
        assert!(report.worktrees[0].dirty);
        assert!(!report.worktrees[0].finished);
    }

    impl Discovered {
        fn for_test(rows: Vec<IssueWorktree>, main_path: String, issue_ids: Vec<String>) -> Self {
            Discovered {
                rows,
                main_path,
                issue_ids,
            }
        }
    }
    impl Prs {
        fn for_test(prs: Vec<Pr>) -> Self {
            Prs(prs)
        }
    }

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
