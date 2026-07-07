use anyhow::{Context, Result};
use devkit_common::cmd::git;
use devkit_common::progress::Steps;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::triage::render;
use devkit_issue::status::{IssueWorktree, gather, reason_not_finished};

fn select_explicit(rows: &[IssueWorktree], selectors: &[String]) -> Vec<IssueWorktree> {
    let mut chosen = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for sel in selectors {
        let hits: Vec<&IssueWorktree> = rows
            .iter()
            .filter(|r| crate::select::matches(r, sel))
            .collect();
        if hits.is_empty() {
            eprintln!("no worktree matches '{sel}'");
        }
        for r in hits {
            if seen.insert(r.worktree.clone()) {
                chosen.push(r.clone());
            }
        }
    }
    chosen
}

fn confirm(label: &str) -> bool {
    print!("  Remove {label}? [y/N] ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Sentinel error for a worktree refused because it has uncommitted changes;
/// the caller downcasts to it to suggest `--force` instead of a generic failure.
#[derive(Debug)]
struct Dirty;

impl std::fmt::Display for Dirty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "dirty")
    }
}

impl std::error::Error for Dirty {}

/// The main repo's root path, derived once so a single post-removal
/// `git worktree prune` can run after all removals join. `start` is the base
/// dir (`.` by default); from inside a worktree or the primary clone,
/// `--git-common-dir` resolves to the main repo's `.git`, whose parent is the
/// root.
fn main_repo(start: &str) -> Result<String> {
    let common = git(
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        start,
    )?
    .trim()
    .to_string();
    let main = Path::new(&common)
        .parent()
        .context("git-common-dir has no parent")?;
    main.to_str()
        .map(str::to_string)
        .context("main path not UTF-8")
}

/// Remove a finished worktree, delete its branch, and remove its ISSUE_*<id>*.md
/// files in the parent of the main repo. Refuses if cwd is inside the worktree,
/// or (without `force`) if the tree is dirty. Serializes `git branch -D` behind
/// `branch_lock` so concurrent removals never contend on `packed-refs.lock`; the
/// worktree removal and file unlinks touch per-worktree state and run in
/// parallel. Pruning the stale worktree entry is left to a single caller-side
/// `git worktree prune` after all removals finish.
fn cleanup(
    worktree_path: &str,
    issue_id: &str,
    force: bool,
    branch_lock: &Mutex<()>,
) -> Result<()> {
    let wt = std::fs::canonicalize(worktree_path)?;
    let wt_s = wt.to_string_lossy().into_owned();
    let cwd = std::env::current_dir()?;
    let cwd_c = std::fs::canonicalize(&cwd).unwrap_or(cwd);
    if cwd_c == wt || cwd_c.starts_with(&wt) {
        anyhow::bail!("cd out of {wt_s} before removing it");
    }
    let dirty = !git(&["status", "--porcelain"], &wt_s)?.trim().is_empty();
    if dirty && !force {
        return Err(Dirty.into());
    }

    let common = git(
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        &wt_s,
    )?
    .trim()
    .to_string();
    let main = Path::new(&common)
        .parent()
        .context("git-common-dir has no parent")?;
    let parent = main.parent().context("main repo has no parent")?;
    let main_s = main.to_str().context("main path not UTF-8")?;
    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"], &wt_s)?
        .trim()
        .to_string();

    let mut rm: Vec<&str> = vec!["worktree", "remove"];
    if force {
        rm.push("--force");
    }
    rm.push(wt_s.as_str());
    git(&rm, main_s)?;

    // Ref deletion can rewrite packed-refs, so concurrent branch deletes contend
    // on packed-refs.lock. Serialize just this step; a thread that can't take the
    // lock queues on it. (A poisoned lock still yields the guard — the critical
    // section is a git call with no invariant to corrupt.)
    {
        let _guard = branch_lock.lock().unwrap_or_else(|e| e.into_inner());
        if git(
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
            main_s,
        )
        .is_ok()
        {
            let _ = git(&["branch", "-D", &branch], main_s);
        }
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

pub fn run(
    start: &str,
    ids: &[String],
    yes: bool,
    force: bool,
    pr_only: bool,
    clean_worktree: bool,
) -> Result<()> {
    let steps = Steps::persistent();
    let targets: Vec<IssueWorktree> = if clean_worktree {
        anyhow::ensure!(
            !ids.is_empty(),
            "--clean-worktree needs one or more selectors (issue id, branch, or worktree path)"
        );
        let report =
            steps.during_result("Fetching PR + Linear status…", || gather(start, &[]))?;
        render(&report, false);
        let t = select_explicit(&report.worktrees, ids);
        if t.is_empty() {
            println!("\nNo matching worktrees.");
            return Ok(());
        }
        println!(
            "\n--clean-worktree: removing {} selected worktree(s), ignoring the PR/Linear/finished gate.",
            t.len()
        );
        t
    } else {
        let report =
            steps.during_result("Fetching PR + Linear status…", || gather(start, ids))?;
        render(&report, false);
        if pr_only {
            println!("--pr-only: Linear 'Done' gate skipped.");
        }
        let t: Vec<IssueWorktree> = report
            .worktrees
            .iter()
            .filter(|r| reason_not_finished(r, report.has_linear_key, pr_only).is_none())
            .cloned()
            .collect();
        if t.is_empty() {
            println!("\nNothing finished to clean up.");
            return Ok(());
        }
        println!("\n{} worktree(s) ready to remove:", t.len());
        t
    };

    let total = targets.len();
    let removed = AtomicUsize::new(0);
    let branch_lock = Mutex::new(());
    // Resolved before the scope so the single post-join prune has a path even if
    // every removal fails; a resolution error just skips the prune.
    let main = main_repo(start).ok();

    std::thread::scope(|s| {
        for row in &targets {
            let label = if row.issue_id != "UNKNOWN" {
                row.issue_id.clone()
            } else {
                row.branch.clone()
            };
            // The interactive decision is the only step that blocks the main
            // thread, and it blocks on nothing but a keystroke. Bars pause during
            // the prompt so a redraw never tears the stdout line.
            let go = steps.suspend(|| {
                println!("\n{label}  {}", row.worktree);
                yes || confirm(&label)
            });
            if !go {
                steps.suspend(|| println!("    skipped"));
                continue;
            }
            let steps = &steps;
            let branch_lock = &branch_lock;
            let removed = &removed;
            s.spawn(move || {
                match steps.during_result(&format!("Removing {label}…"), || {
                    cleanup(&row.worktree, &row.issue_id, force, branch_lock)
                }) {
                    Ok(()) => {
                        removed.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        let msg = if e.downcast_ref::<Dirty>().is_some() {
                            format!("    {label} is dirty — rerun with --force to discard.")
                        } else {
                            format!("    cleanup failed for {label}: {e}")
                        };
                        steps.suspend(|| eprintln!("{msg}"));
                    }
                }
            });
        }
    });

    // Every removal has joined; a single prune reclaims any stale worktree
    // entries without racing a concurrent removal.
    if let Some(main) = main {
        let _ = git(&["worktree", "prune"], &main);
    }
    println!(
        "\nRemoved {} of {}.",
        removed.load(Ordering::Relaxed),
        total
    );
    Ok(())
}
