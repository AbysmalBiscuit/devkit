use anyhow::Result;
use devkit_common::worktree::{self, IncludePlan, Worktree};
use devkit_ports::load;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

fn confirm(label: &str) -> bool {
    print!("  Overwrite in {label}? [y/N] ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

fn list(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The (worktree, issue id) pairs `selectors` names, in selector order and
/// deduplicated; every pair when `selectors` is empty. A selector that names
/// nothing is reported and skipped rather than failing the run.
fn select<'a>(rows: &'a [(Worktree, String)], selectors: &[String]) -> Vec<&'a (Worktree, String)> {
    if selectors.is_empty() {
        return rows.iter().collect();
    }
    let mut chosen = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for sel in selectors {
        let mut hit = false;
        for row in rows {
            let (wt, id) = row;
            if crate::select::matches_parts(&wt.path.to_string_lossy(), &wt.branch, id, sel) {
                hit = true;
                if seen.insert(wt.path.clone()) {
                    chosen.push(row);
                }
            }
        }
        if !hit {
            eprintln!("no worktree matches '{sel}'");
        }
    }
    chosen
}

fn report_dry(plan: &IncludePlan, overwrite: bool) {
    if !plan.missing.is_empty() {
        println!("  would copy: {}", list(&plan.missing));
    }
    if !plan.existing.is_empty() {
        if overwrite {
            println!("  would overwrite: {}", list(&plan.existing));
        } else {
            println!(
                "  would leave alone: {} (rerun with --overwrite to replace)",
                list(&plan.existing)
            );
        }
    }
    if plan.missing.is_empty() && plan.existing.is_empty() {
        println!("  nothing to copy");
    }
}

/// Copy every `defaults.worktree_include` match from the monorepo into each
/// selected worktree. Files already present are left alone unless `overwrite`,
/// which prompts once per worktree with the list it would clobber.
pub fn run(
    start: &str,
    selectors: &[String],
    overwrite: bool,
    yes: bool,
    dry_run: bool,
    config: Option<&str>,
) -> Result<()> {
    let loaded = load::load(config.map(Path::new), Path::new(start))?;
    let patterns = &loaded.config.defaults.worktree_include;
    if patterns.is_empty() {
        println!("defaults.worktree_include is empty — nothing to sync.");
        return Ok(());
    }

    let (source, worktrees) = worktree::discover(start)?;
    let rows: Vec<(Worktree, String)> = worktrees
        .into_iter()
        .map(|w| {
            let id = worktree::issue_id_of(&w.path, &w.branch);
            (w, id)
        })
        .collect();
    let targets = select(&rows, selectors);
    if targets.is_empty() {
        println!("No worktrees to sync.");
        return Ok(());
    }

    println!("Source: {}", source.display());
    for (wt, id) in targets {
        let label = if id == "UNKNOWN" { &wt.branch } else { id };
        println!("\n{label}  {}", wt.path.display());

        let plan = worktree::plan_includes(&source, &wt.path, patterns);
        for w in &plan.warnings {
            eprintln!("warning: {w}");
        }
        if dry_run {
            report_dry(&plan, overwrite);
            continue;
        }
        if overwrite && !plan.existing.is_empty() {
            println!("  will overwrite: {}", list(&plan.existing));
            if !(yes || confirm(label)) {
                println!("  skipped");
                continue;
            }
        }

        // The confirmation above is human-scale, so disk may have moved under
        // it; apply a plan taken now rather than the one that was displayed.
        let plan = worktree::plan_includes(&source, &wt.path, patterns);
        let (copied, warnings) = worktree::apply_includes(&source, &wt.path, &plan, overwrite);
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        if !overwrite {
            for rel in &plan.existing {
                eprintln!(
                    "warning: {} already exists in {label}, left alone (rerun with --overwrite to replace it)",
                    rel.display()
                );
            }
        }
        if copied == 0 {
            println!("  copied nothing");
        } else {
            let names = if overwrite {
                let mut all = plan.missing.clone();
                all.extend(plan.existing.iter().cloned());
                list(&all)
            } else {
                list(&plan.missing)
            };
            println!("  copied {copied} file(s): {names}");
        }
    }
    Ok(())
}
