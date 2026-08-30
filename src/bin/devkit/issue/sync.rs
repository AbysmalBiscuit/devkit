use crate::issue::select::matches_parts;
use anyhow::Result;
use devkit_common::git::Worktree;
use devkit_common::worktree::{self, IncludePlan};
use devkit_ports::load;
use std::io::{self, Write};
use std::path::Path;

fn confirm(label: &str) -> bool {
    print!("  Overwrite in {label}? [y/N] ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

/// How many files each top-level directory names before the rest are elided.
/// An include reaching a build or asset cache matches hundreds of files under
/// one directory; capping per directory keeps that flood from crowding out
/// what the other includes contributed.
const LIST_MAX: usize = 5;

/// Render `paths` as an indented block, grouped by the top-level directory
/// each one sits under and named relative to it. A directory names at most
/// `LIST_MAX` of its files before the tail collapses to a count; a path with no
/// directory component was named by an include on its own and is always shown.
/// `verbose` names every path. The block carries no trailing newline, and is
/// empty when `paths` yields nothing.
fn list<P: AsRef<Path>>(paths: impl IntoIterator<Item = P>, verbose: bool) -> String {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    let mut bare: Vec<String> = Vec::new();
    for path in paths {
        let path = path.as_ref();
        let mut components = path.components();
        let Some(first) = components.next() else {
            continue;
        };
        let rest = components.as_path();
        if rest.as_os_str().is_empty() {
            bare.push(path.display().to_string());
            continue;
        }
        let dir = first.as_os_str().to_string_lossy().into_owned();
        let file = rest.display().to_string();
        match groups.iter_mut().find(|(name, _)| *name == dir) {
            Some((_, files)) => files.push(file),
            None => groups.push((dir, vec![file])),
        }
    }

    let mut lines = Vec::new();
    let mut elided = false;
    for (dir, files) in &groups {
        let shown = if verbose {
            files.len()
        } else {
            files.len().min(LIST_MAX)
        };
        lines.push(format!("    {dir}/: {}", files[..shown].join(",\n      ")));
        let rest = files.len() - shown;
        if rest > 0 {
            lines.push(format!("      ...and {rest} more"));
            elided = true;
        }
    }
    lines.extend(bare.iter().map(|file| format!("    {file}")));
    if elided {
        lines.push("    (rerun with --verbose to name every file)".to_string());
    }
    lines.join("\n")
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
            if matches_parts(&wt.path.to_string_lossy(), &wt.branch, id, sel) {
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

/// One line per non-empty count, pluralised. Links are never folded into the
/// file count: a link is not a copied file. An empty vector means nothing was
/// written at all, which the caller reports in its own words.
pub(crate) fn counts(copied: usize, linked: usize) -> Vec<String> {
    let mut out = Vec::new();
    if copied > 0 {
        out.push(format!(
            "copied {copied} {}",
            if copied == 1 { "file" } else { "files" }
        ));
    }
    if linked > 0 {
        out.push(format!(
            "linked {linked} {}",
            if linked == 1 { "symlink" } else { "symlinks" }
        ));
    }
    out
}

fn report_dry(plan: &IncludePlan, overwrite: bool, verbose: bool) {
    if plan.missing_len() > 0 {
        println!("  would copy:\n{}", list(plan.missing(), verbose));
    }
    if plan.existing_len() > 0 {
        if overwrite {
            println!("  would overwrite:\n{}", list(plan.existing(), verbose));
        } else {
            println!(
                "  would leave alone (rerun with --overwrite to replace):\n{}",
                list(plan.existing(), verbose)
            );
        }
    }
    if plan.missing_len() == 0 && plan.existing_len() == 0 {
        println!("  nothing to copy");
    }
}

/// The boolean switches `issue sync-includes` accepts, passed as one value
/// so the call site names each of them.
pub struct Flags {
    pub overwrite: bool,
    pub all: bool,
    pub yes: bool,
    pub dry_run: bool,
    pub verbose: bool,
}

/// Copy every `defaults.worktree_include` match from the primary checkout
/// into each selected worktree. Files already present are left alone unless
/// `overwrite`,
/// which prompts once per worktree with the list it would clobber; declining
/// that prompt falls back to the default behaviour for that worktree.
/// `overwrite` replaces untracked files git cannot restore, so it needs a scope:
/// `selectors`, or `all` for every worktree.
pub fn run(start: &str, selectors: &[String], flags: Flags, config: Option<&str>) -> Result<()> {
    let Flags {
        overwrite,
        all,
        yes,
        dry_run,
        verbose,
    } = flags;
    anyhow::ensure!(
        !overwrite || dry_run || all || !selectors.is_empty(),
        "--overwrite needs one or more selectors (issue id, branch, or worktree path), or --all for every worktree"
    );
    let loaded = load::load(config.map(Path::new), Path::new(start))?;
    let patterns = &loaded.config.defaults.worktree_include;
    if patterns.is_empty() {
        println!("defaults.worktree_include is empty, nothing to sync.");
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

        let mut plan = worktree::plan_includes(&source, &wt.path, patterns);
        for w in &plan.warnings {
            eprintln!("warning: {w}");
        }
        if dry_run {
            report_dry(&plan, overwrite, verbose);
            continue;
        }

        let mut clobber = overwrite;
        if overwrite && plan.existing_len() > 0 {
            println!("  will overwrite:\n{}", list(plan.existing(), verbose));
            if !yes {
                if confirm(label) {
                    // A confirmation is human-scale, so disk may have moved
                    // under it; apply a plan taken now rather than the one the
                    // answer was given against.
                    plan = worktree::plan_includes(&source, &wt.path, patterns);
                    for w in &plan.warnings {
                        eprintln!("warning: {w}");
                    }
                } else {
                    // Declining the clobber is not declining the safe work.
                    println!("  not overwriting, copying only what is missing");
                    clobber = false;
                }
            }
        }

        let (copied, linked, warnings) =
            worktree::apply_includes(&source, &wt.path, &plan, clobber);
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        if !overwrite && plan.existing_len() > 0 {
            eprintln!(
                "warning: already in {label}, left alone (rerun with --overwrite to replace):\n{}",
                list(plan.existing(), verbose)
            );
        }
        let left_alone_links = plan.links_len() - linked;
        if !overwrite && left_alone_links > 0 {
            eprintln!(
                "warning: {left_alone_links} symlink(s) already in {label}, left alone (rerun with --overwrite to replace)"
            );
        }
        let summary = counts(copied, linked);
        if summary.is_empty() {
            println!("  copied nothing");
        } else {
            if copied > 0 {
                let names = if clobber {
                    list(plan.missing().chain(plan.existing()), verbose)
                } else {
                    list(plan.missing(), verbose)
                };
                println!("  copied {copied} file(s):\n{names}");
            }
            if linked > 0 {
                println!("  {}", summary.last().expect("linked > 0 pushed a line"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn under(dir: &str, n: usize) -> Vec<PathBuf> {
        (0..n)
            .map(|i| PathBuf::from(format!("{dir}/f{i:03}.txt")))
            .collect()
    }

    #[test]
    fn a_directory_at_the_cap_names_every_file() {
        let out = list(under("cache", LIST_MAX), false);
        assert!(!out.contains("more"), "nothing elided: {out}");
        assert!(out.contains("cache/: f000.txt"), "{out}");
        assert!(out.contains(&format!("f{:03}.txt", LIST_MAX - 1)), "{out}");
    }

    /// An include reaching an asset cache matches hundreds of files under one
    /// directory. The head is enough to recognize what is being copied; the
    /// count carries the rest.
    #[test]
    fn a_long_directory_elides_its_tail_and_counts_it() {
        let out = list(under("cache", LIST_MAX + 300), false);
        assert!(out.contains("cache/: f000.txt"), "{out}");
        assert!(
            !out.contains(&format!("f{:03}.txt", LIST_MAX)),
            "tail elided: {out}"
        );
        assert!(out.contains("...and 300 more"), "{out}");
        assert!(out.contains("--verbose"), "{out}");
    }

    /// The cap is per top-level directory, so a flood under one include cannot
    /// crowd out what another include contributed.
    #[test]
    fn each_top_level_directory_gets_its_own_cap() {
        let mut all = under("cache", LIST_MAX + 300);
        all.extend(under("assets", 2));
        all.sort();
        let out = list(&all, false);
        assert!(out.contains("assets/: f000.txt"), "{out}");
        assert!(out.contains("cache/: f000.txt"), "{out}");
        assert!(out.contains("...and 300 more"), "{out}");
    }

    /// Nesting does not split a directory into more groups: everything under
    /// one top-level component shares a single cap.
    #[test]
    fn nested_paths_group_under_their_top_level_component() {
        let mut all = under("cache/imported", 4);
        all.extend(under("cache/editor", 4));
        all.sort();
        let out = list(&all, false);
        assert_eq!(out.matches("cache/").count(), 1, "one group header: {out}");
        assert!(out.contains("...and 3 more"), "{out}");
    }

    /// A file an include named on its own is what a cache flood would
    /// otherwise bury, so it is never elided.
    #[test]
    fn bare_files_are_always_named() {
        let mut all = under("cache", LIST_MAX + 300);
        all.push(PathBuf::from("devkit.local.toml"));
        all.push(PathBuf::from(".env.local"));
        let out = list(&all, false);
        assert!(out.contains("devkit.local.toml"), "{out}");
        assert!(out.contains(".env.local"), "{out}");
    }

    #[test]
    fn verbose_names_every_path() {
        let out = list(under("cache", LIST_MAX + 300), true);
        assert!(
            out.contains(&format!("f{:03}.txt", LIST_MAX + 299)),
            "last path present: {out}"
        );
        assert!(!out.contains("more"), "{out}");
    }

    #[test]
    fn an_empty_list_is_empty() {
        assert_eq!(list(Vec::<PathBuf>::new(), false), "");
    }

    #[test]
    fn counts_name_each_kind_separately() {
        assert_eq!(counts(0, 0), Vec::<String>::new());
        assert_eq!(counts(1, 0), vec!["copied 1 file"]);
        assert_eq!(counts(3, 0), vec!["copied 3 files"]);
        assert_eq!(counts(0, 1), vec!["linked 1 symlink"]);
        assert_eq!(counts(0, 2), vec!["linked 2 symlinks"]);
        assert_eq!(counts(2, 1), vec!["copied 2 files", "linked 1 symlink"]);
    }
}
