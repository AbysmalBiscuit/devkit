//! Copying a worktree's files out before `issue end` removes it. Resolution and
//! validation live here; the copy itself is `devkit_common::worktree::copy_out`.

use devkit_common::record::IssueRecord;
use devkit_config::PreserveConfig;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// One entry resolved against one worktree: the patterns to glob and the
/// directory they land in.
#[derive(Debug)]
pub(crate) struct Resolved {
    pub name: String,
    pub patterns: Vec<String>,
    pub dest: PathBuf,
}

/// An entry that will not run, and the sentence explaining why. Fail-open turns
/// this into a warning; `required` turns it into an error.
#[derive(Debug)]
pub(crate) struct Skipped {
    pub name: String,
    pub reason: String,
}

/// The minijinja context an entry's `from` and `to` render against. `issue`,
/// `slug`, and `apps` come from the record rather than being re-derived, so a
/// template edited since setup cannot misname the destination; `record::read`
/// returns `None` for a malformed record as well as an absent one, and both take
/// the same empty defaults.
///
/// `primary` is `None` when the primary checkout could not be resolved, and the
/// key is then left out of the context entirely. Rendering is strict about
/// undefined names, so an entry that uses `{{ primary }}` fails with that name
/// in the message instead of quietly building a destination from a stand-in
/// path.
pub(crate) fn context(
    worktree: &Path,
    branch: &str,
    record: Option<&IssueRecord>,
    prefix: &str,
    worktree_root: &Path,
    primary: Option<&Path>,
) -> serde_json::Value {
    let mut ctx = serde_json::json!({
        "worktree": worktree.display().to_string(),
        "branch": branch,
        "issue": record.map(|r| r.issue.as_str()).unwrap_or_default(),
        "slug": record.map(|r| r.slug.as_str()).unwrap_or_default(),
        "apps": record.map(|r| r.apps.clone()).unwrap_or_default(),
        "prefix": prefix,
        "worktree_root": worktree_root.display().to_string(),
    });
    if let Some(primary) = primary {
        ctx["primary"] = serde_json::Value::String(primary.display().to_string());
    }
    ctx
}

/// The deepest ancestor of `p` that exists, canonicalized. `to` names a
/// directory that has not been created yet, so `p` itself rarely resolves —
/// but a destination inside a removal root has its whole existing prefix
/// inside that root too, since the root is a worktree that exists. Comparing
/// the prefix therefore answers the same question as comparing `p` would.
fn existing_ancestor(p: &Path) -> Option<PathBuf> {
    p.ancestors().find_map(|a| std::fs::canonicalize(a).ok())
}

/// Whether `dest` is `root` or sits beneath it. The filesystem answers rather
/// than a string compare: NTFS is case-insensitive, so a destination differing
/// only in case names the same directory, and a symlink into `root` names it
/// too. Both sides canonicalize, so Windows' verbatim `\?\` prefix is either
/// on both or on neither and never makes a pair compare unequal. Paths that
/// cannot be canonicalized — a permissions error, or a root already gone —
/// fall back to the lexical compare, which both arguments arrive normalized
/// for.
fn is_within(dest: &Path, root: &Path) -> bool {
    match (existing_ancestor(dest), std::fs::canonicalize(root)) {
        (Some(d), Ok(r)) => d.starts_with(r),
        _ => dest.starts_with(root),
    }
}

/// What the context is missing, as a clause to append to a render failure.
/// minijinja reports a strict-undefined failure as `undefined value` without
/// naming the name, so an entry using `{{ primary }}` in a project whose
/// primary checkout would not resolve gets the reason rather than a riddle.
fn absent_note(ctx: &serde_json::Value) -> &'static str {
    if ctx.get("primary").is_none() {
        " (the primary checkout could not be resolved, so `primary` is unset)"
    } else {
        ""
    }
}

/// Render and validate one entry. `removal_roots` are the worktrees this run
/// will delete, spelled as the status report gives them: a destination under any
/// of them would be archived and then deleted seconds later.
pub(crate) fn resolve_entry(
    name: &str,
    cfg: &PreserveConfig,
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
    removal_roots: &[PathBuf],
) -> Result<Resolved, Skipped> {
    let skip = |reason: String| Skipped {
        name: name.to_string(),
        reason,
    };

    let mut patterns = Vec::with_capacity(cfg.from.len());
    for p in &cfg.from {
        match devkit_common::template::render(p, ctx, vars) {
            Ok(r) if r.trim().is_empty() => {}
            Ok(r) => patterns.push(r.trim().to_string()),
            Err(e) => {
                return Err(skip(format!(
                    "rendering `from` entry `{p}`: {e:#}{}",
                    absent_note(ctx)
                )));
            }
        }
    }

    let rendered = devkit_common::template::render(&cfg.to, ctx, vars)
        .map_err(|e| skip(format!("rendering `to`: {e:#}{}", absent_note(ctx))))?;
    let to = rendered.trim();
    if to.is_empty() {
        return Err(skip("`to` rendered empty".into()));
    }
    // Normalized before the containment check so the lexical fallback inside
    // `is_within` still resolves `..`, which would otherwise walk past a
    // component-prefix test. `to` renders from the worktree's own record,
    // which anything working in that worktree can write.
    let dest = devkit_config::normalize_lexically(Path::new(to));
    if !dest.is_absolute() {
        return Err(skip(format!("`to` must be an absolute path, got `{to}`")));
    }
    if let Some(root) = removal_roots
        .iter()
        .map(|r| devkit_config::normalize_lexically(r))
        .find(|r| is_within(&dest, r))
    {
        return Err(skip(format!(
            "`to` is inside {}, which this run removes",
            root.display()
        )));
    }

    Ok(Resolved {
        name: name.to_string(),
        patterns,
        dest,
    })
}

/// What preserving one worktree produced. `required_failure` is set only when a
/// `required` entry could not run, and is the caller's reason to keep the
/// worktree.
pub(crate) struct Outcome {
    pub files: usize,
    /// The entries that copied at least one file, by name. A caller
    /// summarising several worktrees unions these, so one entry that ran for
    /// three worktrees is still one entry.
    pub archived: BTreeSet<String>,
    pub warnings: Vec<String>,
    pub required_failure: Option<String>,
}

/// The config's preserve entries in sorted key order, which is the order they
/// run in: two entries writing to one destination resolve deterministically
/// rather than by hash iteration. Empty when preservation is waived, when no
/// config loaded, or when the table is empty.
pub(crate) fn preserve_entries(
    config: Option<&devkit_config::Config>,
    no_preserve: bool,
) -> Vec<(String, &PreserveConfig)> {
    if no_preserve {
        return Vec::new();
    }
    let Some(config) = config else {
        return Vec::new();
    };
    let mut names: Vec<&String> = config.preserve.keys().collect();
    names.sort();
    names
        .into_iter()
        .map(|name| (name.clone(), &config.preserve[name]))
        .collect()
}

/// Preserve one worktree. `entries` are the config's preserve entries in sorted
/// key order. Fail-open per entry: a failure warns and the next entry still
/// runs, unless the entry is `required`, which stops this worktree and leaves it
/// for the caller to keep.
pub(crate) fn run_for(
    worktree: &Path,
    entries: &[(String, &PreserveConfig)],
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
    removal_roots: &[PathBuf],
) -> Outcome {
    let mut out = Outcome {
        files: 0,
        archived: BTreeSet::new(),
        warnings: Vec::new(),
        required_failure: None,
    };

    for (name, cfg) in entries {
        match resolve_entry(name, cfg, ctx, vars, removal_roots) {
            Err(skipped) => {
                let msg = format!("preserve `{}`: {}", skipped.name, skipped.reason);
                if cfg.required {
                    out.required_failure = Some(msg);
                    return out;
                }
                out.warnings.push(msg);
            }
            Ok(resolved) => {
                let (files, warnings) =
                    devkit_common::worktree::copy_out(worktree, &resolved.dest, &resolved.patterns);
                // Tallied before the `required` check: a copy that already
                // happened counts even when a later pattern in the same entry
                // warns and stops the worktree.
                out.files += files;
                if files > 0 {
                    out.archived.insert(resolved.name.clone());
                }
                if !warnings.is_empty() && cfg.required {
                    out.required_failure = Some(format!(
                        "preserve `{}`: {}",
                        resolved.name,
                        warnings.join("; ")
                    ));
                    return out;
                }
                out.warnings.extend(
                    warnings
                        .into_iter()
                        .map(|w| format!("preserve `{}`: {w}", resolved.name)),
                );
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An absolute path on both platforms. A `/`-rooted path is only
    /// root-relative on Windows, where `is_absolute` wants a drive prefix.
    fn abs(p: &str) -> String {
        if cfg!(windows) {
            format!("C:{p}")
        } else {
            p.to_string()
        }
    }

    fn cfg(from: &[&str], to: &str, required: bool) -> devkit_config::PreserveConfig {
        devkit_config::PreserveConfig {
            from: from.iter().map(|s| s.to_string()).collect(),
            to: to.to_string(),
            required,
        }
    }

    fn novars() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    fn ctx_for(issue: &str) -> serde_json::Value {
        serde_json::json!({
            "worktree": abs("/wt"),
            "branch": "lev/fix",
            "issue": issue,
            "slug": "fix",
            "apps": Vec::<String>::new(),
            "prefix": "lev/",
            "worktree_root": abs("/wts"),
            "primary": abs("/repo"),
        })
    }

    #[test]
    fn a_relative_destination_is_refused() {
        let err = resolve_entry(
            "notes",
            &cfg(&["a.md"], "archive/{{ issue }}", false),
            &ctx_for("ENG-1"),
            &novars(),
            &[],
        )
        .unwrap_err();
        assert_eq!(err.name, "notes");
        assert!(err.reason.contains("absolute"), "{}", err.reason);
    }

    /// An empty `to` addresses the process cwd, which would write the archive
    /// into whatever directory the command was run from.
    #[test]
    fn an_empty_destination_is_refused() {
        let err = resolve_entry(
            "notes",
            &cfg(&["a.md"], "{{ issue }}", false),
            &ctx_for(""),
            &novars(),
            &[],
        )
        .unwrap_err();
        assert!(err.reason.contains("empty"), "{}", err.reason);
    }

    /// Archiving into a tree this run deletes loses the copy moments later.
    #[test]
    fn a_destination_inside_a_worktree_being_removed_is_refused() {
        let roots = vec![PathBuf::from(abs("/wts/fix"))];
        let dest = abs("/wts/fix/archive");
        let err = resolve_entry(
            "notes",
            &cfg(&["a.md"], &dest, false),
            &ctx_for("ENG-1"),
            &novars(),
            &roots,
        )
        .unwrap_err();
        assert!(err.reason.contains("removes"), "{}", err.reason);

        // The check compares whole path components, so a sibling that merely
        // shares a name prefix is a different directory and is accepted.
        let sibling = abs("/wts/fixtures/archive");
        let resolved = resolve_entry(
            "notes",
            &cfg(&["a.md"], &sibling, false),
            &ctx_for("ENG-1"),
            &novars(),
            &roots,
        )
        .expect("a sibling sharing a name prefix is not inside the removal root");
        assert_eq!(resolved.dest, PathBuf::from(sibling));
    }

    /// The containment check compares normalized paths, so a `to` that walks
    /// back into a removal root through `..` is still refused. `issue` and
    /// `slug` come from the worktree's own record, so the `..` need not be
    /// author-written to reach the template.
    #[test]
    fn a_destination_reaching_a_removal_root_through_dotdot_is_refused() {
        let roots = vec![PathBuf::from(abs("/wts/fix"))];
        let dest = abs("/wts/other/../../wts/fix/archive");
        let err = resolve_entry(
            "notes",
            &cfg(&["a.md"], &dest, false),
            &ctx_for("ENG-1"),
            &novars(),
            &roots,
        )
        .unwrap_err();
        assert!(err.reason.contains("removes"), "{}", err.reason);
    }

    /// A removal root spelled with `..` names the same directory as its
    /// normalized form, and must gate the same destinations.
    #[test]
    fn a_removal_root_spelled_with_dotdot_still_gates() {
        let roots = vec![PathBuf::from(abs("/wts/other/../fix"))];
        let dest = abs("/wts/fix/archive");
        let err = resolve_entry(
            "notes",
            &cfg(&["a.md"], &dest, false),
            &ctx_for("ENG-1"),
            &novars(),
            &roots,
        )
        .unwrap_err();
        assert!(err.reason.contains("removes"), "{}", err.reason);
    }

    /// NTFS is case-insensitive, so a destination differing from the removal
    /// root only in case names a directory this run deletes. Windows-only: on
    /// ext4 the two really are different directories and the entry is fine.
    /// (macOS is case-insensitive by default too, but a case-sensitive volume
    /// is a supported configuration there, so the assertion would not hold.)
    #[test]
    #[cfg(windows)]
    fn a_destination_differing_from_a_removal_root_only_in_case_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt-eng-1");
        std::fs::create_dir_all(&wt).unwrap();

        let mixed = dir.path().join("WT-ENG-1").join("archive");
        let err = resolve_entry(
            "notes",
            &cfg(&["a.md"], mixed.to_str().unwrap(), false),
            &ctx_for("ENG-1"),
            &novars(),
            &[wt],
        )
        .unwrap_err();
        assert!(err.reason.contains("removes"), "{}", err.reason);
    }

    /// The containment check resolves symlinks, so a destination that reaches a
    /// removal root through one is refused as well. A lexical compare sees two
    /// unrelated paths and lets the archive land in the tree being deleted.
    #[test]
    #[cfg(unix)]
    fn a_destination_reaching_a_removal_root_through_a_symlink_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt-eng-1");
        std::fs::create_dir_all(&wt).unwrap();
        let link = dir.path().join("shortcut");
        std::os::unix::fs::symlink(&wt, &link).unwrap();

        let dest = link.join("archive");
        let err = resolve_entry(
            "notes",
            &cfg(&["a.md"], dest.to_str().unwrap(), false),
            &ctx_for("ENG-1"),
            &novars(),
            &[wt],
        )
        .unwrap_err();
        assert!(err.reason.contains("removes"), "{}", err.reason);
    }

    /// A destination whose parents do not exist yet is the normal case — the
    /// archive directory is created when the first file lands. Resolving the
    /// deepest ancestor that does exist still places it.
    #[test]
    fn a_destination_that_does_not_exist_yet_is_still_placed() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt-eng-1");
        std::fs::create_dir_all(&wt).unwrap();

        let inside = wt.join("deep/archive/ENG-1");
        let err = resolve_entry(
            "notes",
            &cfg(&["a.md"], inside.to_str().unwrap(), false),
            &ctx_for("ENG-1"),
            &novars(),
            std::slice::from_ref(&wt),
        )
        .unwrap_err();
        assert!(err.reason.contains("removes"), "{}", err.reason);

        let outside = dir.path().join("archive/ENG-1");
        resolve_entry(
            "notes",
            &cfg(&["a.md"], outside.to_str().unwrap(), false),
            &ctx_for("ENG-1"),
            &novars(),
            std::slice::from_ref(&wt),
        )
        .expect("a sibling of the removal root is not inside it");
    }

    /// A pattern that renders empty drops out rather than reaching `copy_out`,
    /// where an empty glob would otherwise have to be caught again.
    #[test]
    fn a_pattern_that_renders_empty_drops_out() {
        let dest = abs("/archive");
        let resolved = resolve_entry(
            "notes",
            &cfg(&["{{ issue }}", "keep.md"], &dest, false),
            &ctx_for(""),
            &novars(),
            &[],
        )
        .unwrap();
        assert_eq!(resolved.patterns, vec!["keep.md".to_string()]);
    }

    #[test]
    fn a_resolved_entry_renders_both_fields() {
        let resolved = resolve_entry(
            "notes",
            &cfg(
                &["out/{{ slug }}/**"],
                "{{ worktree_root }}/archive/{{ issue }}",
                false,
            ),
            &ctx_for("ENG-7"),
            &novars(),
            &[],
        )
        .unwrap();
        assert_eq!(resolved.name, "notes");
        assert_eq!(resolved.patterns, vec!["out/fix/**".to_string()]);
        assert_eq!(
            resolved.dest,
            PathBuf::from(format!("{}/archive/ENG-7", abs("/wts")))
        );
    }

    /// A malformed record reads as `None`, exactly like an absent one, so both
    /// take the same defaults rather than failing the render.
    #[test]
    fn a_missing_record_renders_the_issue_fields_empty() {
        let ctx = context(
            Path::new("/wt"),
            "lev/fix",
            None,
            "lev/",
            Path::new("/wts"),
            Some(Path::new("/repo")),
        );
        assert_eq!(ctx["issue"], "");
        assert_eq!(ctx["slug"], "");
        assert_eq!(ctx["apps"], serde_json::json!([]));
        assert_eq!(ctx["branch"], "lev/fix");
    }

    /// An unresolvable primary checkout leaves `primary` out of the context, so
    /// an entry that uses it fails naming that variable. Substituting the
    /// worktree instead would archive into the directory being removed and
    /// blame the destination for it.
    #[test]
    fn an_unresolvable_primary_fails_the_entry_that_uses_it() {
        let ctx = context(
            Path::new("/wt"),
            "lev/fix",
            None,
            "lev/",
            Path::new("/wts"),
            None,
        );
        assert!(ctx.get("primary").is_none());

        let err = resolve_entry(
            "notes",
            &cfg(&["a.md"], "{{ primary }}/../archive", false),
            &ctx,
            &novars(),
            &[],
        )
        .unwrap_err();
        assert!(err.reason.contains("primary"), "{}", err.reason);

        // Entries that never mention it are unaffected.
        let dest = abs("/archive");
        resolve_entry("notes", &cfg(&["a.md"], &dest, false), &ctx, &novars(), &[])
            .expect("an entry that does not use `primary` still resolves");
    }

    /// Fail-open is the default: a bad entry warns and the caller still removes
    /// the worktree.
    #[test]
    fn a_failing_entry_warns_without_blocking_removal() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();

        let bad = cfg(&["a.md"], "relative/path", false);
        let out = run_for(
            &wt,
            &[("notes".to_string(), &bad)],
            &ctx_for("ENG-1"),
            &novars(),
            &[],
        );

        assert!(out.required_failure.is_none(), "removal is not blocked");
        assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
        assert!(out.warnings[0].contains("notes"), "{:?}", out.warnings);
    }

    /// `required` turns the same warning into the reason the worktree survives.
    #[test]
    fn a_failing_required_entry_blocks_removal() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();

        let bad = cfg(&["a.md"], "relative/path", true);
        let out = run_for(
            &wt,
            &[("notes".to_string(), &bad)],
            &ctx_for("ENG-1"),
            &novars(),
            &[],
        );

        let err = out.required_failure.expect("required entry blocks");
        assert!(err.contains("notes"), "{err}");
    }

    /// The files a blocked entry already copied are on disk, so the summary
    /// counts them: only the worktree is kept, nothing is undone.
    #[test]
    fn a_blocked_required_entry_still_reports_what_it_copied() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        let archive = dir.path().join("archive");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join("keep.md"), "keep").unwrap();

        let entry = cfg(
            &["keep.md", "../outside.md"],
            archive.to_str().unwrap(),
            true,
        );
        let out = run_for(
            &wt,
            &[("notes".to_string(), &entry)],
            &ctx_for("ENG-1"),
            &novars(),
            &[],
        );

        assert!(out.required_failure.is_some(), "the escaping pattern warns");
        assert_eq!(out.files, 1);
        assert_eq!(out.archived, ["notes".to_string()].into());
        assert!(archive.join("keep.md").exists());
    }

    /// `required` governs errors, never emptiness. A worktree that produced no
    /// scratch still removes cleanly.
    #[test]
    fn a_required_entry_matching_nothing_does_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        let archive = dir.path().join("archive");
        std::fs::create_dir_all(&wt).unwrap();

        let entry = cfg(&["scratch/"], archive.to_str().unwrap(), true);
        let out = run_for(
            &wt,
            &[("notes".to_string(), &entry)],
            &ctx_for("ENG-1"),
            &novars(),
            &[],
        );

        assert!(out.required_failure.is_none());
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        assert_eq!(out.files, 0);
    }

    /// A directory pattern is the spelling that archives a whole tree: the
    /// match is the directory itself, which `plan_includes` walks recursively.
    /// A trailing `**` matches only the subdirectories under it, so it would
    /// archive nested files and silently drop the ones at the top.
    #[test]
    fn a_matching_entry_reports_what_it_archived() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        let archive = dir.path().join("archive");
        std::fs::create_dir_all(wt.join("scratch")).unwrap();
        std::fs::write(wt.join("scratch/a.md"), "a").unwrap();

        let entry = cfg(&["scratch/"], archive.to_str().unwrap(), false);
        let out = run_for(
            &wt,
            &[("notes".to_string(), &entry)],
            &ctx_for("ENG-1"),
            &novars(),
            &[],
        );

        assert_eq!(out.files, 1, "{:?}", out.warnings);
        assert_eq!(out.archived, ["notes".to_string()].into());
        assert!(archive.join("scratch/a.md").exists());
    }

    fn config_with(entries: &[(&str, PreserveConfig)]) -> devkit_config::Config {
        let mut config = devkit_config::Config::default();
        for (name, entry) in entries {
            config.preserve.insert(
                (*name).to_string(),
                PreserveConfig {
                    from: entry.from.clone(),
                    to: entry.to.clone(),
                    required: entry.required,
                },
            );
        }
        config
    }

    /// `--no-preserve` waives preservation whatever the config asks for.
    #[test]
    fn no_preserve_selects_nothing_from_a_populated_config() {
        let config = config_with(&[("notes", cfg(&["a.md"], &abs("/archive"), false))]);
        assert!(preserve_entries(Some(&config), true).is_empty());
    }

    #[test]
    fn an_empty_preserve_table_selects_nothing() {
        let config = config_with(&[]);
        assert!(preserve_entries(Some(&config), false).is_empty());
        assert!(preserve_entries(None, false).is_empty());
    }

    /// Entries run in sorted key order, so two entries sharing a destination
    /// resolve in an order that does not depend on hash iteration.
    #[test]
    fn entries_come_back_in_sorted_key_order() {
        let dest = abs("/archive");
        let config = config_with(&[
            ("zeta", cfg(&["z.md"], &dest, false)),
            ("alpha", cfg(&["a.md"], &dest, false)),
            ("mid", cfg(&["m.md"], &dest, false)),
        ]);

        let names: Vec<String> = preserve_entries(Some(&config), false)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names, vec!["alpha", "mid", "zeta"]);
    }
}
