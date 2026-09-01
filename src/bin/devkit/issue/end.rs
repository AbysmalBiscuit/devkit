use anyhow::{Context, Result};
use devkit_common::progress::Steps;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Mutex;

use crate::issue::triage::render;
use devkit_issue::status::{IssueWorktree, gather_with, label, reason_not_finished};

fn select_explicit(rows: &[IssueWorktree], selectors: &[String]) -> Vec<IssueWorktree> {
    let mut chosen = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for sel in selectors {
        let hits: Vec<&IssueWorktree> = rows
            .iter()
            .filter(|r| crate::issue::select::matches(r, sel))
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
    devkit_common::record::read(worktree)?.summary
}

/// Whether `name` is an `ISSUE_*.md` file belonging to `issue_id`, for records
/// written before they carried an exact summary path. The filename spells the
/// tracker's canonical id while the record spells whatever setup was given, so
/// the id match ignores case.
fn is_legacy_summary(name: &str, issue_id: &str) -> bool {
    name.starts_with("ISSUE_")
        && name.ends_with(".md")
        && name.to_lowercase().contains(&issue_id.to_lowercase())
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

/// The primary checkout as a string, resolved once so a single post-removal
/// `git worktree prune` can run after all removals join. `start` is the base
/// dir (`.` by default), and works from inside a worktree or from the primary
/// checkout itself.
fn main_repo(start: &str) -> Result<String> {
    devkit_common::git::primary_checkout(Path::new(start))?
        .to_str()
        .map(str::to_string)
        .context("primary checkout path not UTF-8")
}

/// Remove a finished worktree, delete its branch, and remove its summary file.
/// The record names that file exactly; only a record that names none falls back
/// to sweeping the parent of the main repo for an `ISSUE_*<id>*.md` belonging to
/// this issue, so a run that knows its summary path touches nothing else in a
/// directory it does not own. Refuses if cwd is inside the worktree,
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
    let dirty = !devkit_common::git::Git::at(&wt)
        .args(["status", "--porcelain"])
        .output()?
        .trim()
        .is_empty();
    if dirty && !force {
        return Err(Dirty.into());
    }
    let summary = recorded_summary(&wt);

    let main = devkit_common::git::primary_checkout(&wt)?;
    let parent = main.parent().context("main repo has no parent")?;
    let branch = devkit_common::git::branch(&wt)?;

    let mut rm: Vec<&str> = vec!["worktree", "remove"];
    if force {
        rm.push("--force");
    }
    rm.push(wt_s.as_str());
    devkit_common::git::Git::at(&main)
        .args(rm)
        .timeout(devkit_common::git::SLOW_TIMEOUT)
        .output()?;

    // Ref deletion can rewrite packed-refs, so concurrent branch deletes contend
    // on packed-refs.lock. Serialize just this step; a thread that can't take the
    // lock queues on it. (A poisoned lock still yields the guard — the critical
    // section is a git call with no invariant to corrupt.)
    {
        let _guard = branch_lock.lock().unwrap_or_else(|e| e.into_inner());
        if devkit_common::git::Git::at(&main)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ])
            .success()?
        {
            let _ = devkit_common::git::Git::at(&main)
                .args(["branch", "-D", &branch])
                .output();
        }
    }

    match summary {
        Some(path) => {
            let _ = std::fs::remove_file(path);
        }
        // Records written before they carried a summary path give no way to
        // name the file, so the parent of the primary checkout — where the
        // default template puts it — is scanned for one belonging to this
        // issue.
        None => {
            if let Ok(read) = std::fs::read_dir(parent) {
                for ent in read.flatten() {
                    let name = ent.file_name().to_string_lossy().into_owned();
                    if is_legacy_summary(&name, issue_id) {
                        let _ = std::fs::remove_file(ent.path());
                    }
                }
            }
        }
    }
    Ok(())
}

/// How a worktree is named in prompts, steps, and errors: its issue id when the
/// record has one, else its branch.
fn row_label(row: &IssueWorktree) -> String {
    if row.issue_id != "UNKNOWN" {
        row.issue_id.clone()
    } else {
        row.branch.clone()
    }
}

/// The gates and waivers `issue end` was invoked with. Grouped because five
/// bare `bool` parameters in a row transpose silently at the call site.
pub struct EndFlags {
    /// Remove without prompting per worktree.
    pub yes: bool,
    /// Remove a worktree with uncommitted changes, discarding them.
    pub force: bool,
    /// Judge finished on the PR alone, skipping the issue-state and id gates.
    pub pr_only: bool,
    /// Remove the named worktrees whether or not they look finished.
    pub clean_worktree: bool,
    /// Remove without copying anything out first.
    pub no_preserve: bool,
}

/// The approved worktrees that were actually removed, in approval order. The
/// removal phase runs in parallel and finishes out of order, so its unordered
/// result is put back into the order the prompts ran in before any hook or
/// report reads it.
fn removed_in_order(
    approved: &[IssueWorktree],
    removed: &std::collections::HashSet<String>,
) -> Vec<String> {
    approved
        .iter()
        .map(|r| r.worktree.clone())
        .filter(|w| removed.contains(w))
        .collect()
}

/// The `after_worktree_remove` render context for each approved worktree,
/// keyed by its path. Built before the removal phase: `issue`, `slug` and
/// `apps` come from `.devkit/issue.toml`, which the removal deletes along with
/// everything else in the worktree.
fn remove_contexts(
    approved: &[IssueWorktree],
    prefix: &str,
    worktree_root: &Path,
    primary: Option<&Path>,
) -> std::collections::HashMap<String, serde_json::Value> {
    approved
        .iter()
        .map(|row| {
            let wt = Path::new(&row.worktree);
            let record = devkit_common::record::read(wt);
            (
                row.worktree.clone(),
                crate::issue::preserve::context(
                    wt,
                    &row.branch,
                    record.as_ref(),
                    prefix,
                    worktree_root,
                    primary,
                ),
            )
        })
        .collect()
}

/// The `after_end` render context. A run may have removed several worktrees,
/// so it carries the list rather than any one worktree's identity, and omits
/// `worktree`, `branch`, `issue` and `slug` entirely: there is no honest single
/// value for them.
fn end_context(
    removed: &[String],
    prefix: &str,
    worktree_root: &Path,
    primary: Option<&Path>,
) -> serde_json::Value {
    let mut ctx = serde_json::json!({
        "removed": removed,
        "count": removed.len(),
        "prefix": prefix,
        "worktree_root": worktree_root.display().to_string(),
    });
    if let Some(primary) = primary {
        ctx["primary"] = serde_json::Value::String(primary.display().to_string());
    }
    ctx
}

pub fn run(start: &str, ids: &[String], flags: EndFlags, config: Option<&str>) -> Result<()> {
    let EndFlags {
        yes,
        force,
        pr_only,
        clean_worktree,
        no_preserve,
    } = flags;
    let steps = Steps::persistent();
    let sel = crate::issue::tracker::select_full(config, start, None);
    // A config that does not load reads as an empty [preserve] table, which
    // would remove a worktree having archived nothing it was asked to keep.
    if !no_preserve && let devkit_config::Health::Broken(why) = &sel.health {
        anyhow::bail!(
            "devkit.toml does not load, so [preserve] entries cannot be read: {why}\n\
             rerun with --no-preserve to remove without preserving anything"
        );
    }
    let (tracker, repos) = (sel.tracker, sel.repos);
    let targets: Vec<IssueWorktree> = if clean_worktree {
        anyhow::ensure!(
            !ids.is_empty(),
            "--clean-worktree needs one or more selectors (issue id, branch, or worktree path)"
        );
        let report = steps.during_result("Fetching PR + issue status…", || {
            gather_with(start, &[], &tracker, &repos)
        })?;
        render(&report, false);
        let t = select_explicit(&report.worktrees, ids);
        if t.is_empty() {
            println!("\nNo matching worktrees.");
            return Ok(());
        }
        println!(
            "\n--clean-worktree: removing {} selected worktree(s), ignoring the PR/state/finished gate.",
            t.len()
        );
        t
    } else {
        let report = steps.during_result("Fetching PR + issue status…", || {
            gather_with(start, ids, &tracker, &repos)
        })?;
        render(&report, false);
        if pr_only {
            println!(
                "--pr-only: the {} state and issue-id gates are skipped.",
                label(report.tracker.kind)
            );
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

    // Phase 1: every prompt precedes every action, so nothing is being removed
    // while the next question is on screen.
    let mut approved: Vec<IssueWorktree> = Vec::new();
    for row in &targets {
        let label = row_label(row);
        // The interactive decision is the only step that blocks the main
        // thread, and it blocks on nothing but a keystroke. Bars pause during
        // the prompt so a redraw never tears the stdout line.
        let go = steps.suspend(|| {
            println!("\n{label}  {}", row.worktree);
            yes || confirm(&label)
        });
        if go {
            approved.push(row.clone());
        } else {
            steps.suspend(|| println!("    skipped"));
        }
    }
    if approved.is_empty() {
        println!("\nNothing to remove.");
        return Ok(());
    }

    // Resolved before any removal so the single post-join prune has a path even
    // if every removal fails; a resolution error just skips the prune. Phase 2
    // renders its context against it too.
    let main = main_repo(start).ok();

    // Phase 2: serial, and complete before the first removal. That ordering is
    // what makes a destination collision resolve in worktree order and a
    // `required` failure surface while every file still exists.
    let entries = crate::issue::preserve::preserve_entries(sel.config.as_ref(), no_preserve);
    // The status report's spelling of each worktree path, never a canonicalized
    // one. Canonicalizing on Windows yields a verbatim `\\?\` prefix, which
    // compares unequal to the drive prefix a destination carries, so the
    // containment check would stop rejecting anything.
    let removal_roots: Vec<std::path::PathBuf> = approved
        .iter()
        .map(|r| std::path::PathBuf::from(&r.worktree))
        .collect();
    let vars = sel
        .config
        .as_ref()
        .map(|c| c.templates.variables.clone())
        .unwrap_or_default();
    let (wt_root, prefix) = sel
        .config
        .as_ref()
        .map(|c| {
            (
                devkit_config::expand_tilde(&c.defaults.worktree_root),
                c.defaults.branch_prefix.clone(),
            )
        })
        .unwrap_or_default();

    // Read before the removal phase so a hook can name the issue and slug of a
    // worktree whose record no longer exists.
    let empty: &[Vec<String>] = &[];
    let cfg_hooks = sel.config.as_ref().map(|c| &c.hooks);
    let after_worktree_remove = cfg_hooks.map_or(empty, |h| h.after_worktree_remove.as_slice());
    let after_end = cfg_hooks.map_or(empty, |h| h.after_end.as_slice());
    let remove_ctxs = if after_worktree_remove.is_empty() {
        std::collections::HashMap::new()
    } else {
        remove_contexts(&approved, &prefix, &wt_root, main.as_deref().map(Path::new))
    };

    let mut blocked: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut files = 0usize;
    // Entry names rather than a count of runs: one entry that archived for
    // three worktrees is one entry, not three.
    let mut archived: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut required_failures = 0usize;
    if !entries.is_empty() {
        let primary = main.as_deref().map(Path::new);
        for row in &approved {
            let label = row_label(row);
            // The report's spelling again: `glob` matches nothing behind a
            // verbatim UNC prefix, so a canonicalized path would silently
            // preserve nothing from a repository on a network share.
            let wt = Path::new(&row.worktree);
            let record = devkit_common::record::read(wt);
            let ctx = crate::issue::preserve::context(
                wt,
                &row.branch,
                record.as_ref(),
                &prefix,
                &wt_root,
                primary,
            );
            let out = steps.during_ok(&format!("Preserving {label}…"), || {
                let out =
                    crate::issue::preserve::run_for(wt, &entries, &ctx, &vars, &removal_roots);
                let ok = out.required_failure.is_none();
                (out, ok)
            });
            files += out.files;
            archived.extend(out.archived);
            for w in &out.warnings {
                steps.suspend(|| eprintln!("warning: {w}"));
            }
            if let Some(err) = out.required_failure {
                steps.suspend(|| eprintln!("    {label} kept: {err}"));
                blocked.insert(row.worktree.clone());
                required_failures += 1;
            }
        }
    }

    // Phase 3: the removals, in parallel.
    let total = approved.len() - blocked.len();
    let removed: Mutex<std::collections::HashSet<String>> =
        Mutex::new(std::collections::HashSet::new());
    let branch_lock = Mutex::new(());
    std::thread::scope(|s| {
        for row in approved.iter().filter(|r| !blocked.contains(&r.worktree)) {
            let label = row_label(row);
            let steps = &steps;
            let branch_lock = &branch_lock;
            let removed = &removed;
            s.spawn(move || {
                match steps.during_result(&format!("Removing {label}…"), || {
                    cleanup(&row.worktree, &row.issue_id, force, branch_lock)
                }) {
                    Ok(()) => {
                        removed.lock().unwrap().insert(row.worktree.clone());
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
    let removed = removed_in_order(&approved, &removed.into_inner().unwrap());

    // Every removal has joined; a single prune reclaims any stale worktree
    // entries without racing a concurrent removal.
    if let Some(main) = &main {
        let _ = devkit_common::git::Git::at(Path::new(main))
            .args(["worktree", "prune"])
            .output();
    }
    println!();
    if !archived.is_empty() {
        println!(
            "Preserved {files} file(s) across {} entr{}.",
            archived.len(),
            if archived.len() == 1 { "y" } else { "ies" }
        );
    }
    println!("Removed {} of {}.", removed.len(), total);
    // After the summary and after the prune: a hook sees every removal
    // finished, and its progress step cannot tear the report. A run that
    // removed nothing changed nothing on disk, so nothing fires.
    if !removed.is_empty() && !(after_worktree_remove.is_empty() && after_end.is_empty()) {
        match main.as_deref() {
            Some(root) => {
                let root = Path::new(root);
                for wt in &removed {
                    let Some(ctx) = remove_ctxs.get(wt) else {
                        continue;
                    };
                    crate::issue::hooks::run_all(
                        root,
                        "after_worktree_remove",
                        after_worktree_remove,
                        ctx,
                        &vars,
                        &steps,
                    );
                }
                let ctx = end_context(&removed, &prefix, &wt_root, Some(root));
                crate::issue::hooks::run_all(root, "after_end", after_end, &ctx, &vars, &steps);
            }
            // The worktree the command was run from is usually the one just
            // removed, so there is no directory left to inherit.
            None => eprintln!(
                "warning: after_worktree_remove and after_end hooks skipped: the main repository root did not resolve"
            ),
        }
    }
    anyhow::ensure!(
        required_failures == 0,
        "{required_failures} worktree(s) kept: a required preserve entry failed"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The summary filename carries the tracker's canonical id, which Linear
    /// spells in caps, while the record carries whatever spelling setup was
    /// given. The sweep has to bridge that.
    #[test]
    fn the_legacy_sweep_matches_a_lowercase_id_against_an_uppercase_filename() {
        assert!(is_legacy_summary("ISSUE_SUMMARY_ENG-1234.md", "eng-1234"));
        assert!(is_legacy_summary("ISSUE_SUMMARY_ENG-1234.md", "ENG-1234"));
        assert!(is_legacy_summary("ISSUE_NOTES_eng-1234.md", "ENG-1234"));
    }

    /// Case is the only thing that widened: an unrelated id, a filename without
    /// the prefix, and a non-markdown extension are all still rejected.
    #[test]
    fn the_legacy_sweep_rejects_a_different_issue() {
        assert!(!is_legacy_summary("ISSUE_SUMMARY_ENG-1234.md", "ops-99"));
        assert!(!is_legacy_summary("NOTES_ENG-1234.md", "eng-1234"));
        assert!(!is_legacy_summary("ISSUE_SUMMARY_ENG-1234.txt", "eng-1234"));
    }

    #[test]
    fn the_recorded_summary_path_is_what_gets_removed() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        let summary = dir.path().join("notes").join("ENG-1.md");
        std::fs::create_dir_all(summary.parent().unwrap()).unwrap();
        std::fs::write(&summary, "notes\n").unwrap();
        devkit_common::record::write(
            &wt,
            &devkit_common::record::IssueRecord {
                issue: "ENG-1".into(),
                slug: "fix".into(),
                apps: vec![],
                summary: Some(summary.display().to_string()),
                pr: None,
            },
        )
        .unwrap();

        let found = recorded_summary(&wt).expect("record names the summary");
        std::fs::remove_file(&found).unwrap();
        assert!(!summary.exists());
    }

    #[test]
    fn a_worktree_with_no_summary_has_nothing_to_remove() {
        let dir = tempfile::tempdir().unwrap();
        devkit_common::record::write(
            dir.path(),
            &devkit_common::record::IssueRecord {
                issue: "ENG-2".into(),
                slug: "fix".into(),
                apps: vec![],
                summary: None,
                pr: None,
            },
        )
        .unwrap();
        assert!(recorded_summary(dir.path()).is_none());
    }

    #[test]
    fn cleanup_removes_the_worktree_its_branch_and_its_summary() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        let g = |args: &[&str], cwd: &std::path::Path| {
            devkit_common::git::Git::fixture(cwd)
                .args(args.iter().copied())
                .output()
                .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
        };
        g(&["init", "-q", "-b", "main"], &main);
        std::fs::write(main.join("f.txt"), "x\n").unwrap();
        g(&["add", "-A"], &main);
        g(&["commit", "-qm", "init"], &main);

        let wt = dir.path().join("wt-eng-1");
        g(
            &["worktree", "add", "-q", "-b", "eng-1", wt.to_str().unwrap()],
            &main,
        );

        // The summary sits beside the worktree, as the default path template puts it.
        let summary = dir.path().join("ISSUE_SUMMARY_ENG-1.md");
        std::fs::write(&summary, "months of notes\n").unwrap();
        devkit_common::record::write(
            &wt,
            &devkit_common::record::IssueRecord {
                issue: "ENG-1".into(),
                slug: "fix".into(),
                apps: vec![],
                summary: Some(summary.display().to_string()),
                pr: None,
            },
        )
        .unwrap();

        // The record itself is untracked scratch, so the tree is dirty without --force.
        cleanup(wt.to_str().unwrap(), "ENG-1", true, &Mutex::new(())).unwrap();

        assert!(!wt.exists(), "worktree removed");
        assert!(!summary.exists(), "recorded summary removed");
        let branches = devkit_common::git::Git::fixture(&main)
            .args(["branch", "--list", "eng-1"])
            .output()
            .unwrap();
        assert!(branches.trim().is_empty(), "branch deleted");
    }

    #[test]
    fn cleanup_leaves_a_summary_outside_the_record_alone() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        let g = |args: &[&str], cwd: &std::path::Path| {
            devkit_common::git::Git::fixture(cwd)
                .args(args.iter().copied())
                .output()
                .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
        };
        g(&["init", "-q", "-b", "main"], &main);
        std::fs::write(main.join("f.txt"), "x\n").unwrap();
        g(&["add", "-A"], &main);
        g(&["commit", "-qm", "init"], &main);
        let wt = dir.path().join("wt-eng-2");
        g(
            &["worktree", "add", "-q", "-b", "eng-2", wt.to_str().unwrap()],
            &main,
        );

        // Another issue's notes, in the same directory. Ending ENG-2 must not touch them.
        let other = dir.path().join("ISSUE_SUMMARY_ENG-99.md");
        std::fs::write(&other, "someone else\n").unwrap();

        cleanup(wt.to_str().unwrap(), "ENG-2", true, &Mutex::new(())).unwrap();
        assert!(other.exists(), "another issue's summary is untouched");
    }

    /// The sweep is the fallback for a record that names no summary. Once the
    /// record names one, that file is the whole target: an `ISSUE_*` file for
    /// the same issue sitting beside the primary checkout is somebody else's,
    /// including an archive a `[preserve]` entry just wrote there.
    #[test]
    fn cleanup_does_not_sweep_when_the_record_names_a_summary() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        let g = |args: &[&str], cwd: &std::path::Path| {
            devkit_common::git::Git::fixture(cwd)
                .args(args.iter().copied())
                .output()
                .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
        };
        g(&["init", "-q", "-b", "main"], &main);
        std::fs::write(main.join("f.txt"), "x\n").unwrap();
        g(&["add", "-A"], &main);
        g(&["commit", "-qm", "init"], &main);
        let wt = dir.path().join("wt-eng-4");
        g(
            &["worktree", "add", "-q", "-b", "eng-4", wt.to_str().unwrap()],
            &main,
        );

        let recorded = dir.path().join("ISSUE_SUMMARY_ENG-4.md");
        std::fs::write(&recorded, "the summary\n").unwrap();
        let archived = dir.path().join("ISSUE_NOTES_ENG-4.md");
        std::fs::write(&archived, "preserved\n").unwrap();
        devkit_common::record::write(
            &wt,
            &devkit_common::record::IssueRecord {
                issue: "ENG-4".into(),
                slug: "fix".into(),
                apps: vec![],
                summary: Some(recorded.display().to_string()),
                pr: None,
            },
        )
        .unwrap();

        cleanup(wt.to_str().unwrap(), "ENG-4", true, &Mutex::new(())).unwrap();

        assert!(!recorded.exists(), "the recorded summary is removed");
        assert!(
            archived.exists(),
            "a same-issue file the record did not name survives"
        );
    }

    #[test]
    fn a_worktree_with_no_record_has_nothing_to_remove() {
        let dir = tempfile::tempdir().unwrap();
        assert!(recorded_summary(dir.path()).is_none());
    }

    /// `run_for` never deletes: a required entry that fails reports the reason
    /// and leaves the worktree, its branch, and its summary untouched, which is
    /// what lets the caller keep a blocked worktree out of the removal phase.
    #[test]
    fn a_blocked_worktree_keeps_its_branch_and_summary() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        let g = |args: &[&str], cwd: &std::path::Path| {
            devkit_common::git::Git::fixture(cwd)
                .args(args.iter().copied())
                .output()
                .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
        };
        g(&["init", "-q", "-b", "main"], &main);
        std::fs::write(main.join("f.txt"), "x\n").unwrap();
        g(&["add", "-A"], &main);
        g(&["commit", "-qm", "init"], &main);
        let wt = dir.path().join("wt-eng-3");
        g(
            &["worktree", "add", "-q", "-b", "eng-3", wt.to_str().unwrap()],
            &main,
        );
        let summary = dir.path().join("ISSUE_SUMMARY_ENG-3.md");
        std::fs::write(&summary, "notes\n").unwrap();

        // A required entry that cannot resolve: `to` is relative.
        let entry = devkit_config::PreserveConfig {
            from: vec!["a.md".into()],
            to: "relative/path".into(),
            required: true,
        };
        let out = crate::issue::preserve::run_for(
            &wt,
            &[("notes".to_string(), &entry)],
            &crate::issue::preserve::context(&wt, "eng-3", None, "", dir.path(), Some(&main)),
            &std::collections::BTreeMap::new(),
            &[],
        );

        assert!(out.required_failure.is_some());
        assert!(wt.exists(), "worktree untouched");
        assert!(summary.exists(), "summary untouched");
        let branches = devkit_common::git::Git::fixture(&main)
            .args(["branch", "--list", "eng-3"])
            .output()
            .unwrap();
        assert!(!branches.trim().is_empty(), "branch untouched");
    }

    fn approved_row(worktree: &str, branch: &str, issue_id: &str) -> IssueWorktree {
        IssueWorktree {
            worktree: worktree.into(),
            branch: branch.into(),
            issue_id: issue_id.into(),
            dirty: false,
            pr: devkit_issue::status::PrStatus::None,
            state: None,
            finished: true,
            reason_not_finished: None,
        }
    }

    #[test]
    fn removed_worktrees_come_back_in_approval_order() {
        let approved = vec![
            approved_row("/wt/a", "lev/a", "ENG-1"),
            approved_row("/wt/b", "lev/b", "ENG-2"),
            approved_row("/wt/c", "lev/c", "ENG-3"),
        ];
        // The parallel removal phase finishes in whatever order it finishes.
        let done: std::collections::HashSet<String> = ["/wt/c".to_string(), "/wt/a".to_string()]
            .into_iter()
            .collect();
        assert_eq!(
            removed_in_order(&approved, &done),
            vec!["/wt/a".to_string(), "/wt/c".to_string()]
        );
    }

    #[test]
    fn a_worktree_that_was_not_removed_is_left_out() {
        let approved = vec![approved_row("/wt/a", "lev/a", "ENG-1")];
        let done = std::collections::HashSet::new();
        assert!(removed_in_order(&approved, &done).is_empty());
    }

    #[test]
    fn the_remove_context_reads_the_record_before_removal() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("eng-9-fix");
        std::fs::create_dir_all(&wt).unwrap();
        devkit_common::record::write(
            &wt,
            &devkit_common::record::IssueRecord {
                issue: "ENG-9".into(),
                slug: "fix".into(),
                apps: vec!["web".into()],
                summary: None,
                pr: None,
            },
        )
        .unwrap();
        let approved = vec![approved_row(wt.to_str().unwrap(), "lev/eng-9-fix", "ENG-9")];

        let ctxs = remove_contexts(&approved, "lev/", dir.path(), None);

        let ctx = &ctxs[wt.to_str().unwrap()];
        assert_eq!(ctx["issue"], "ENG-9");
        assert_eq!(ctx["slug"], "fix");
        assert_eq!(ctx["branch"], "lev/eng-9-fix");
        assert_eq!(ctx["worktree"], wt.display().to_string());
    }

    #[test]
    fn a_worktree_with_no_record_still_gets_a_context() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("no-record");
        std::fs::create_dir_all(&wt).unwrap();
        let approved = vec![approved_row(wt.to_str().unwrap(), "lev/x", "ENG-0")];

        let ctxs = remove_contexts(&approved, "lev/", dir.path(), None);

        assert_eq!(ctxs[wt.to_str().unwrap()]["issue"], "");
    }

    #[test]
    fn the_end_context_carries_every_removed_worktree() {
        let removed = vec!["/wt/a".to_string(), "/wt/b".to_string()];
        let ctx = end_context(&removed, "lev/", Path::new("/wt"), Some(Path::new("/repo")));
        assert_eq!(ctx["removed"][0], "/wt/a");
        assert_eq!(ctx["removed"][1], "/wt/b");
        assert_eq!(ctx["count"], 2);
        assert_eq!(ctx["prefix"], "lev/");
        assert_eq!(ctx["worktree_root"], "/wt");
        assert_eq!(ctx["primary"], "/repo");
    }

    #[test]
    fn the_end_context_omits_a_primary_that_did_not_resolve() {
        let ctx = end_context(&["/wt/a".to_string()], "lev/", Path::new("/wt"), None);
        assert!(ctx.get("primary").is_none());
    }

    #[test]
    fn the_end_context_names_no_single_worktree() {
        let ctx = end_context(&["/wt/a".to_string()], "lev/", Path::new("/wt"), None);
        for key in ["worktree", "branch", "issue", "slug", "apps"] {
            assert!(
                ctx.get(key).is_none(),
                "a run-level context must not carry `{key}`"
            );
        }
    }
}
