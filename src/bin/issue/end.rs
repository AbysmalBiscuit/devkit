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

/// The summary file `issue setup` recorded for this worktree, read before the
/// worktree is removed and the record with it. Taken from the record rather
/// than re-derived, so a `issue_summary_path` template edited since setup
/// cannot leave the old file behind.
fn recorded_summary(worktree: &Path) -> Option<String> {
    crate::record::read(worktree)?.summary
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

/// Remove a finished worktree, delete its branch, and remove its summary file —
/// the one the record names, plus any ISSUE_*<id>*.md left in the parent of the
/// main repo. Refuses if cwd is inside the worktree,
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
    let summary = recorded_summary(&wt);

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

    if let Some(path) = summary {
        let _ = std::fs::remove_file(path);
    }

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
            println!("--pr-only: Linear 'Done' and issue-id gates skipped.");
        }
        let t: Vec<IssueWorktree> = report
            .worktrees
            .iter()
            .filter(|r| reason_not_finished(r, &report.tracker, pr_only).is_none())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("devkit-end-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_recorded_summary_path_is_what_gets_removed() {
        let dir = scratch("recorded");
        let wt = dir.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        let summary = dir.join("notes").join("ENG-1.md");
        std::fs::create_dir_all(summary.parent().unwrap()).unwrap();
        std::fs::write(&summary, "notes\n").unwrap();
        crate::record::write(
            &wt,
            &crate::record::IssueRecord {
                issue: "ENG-1".into(),
                slug: "fix".into(),
                apps: vec![],
                summary: Some(summary.display().to_string()),
            },
        )
        .unwrap();

        let found = recorded_summary(&wt).expect("record names the summary");
        std::fs::remove_file(&found).unwrap();
        assert!(!summary.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_worktree_with_no_summary_has_nothing_to_remove() {
        let dir = scratch("nosummary");
        crate::record::write(
            &dir,
            &crate::record::IssueRecord {
                issue: "ENG-2".into(),
                slug: "fix".into(),
                apps: vec![],
                summary: None,
            },
        )
        .unwrap();
        assert!(recorded_summary(&dir).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cleanup_removes_the_worktree_its_branch_and_its_summary() {
        let dir = scratch("cleanup");
        let main = dir.join("main");
        std::fs::create_dir_all(&main).unwrap();
        let g = |args: &[&str], cwd: &std::path::Path| {
            let ok = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(ok.status.success(), "git {args:?}: {ok:?}");
        };
        g(&["init", "-q", "-b", "main"], &main);
        std::fs::write(main.join("f.txt"), "x\n").unwrap();
        g(&["add", "-A"], &main);
        g(&["commit", "-qm", "init"], &main);

        let wt = dir.join("wt-eng-1");
        g(
            &["worktree", "add", "-q", "-b", "eng-1", wt.to_str().unwrap()],
            &main,
        );

        // The summary sits beside the worktree, as the default path template puts it.
        let summary = dir.join("ISSUE_SUMMARY_ENG-1.md");
        std::fs::write(&summary, "months of notes\n").unwrap();
        crate::record::write(
            &wt,
            &crate::record::IssueRecord {
                issue: "ENG-1".into(),
                slug: "fix".into(),
                apps: vec![],
                summary: Some(summary.display().to_string()),
            },
        )
        .unwrap();

        // The record itself is untracked scratch, so the tree is dirty without --force.
        cleanup(wt.to_str().unwrap(), "ENG-1", true, &Mutex::new(())).unwrap();

        assert!(!wt.exists(), "worktree removed");
        assert!(!summary.exists(), "recorded summary removed");
        let branches = std::process::Command::new("git")
            .args(["branch", "--list", "eng-1"])
            .current_dir(&main)
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&branches.stdout).trim().is_empty(),
            "branch deleted"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cleanup_leaves_a_summary_outside_the_record_alone() {
        let dir = scratch("cleanup-unrecorded");
        let main = dir.join("main");
        std::fs::create_dir_all(&main).unwrap();
        let g = |args: &[&str], cwd: &std::path::Path| {
            let ok = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(ok.status.success(), "git {args:?}: {ok:?}");
        };
        g(&["init", "-q", "-b", "main"], &main);
        std::fs::write(main.join("f.txt"), "x\n").unwrap();
        g(&["add", "-A"], &main);
        g(&["commit", "-qm", "init"], &main);
        let wt = dir.join("wt-eng-2");
        g(
            &["worktree", "add", "-q", "-b", "eng-2", wt.to_str().unwrap()],
            &main,
        );

        // Another issue's notes, in the same directory. Ending ENG-2 must not touch them.
        let other = dir.join("ISSUE_SUMMARY_ENG-99.md");
        std::fs::write(&other, "someone else\n").unwrap();

        cleanup(wt.to_str().unwrap(), "ENG-2", true, &Mutex::new(())).unwrap();
        assert!(other.exists(), "another issue's summary is untouched");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_worktree_with_no_record_has_nothing_to_remove() {
        let dir = scratch("norecord");
        assert!(recorded_summary(&dir).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
