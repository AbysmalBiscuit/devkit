//! Copying a worktree's files out before `issue end` removes it. Resolution and
//! validation live here; the copy itself is `devkit_common::worktree::copy_out`.

use devkit_common::record::IssueRecord;
use devkit_config::PreserveConfig;
use std::collections::BTreeMap;
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
pub(crate) fn context(
    worktree: &Path,
    branch: &str,
    record: Option<&IssueRecord>,
    prefix: &str,
    worktree_root: &Path,
    primary: &Path,
) -> serde_json::Value {
    serde_json::json!({
        "worktree": worktree.display().to_string(),
        "branch": branch,
        "issue": record.map(|r| r.issue.as_str()).unwrap_or_default(),
        "slug": record.map(|r| r.slug.as_str()).unwrap_or_default(),
        "apps": record.map(|r| r.apps.clone()).unwrap_or_default(),
        "prefix": prefix,
        "worktree_root": worktree_root.display().to_string(),
        "primary": primary.display().to_string(),
    })
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
            Err(e) => return Err(skip(format!("rendering `from` entry `{p}`: {e:#}"))),
        }
    }

    let rendered = devkit_common::template::render(&cfg.to, ctx, vars)
        .map_err(|e| skip(format!("rendering `to`: {e:#}")))?;
    let to = rendered.trim();
    if to.is_empty() {
        return Err(skip("`to` rendered empty".into()));
    }
    let dest = PathBuf::from(to);
    if !dest.is_absolute() {
        return Err(skip(format!("`to` must be an absolute path, got `{to}`")));
    }
    if let Some(root) = removal_roots.iter().find(|r| dest.starts_with(r)) {
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
    pub entries: usize,
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
        entries: 0,
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
                out.files += files;
                if files > 0 {
                    out.entries += 1;
                }
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
            "graphify",
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
        assert_eq!(resolved.name, "graphify");
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
            Path::new("/repo"),
        );
        assert_eq!(ctx["issue"], "");
        assert_eq!(ctx["slug"], "");
        assert_eq!(ctx["apps"], serde_json::json!([]));
        assert_eq!(ctx["branch"], "lev/fix");
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

    /// `required` governs errors, never emptiness. A worktree that produced no
    /// scratch still removes cleanly.
    #[test]
    fn a_required_entry_matching_nothing_does_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        let archive = dir.path().join("archive");
        std::fs::create_dir_all(&wt).unwrap();

        let entry = cfg(&["graphify-out/**"], archive.to_str().unwrap(), true);
        let out = run_for(
            &wt,
            &[("graphify".to_string(), &entry)],
            &ctx_for("ENG-1"),
            &novars(),
            &[],
        );

        assert!(out.required_failure.is_none());
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        assert_eq!(out.files, 0);
    }

    /// A trailing `**` is glob 0.3's "directories under here" and matches no
    /// files, so the pattern that actually archives a tree is `**/*`.
    #[test]
    fn a_matching_entry_reports_what_it_archived() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        let archive = dir.path().join("archive");
        std::fs::create_dir_all(wt.join("graphify-out")).unwrap();
        std::fs::write(wt.join("graphify-out/a.md"), "a").unwrap();

        let entry = cfg(&["graphify-out/**/*"], archive.to_str().unwrap(), false);
        let out = run_for(
            &wt,
            &[("graphify".to_string(), &entry)],
            &ctx_for("ENG-1"),
            &novars(),
            &[],
        );

        assert_eq!(out.files, 1, "{:?}", out.warnings);
        assert_eq!(out.entries, 1);
        assert!(archive.join("graphify-out/a.md").exists());
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
