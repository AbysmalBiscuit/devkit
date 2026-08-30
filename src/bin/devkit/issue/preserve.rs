//! Copying a worktree's files out before `issue end` removes it. Resolution and
//! validation live here; the copy itself is `devkit_common::worktree::copy_out`.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "no non-test caller until issue end copies preserve entries out"
    )
)]

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
}
