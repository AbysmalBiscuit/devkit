use anyhow::{Context, Result};
use devkit_common::cmd::{gh_json, git};
use devkit_common::linear::{self, LinearState};
use devkit_common::ui;
use devkit_common::worktree;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Pr {
    pub(crate) number: u64,
    pub(crate) state: String, // MERGED | OPEN | CLOSED
    pub(crate) url: String,
    #[serde(rename = "headRefName")]
    pub(crate) head_ref_name: String,
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
pub(crate) fn best_pr<'a>(prs: &'a [Pr], head: &str) -> Option<&'a Pr> {
    prs.iter()
        .filter(|p| p.head_ref_name == head)
        .max_by_key(|p| (state_rank(&p.state), p.number))
}

#[derive(Debug, Clone)]
pub(crate) struct Row {
    pub(crate) worktree: String,
    pub(crate) branch: String,
    pub(crate) issue_id: String,
    pub(crate) dirty: bool,
    pub(crate) pr_number: Option<u64>,
    pub(crate) pr_state: String, // MERGED|OPEN|CLOSED|NO_PR
    pub(crate) pr_url: Option<String>,
}

pub(crate) fn build_rows(start: &str) -> Result<Vec<Row>> {
    let (main, others) = worktree::discover(start)?;
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
        match pr {
            Some(p) => rows.push(Row {
                worktree: path,
                branch: wt.branch.clone(),
                issue_id: iid,
                dirty,
                pr_number: Some(p.number),
                pr_state: p.state.clone(),
                pr_url: Some(p.url.clone()),
            }),
            None => rows.push(Row {
                worktree: path,
                branch: wt.branch.clone(),
                issue_id: iid,
                dirty,
                pr_number: None,
                pr_state: "NO_PR".into(),
                pr_url: None,
            }),
        }
    }
    Ok(rows)
}

/// None when finished; otherwise a short reason it is not. With `pr_only`, the
/// Linear gate is dropped (finished = PR merged + clean).
pub(crate) fn reason_not_finished(
    row: &Row,
    linear: Option<&LinearState>,
    has_key: bool,
    pr_only: bool,
) -> Option<String> {
    if row.issue_id == "UNKNOWN" {
        return Some("not an issue worktree".into());
    }
    let mut bits: Vec<String> = Vec::new();
    if row.pr_state != "MERGED" {
        bits.push(if row.pr_state != "NO_PR" {
            "PR not merged".into()
        } else {
            "no PR".into()
        });
    }
    if !pr_only {
        match linear {
            None => bits.push(if has_key {
                "Linear unknown".into()
            } else {
                "no Linear key".into()
            }),
            Some(st) if st.kind != "completed" => bits.push(format!("Linear {}", st.name)),
            _ => {}
        }
    }
    if row.dirty {
        bits.push("dirty".into());
    }
    if bits.is_empty() {
        None
    } else {
        Some(bits.join(", "))
    }
}

/// Rows, the Linear state per issue id, whether a Linear key is set, and the Linear
/// workspace slug (for issue links).
pub(crate) type Gathered = (Vec<Row>, HashMap<String, LinearState>, bool, Option<String>);

pub(crate) fn gather(start: &str, ids: &[String]) -> Result<Gathered> {
    let mut rows = build_rows(start)?;
    if !ids.is_empty() {
        let wanted: Vec<String> = ids.iter().map(|s| s.to_uppercase()).collect();
        rows.retain(|r| wanted.contains(&r.issue_id));
    }
    let key = std::env::var("LINEAR_API_KEY").ok();
    let issue_ids: Vec<String> = rows
        .iter()
        .filter(|r| r.issue_id != "UNKNOWN")
        .map(|r| r.issue_id.clone())
        .collect();
    let states = linear::states(&issue_ids, key.as_deref());
    let url_key = std::env::var("LINEAR_WORKSPACE").ok();
    Ok((rows, states, key.is_some(), url_key))
}

fn pr_label(row: &Row) -> String {
    if row.pr_state == "NO_PR" {
        "no PR".into()
    } else {
        format!("{} #{}", row.pr_state, row.pr_number.unwrap_or(0))
    }
}

pub(crate) fn render(
    rows: &[Row],
    states: &HashMap<String, LinearState>,
    has_key: bool,
    url_key: Option<&str>,
) -> usize {
    println!("ISSUE WORKTREES");
    if rows.is_empty() {
        println!("  (none)");
        return 0;
    }
    let mut sorted: Vec<&Row> = rows.iter().collect();
    sorted.sort_by(|a, b| a.issue_id.cmp(&b.issue_id));
    let mut t = ui::table(&["ISSUE", "BRANCH", "TREE", "PR", "LINEAR", "VERDICT"]);
    let mut finished = 0;
    for row in sorted {
        let linear = states.get(&row.issue_id);
        let verdict = match reason_not_finished(row, linear, has_key, false) {
            None => {
                finished += 1;
                "FINISHED".to_string()
            }
            Some(r) => r,
        };
        let issue_disp = match url_key {
            Some(k) if states.contains_key(&row.issue_id) => ui::link(
                &row.issue_id,
                &format!("https://linear.app/{k}/issue/{}", row.issue_id),
            ),
            _ => row.issue_id.clone(),
        };
        let pr_disp = match &row.pr_url {
            Some(u) => ui::link(&pr_label(row), u),
            None => pr_label(row),
        };
        let linear_disp = match linear {
            None => {
                if has_key {
                    "unknown".to_string()
                } else {
                    "no key".to_string()
                }
            }
            Some(st) => st.name.clone(),
        };
        t.add_row(vec![
            issue_disp,
            row.branch.clone(),
            if row.dirty {
                "dirty".to_string()
            } else {
                "clean".to_string()
            },
            pr_disp,
            linear_disp,
            verdict,
        ]);
    }
    println!("{t}");
    finished
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
        let row = Row {
            worktree: "/w".into(),
            branch: "b".into(),
            issue_id: "ENG-1".into(),
            dirty: false,
            pr_number: Some(1),
            pr_state: "MERGED".into(),
            pr_url: None,
        };
        let st = LinearState {
            kind: "completed".into(),
            name: "Done".into(),
        };
        assert!(reason_not_finished(&row, Some(&st), true, false).is_none());
    }
    #[test]
    fn not_finished_when_dirty() {
        let row = Row {
            worktree: "/w".into(),
            branch: "b".into(),
            issue_id: "ENG-1".into(),
            dirty: true,
            pr_number: Some(1),
            pr_state: "MERGED".into(),
            pr_url: None,
        };
        let st = LinearState {
            kind: "completed".into(),
            name: "Done".into(),
        };
        assert_eq!(
            reason_not_finished(&row, Some(&st), true, false).as_deref(),
            Some("dirty")
        );
    }
    #[test]
    fn pr_only_ignores_linear() {
        let row = Row {
            worktree: "/w".into(),
            branch: "b".into(),
            issue_id: "ENG-1".into(),
            dirty: false,
            pr_number: Some(1),
            pr_state: "MERGED".into(),
            pr_url: None,
        };
        assert!(reason_not_finished(&row, None, false, true).is_none());
    }
}
