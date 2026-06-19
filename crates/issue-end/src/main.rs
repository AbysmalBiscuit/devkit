use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use devkit_common::cmd::{git, gh_json};
use devkit_common::linear::{self, LinearState};
use devkit_common::ui;
use devkit_common::worktree;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct Pr {
    pub number: u64,
    pub state: String, // MERGED | OPEN | CLOSED
    pub url: String,
    #[serde(rename = "headRefName")]
    pub head_ref_name: String,
}

fn state_rank(s: &str) -> u8 {
    match s { "MERGED" => 3, "OPEN" => 2, "CLOSED" => 1, _ => 0 }
}

/// Best PR for a head branch: prefer MERGED > OPEN > CLOSED, then higher number.
pub fn best_pr<'a>(prs: &'a [Pr], head: &str) -> Option<&'a Pr> {
    prs.iter()
        .filter(|p| p.head_ref_name == head)
        .max_by_key(|p| (state_rank(&p.state), p.number))
}

#[derive(Debug, Clone)]
struct Row {
    worktree: String,
    branch: String,
    issue_id: String,
    dirty: bool,
    pr_number: Option<u64>,
    pr_state: String, // MERGED|OPEN|CLOSED|NO_PR
    pr_url: Option<String>,
}

#[derive(Parser)]
#[command(about = "Triage and clean up finished issue worktrees")]
struct Cli {
    #[arg(short = 'C', long = "dir", global = true)]
    dir: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Read-only report of every issue worktree (optionally filtered by ID).
    Status { ids: Vec<String> },
    /// Remove FINISHED worktrees (PR merged + Linear done + clean).
    Clean {
        ids: Vec<String>,
        #[arg(short = 'y', long)]
        yes: bool,
        #[arg(long)]
        force: bool,
        #[arg(long = "pr-only")]
        pr_only: bool,
        #[arg(long = "clean-worktree")]
        clean_worktree: bool,
    },
}

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("issue-end");
    let cli = Cli::parse();
    let start = cli.dir.clone().unwrap_or_else(|| ".".to_string());
    match cli.cmd.unwrap_or(Cmd::Status { ids: vec![] }) {
        Cmd::Status { ids } => cmd_status(&start, &ids),
        Cmd::Clean { ids, yes, force, pr_only, clean_worktree } =>
            cmd_clean(&start, &ids, yes, force, pr_only, clean_worktree),
    }
}

fn build_rows(start: &str) -> Result<Vec<Row>> {
    let (main, others) = worktree::discover(start)?;
    let main_s = main.to_str().context("main repo path not UTF-8")?;
    let prs: Vec<Pr> = gh_json(
        &["pr", "list", "--state", "all", "--limit", "500", "--json", "number,state,url,headRefName"],
        main_s,
    )?;
    let mut rows = Vec::new();
    for wt in &others {
        let path = wt.path.to_string_lossy().into_owned();
        let dirty = !git(&["status", "--porcelain"], &path).unwrap_or_default().trim().is_empty();
        let iid = worktree::issue_id_of(&wt.branch, &wt.path);
        let pr = if wt.branch != "DETACHED" { best_pr(&prs, &wt.branch) } else { None };
        match pr {
            Some(p) => rows.push(Row {
                worktree: path, branch: wt.branch.clone(), issue_id: iid, dirty,
                pr_number: Some(p.number), pr_state: p.state.clone(), pr_url: Some(p.url.clone()),
            }),
            None => rows.push(Row {
                worktree: path, branch: wt.branch.clone(), issue_id: iid, dirty,
                pr_number: None, pr_state: "NO_PR".into(), pr_url: None,
            }),
        }
    }
    Ok(rows)
}

/// None when finished; otherwise a short reason it is not. With `pr_only`, the
/// Linear gate is dropped (finished = PR merged + clean).
fn reason_not_finished(row: &Row, linear: Option<&LinearState>, has_key: bool, pr_only: bool) -> Option<String> {
    if row.issue_id == "UNKNOWN" { return Some("not an issue worktree".into()); }
    let mut bits: Vec<String> = Vec::new();
    if row.pr_state != "MERGED" {
        bits.push(if row.pr_state != "NO_PR" { "PR not merged".into() } else { "no PR".into() });
    }
    if !pr_only {
        match linear {
            None => bits.push(if has_key { "Linear unknown".into() } else { "no Linear key".into() }),
            Some(st) if st.kind != "completed" => bits.push(format!("Linear {}", st.name)),
            _ => {}
        }
    }
    if row.dirty { bits.push("dirty".into()); }
    if bits.is_empty() { None } else { Some(bits.join(", ")) }
}

/// Rows, the Linear state per issue id, whether a Linear key is set, and the Linear
/// workspace slug (for issue links).
type Gathered = (Vec<Row>, HashMap<String, LinearState>, bool, Option<String>);

fn gather(start: &str, ids: &[String]) -> Result<Gathered> {
    let mut rows = build_rows(start)?;
    if !ids.is_empty() {
        let wanted: Vec<String> = ids.iter().map(|s| s.to_uppercase()).collect();
        rows.retain(|r| wanted.contains(&r.issue_id));
    }
    let key = std::env::var("LINEAR_API_KEY").ok();
    let issue_ids: Vec<String> = rows.iter().filter(|r| r.issue_id != "UNKNOWN").map(|r| r.issue_id.clone()).collect();
    let states = linear::states(&issue_ids, key.as_deref());
    let url_key = std::env::var("LINEAR_WORKSPACE").ok();
    Ok((rows, states, key.is_some(), url_key))
}

fn pr_label(row: &Row) -> String {
    if row.pr_state == "NO_PR" { "no PR".into() } else { format!("{} #{}", row.pr_state, row.pr_number.unwrap_or(0)) }
}

fn render(rows: &[Row], states: &HashMap<String, LinearState>, has_key: bool, url_key: Option<&str>) -> usize {
    println!("ISSUE WORKTREES");
    if rows.is_empty() { println!("  (none)"); return 0; }
    let mut sorted: Vec<&Row> = rows.iter().collect();
    sorted.sort_by(|a, b| a.issue_id.cmp(&b.issue_id));
    let mut t = ui::table(&["ISSUE", "BRANCH", "TREE", "PR", "LINEAR", "VERDICT"]);
    let mut finished = 0;
    for row in sorted {
        let linear = states.get(&row.issue_id);
        let verdict = match reason_not_finished(row, linear, has_key, false) {
            None => { finished += 1; "FINISHED".to_string() }
            Some(r) => r,
        };
        let issue_disp = match url_key {
            Some(k) if states.contains_key(&row.issue_id) =>
                ui::link(&row.issue_id, &format!("https://linear.app/{k}/issue/{}", row.issue_id)),
            _ => row.issue_id.clone(),
        };
        let pr_disp = match &row.pr_url {
            Some(u) => ui::link(&pr_label(row), u),
            None => pr_label(row),
        };
        let linear_disp = match linear {
            None => if has_key { "unknown".to_string() } else { "no key".to_string() },
            Some(st) => st.name.clone(),
        };
        t.add_row(vec![
            issue_disp, row.branch.clone(),
            if row.dirty { "dirty".to_string() } else { "clean".to_string() },
            pr_disp, linear_disp, verdict,
        ]);
    }
    println!("{t}");
    finished
}

fn cmd_status(start: &str, ids: &[String]) -> Result<()> {
    let (rows, states, has_key, url_key) = gather(start, ids)?;
    let finished = render(&rows, &states, has_key, url_key.as_deref());
    if finished > 0 {
        println!("\n{finished} finished. Run `issue-end clean` to remove them.");
    }
    if !has_key {
        println!("\nLINEAR_API_KEY unset — Linear gate skipped. Create a key at https://linear.app/settings/api");
    }
    Ok(())
}

fn select_explicit(rows: &[Row], selectors: &[String]) -> Vec<Row> {
    let mut chosen = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for sel in selectors {
        let s = sel.to_lowercase();
        let hits: Vec<&Row> = rows.iter().filter(|r| {
            let base = Path::new(&r.worktree).file_name().and_then(|x| x.to_str()).unwrap_or("").to_lowercase();
            [r.issue_id.to_lowercase(), r.branch.to_lowercase(), base, r.worktree.to_lowercase()].contains(&s)
        }).collect();
        if hits.is_empty() { eprintln!("no worktree matches '{sel}'"); }
        for r in hits {
            if seen.insert(r.worktree.clone()) { chosen.push(r.clone()); }
        }
    }
    chosen
}

fn confirm(label: &str) -> bool {
    print!("  Remove {label}? [y/N] ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() { return false; }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

enum CleanupError { Dirty, Other(anyhow::Error) }

/// Remove a finished worktree, delete its branch, and remove its ISSUE_*<id>*.md
/// files in the parent of the main repo. Refuses if cwd is inside the worktree, or
/// (without `force`) if the tree is dirty.
fn cleanup(worktree_path: &str, issue_id: &str, force: bool) -> std::result::Result<(), CleanupError> {
    let wt = std::fs::canonicalize(worktree_path).map_err(|e| CleanupError::Other(e.into()))?;
    let wt_s = wt.to_string_lossy().into_owned();
    let cwd = std::env::current_dir().map_err(|e| CleanupError::Other(e.into()))?;
    let cwd_c = std::fs::canonicalize(&cwd).unwrap_or(cwd);
    if cwd_c == wt || cwd_c.starts_with(&wt) {
        return Err(CleanupError::Other(anyhow::anyhow!("cd out of {wt_s} before removing it")));
    }
    let dirty = !git(&["status", "--porcelain"], &wt_s).map_err(CleanupError::Other)?.trim().is_empty();
    if dirty && !force { return Err(CleanupError::Dirty); }

    let common = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"], &wt_s)
        .map_err(CleanupError::Other)?.trim().to_string();
    let main = Path::new(&common).parent()
        .context("git-common-dir has no parent").map_err(CleanupError::Other)?;
    let parent = main.parent().context("main repo has no parent").map_err(CleanupError::Other)?;
    let main_s = main.to_str().context("main path not UTF-8").map_err(CleanupError::Other)?;
    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"], &wt_s)
        .map_err(CleanupError::Other)?.trim().to_string();

    let mut rm: Vec<&str> = vec!["worktree", "remove"];
    if force { rm.push("--force"); }
    rm.push(wt_s.as_str());
    git(&rm, main_s).map_err(CleanupError::Other)?;
    let _ = git(&["worktree", "prune"], main_s);

    if git(&["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")], main_s).is_ok() {
        let _ = git(&["branch", "-D", &branch], main_s);
    }

    if let Ok(read) = std::fs::read_dir(parent) {
        for ent in read.flatten() {
            let name = ent.file_name().to_string_lossy().into_owned();
            if name.starts_with("ISSUE_") && name.contains(issue_id) && name.ends_with(".md") {
                let _ = std::fs::remove_file(ent.path());
            }
        }
    }
    Ok(())
}

fn cmd_clean(start: &str, ids: &[String], yes: bool, force: bool, pr_only: bool, clean_worktree: bool) -> Result<()> {
    let targets: Vec<Row> = if clean_worktree {
        anyhow::ensure!(!ids.is_empty(), "--clean-worktree needs one or more selectors (issue id, branch, or worktree path)");
        let (rows, states, has_key, url_key) = gather(start, &[])?;
        render(&rows, &states, has_key, url_key.as_deref());
        let t = select_explicit(&rows, ids);
        if t.is_empty() { println!("\nNo matching worktrees."); return Ok(()); }
        println!("\n--clean-worktree: removing {} selected worktree(s), ignoring the PR/Linear/finished gate.", t.len());
        t
    } else {
        let (rows, states, has_key, url_key) = gather(start, ids)?;
        render(&rows, &states, has_key, url_key.as_deref());
        if pr_only { println!("--pr-only: Linear 'Done' gate skipped."); }
        let t: Vec<Row> = rows.iter()
            .filter(|r| reason_not_finished(r, states.get(&r.issue_id), has_key, pr_only).is_none())
            .cloned().collect();
        if t.is_empty() { println!("\nNothing finished to clean up."); return Ok(()); }
        println!("\n{} worktree(s) ready to remove:", t.len());
        t
    };

    let mut removed = 0;
    let total = targets.len();
    for row in &targets {
        let label = if row.issue_id != "UNKNOWN" { row.issue_id.clone() } else { row.branch.clone() };
        println!("\n{label}  {}", row.worktree);
        if !yes && !confirm(&label) { println!("    skipped"); continue; }
        match cleanup(&row.worktree, &row.issue_id, force) {
            Ok(()) => removed += 1,
            Err(CleanupError::Dirty) => eprintln!("    {label} is dirty — rerun with --force to discard."),
            Err(CleanupError::Other(e)) => eprintln!("    cleanup failed for {label}: {e}"),
        }
    }
    println!("\nRemoved {removed} of {total}.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pr(n: u64, state: &str, head: &str) -> Pr {
        Pr { number: n, state: state.into(), url: format!("https://x/{n}"), head_ref_name: head.into() }
    }
    #[test]
    fn best_pr_prefers_merged_over_open() {
        let prs = vec![pr(1, "OPEN", "feat"), pr(2, "MERGED", "feat"), pr(3, "CLOSED", "feat")];
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
        let row = Row { worktree: "/w".into(), branch: "b".into(), issue_id: "ENG-1".into(),
            dirty: false, pr_number: Some(1), pr_state: "MERGED".into(), pr_url: None };
        let st = LinearState { kind: "completed".into(), name: "Done".into() };
        assert!(reason_not_finished(&row, Some(&st), true, false).is_none());
    }
    #[test]
    fn not_finished_when_dirty() {
        let row = Row { worktree: "/w".into(), branch: "b".into(), issue_id: "ENG-1".into(),
            dirty: true, pr_number: Some(1), pr_state: "MERGED".into(), pr_url: None };
        let st = LinearState { kind: "completed".into(), name: "Done".into() };
        assert_eq!(reason_not_finished(&row, Some(&st), true, false).as_deref(), Some("dirty"));
    }
    #[test]
    fn pr_only_ignores_linear() {
        let row = Row { worktree: "/w".into(), branch: "b".into(), issue_id: "ENG-1".into(),
            dirty: false, pr_number: Some(1), pr_state: "MERGED".into(), pr_url: None };
        assert!(reason_not_finished(&row, None, false, true).is_none());
    }
}
