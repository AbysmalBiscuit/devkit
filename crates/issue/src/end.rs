use anyhow::{Context, Result};
use devkit_common::cmd::git;
use std::io::{self, Write};
use std::path::Path;

use crate::triage::{gather, reason_not_finished, render, Row};

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

pub fn run(start: &str, ids: &[String], yes: bool, force: bool, pr_only: bool, clean_worktree: bool) -> Result<()> {
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
