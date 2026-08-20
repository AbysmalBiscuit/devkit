//! The issue summary file `issue setup --summary` leaves behind: the Linear
//! facts and description as a scaffold, with the sections an agent fills in.

use anyhow::{Context, Result};
use devkit_common::linear::IssueDetails;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Render context for both `issue_summary_path` and `issue_summary`. Every
/// Linear field Linear left empty renders as the empty string, so a template
/// can branch on it with a plain `{% if %}`.
fn context(
    d: &IssueDetails,
    worktree: &str,
    branch: &str,
    slug: &str,
    prefix: &str,
    apps: &[String],
) -> serde_json::Value {
    serde_json::json!({
        "issue": d.identifier,
        "title": d.title,
        "url": d.url,
        "description": d.description,
        "state": d.state.clone().unwrap_or_default(),
        "assignee": d.assignee.clone().unwrap_or_default(),
        "priority": d.priority.clone().unwrap_or_default(),
        "estimate": d.estimate.clone().unwrap_or_default(),
        "labels": d.labels,
        "parent": d.parent.clone().unwrap_or_default(),
        "project": d.project.clone().unwrap_or_default(),
        "worktree": worktree,
        "branch": branch,
        "slug": slug,
        "prefix": prefix,
        "apps": apps,
    })
}

/// Where the summary file goes: `template` rendered, then taken from
/// `worktree_root` unless it came out absolute. Rendering `{{ worktree }}` into
/// the template is what puts the file inside the worktree instead of beside it.
fn resolve_path(
    template: &str,
    worktree_root: &Path,
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
) -> Result<PathBuf> {
    let rendered = devkit_common::template::render(template, ctx, vars)
        .context("rendering `issue_summary_path` template")?;
    let rendered = rendered.trim();
    anyhow::ensure!(
        !rendered.is_empty(),
        "`issue_summary_path` rendered to an empty path"
    );
    let p = Path::new(rendered);
    Ok(if p.is_absolute() {
        p.to_path_buf()
    } else {
        worktree_root.join(p)
    })
}

/// Write `body` to `path`, creating parent directories. Returns whether it was
/// written: an existing summary is left byte-for-byte, since by the second
/// `issue setup` it holds investigation the scaffold cannot reproduce.
fn write_if_absent(path: &Path, body: &str) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

/// The path `issue setup --summary` will write, without writing it — what
/// `--dry-run` reports.
pub(crate) fn plan_path(
    cfg: &devkit_ports::config::Config,
    d: &IssueDetails,
    worktree_root: &Path,
    worktree: &str,
    branch: &str,
    slug: &str,
    apps: &[String],
) -> Result<PathBuf> {
    let vars = &cfg.templates.variables;
    let ctx = context(d, worktree, branch, slug, &cfg.defaults.branch_prefix, apps);
    resolve_path(
        cfg.templates.issue_summary_path(),
        worktree_root,
        &ctx,
        vars,
    )
}

/// Render the summary and write it if nothing is there yet. Returns the path
/// and whether this run created it.
pub(crate) fn write(
    cfg: &devkit_ports::config::Config,
    d: &IssueDetails,
    worktree_root: &Path,
    worktree: &str,
    branch: &str,
    slug: &str,
    apps: &[String],
) -> Result<(PathBuf, bool)> {
    let vars = &cfg.templates.variables;
    let ctx = context(d, worktree, branch, slug, &cfg.defaults.branch_prefix, apps);
    let path = resolve_path(
        cfg.templates.issue_summary_path(),
        worktree_root,
        &ctx,
        vars,
    )?;
    let body = devkit_common::template::render(cfg.templates.issue_summary(), &ctx, vars)
        .context("rendering `issue_summary` template")?;
    let written = write_if_absent(&path, &body)?;
    Ok((path, written))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devkit-sum-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn details() -> devkit_common::linear::IssueDetails {
        devkit_common::linear::IssueDetails {
            identifier: "ENG-42".into(),
            title: "Fix the login redirect".into(),
            url: "https://linear.app/acme/issue/ENG-42/fix-the-login-redirect".into(),
            description: "Clicking sign-in bounces to /.".into(),
            state: Some("Todo".into()),
            assignee: Some("Lev".into()),
            priority: Some("High".into()),
            estimate: Some("3".into()),
            labels: vec!["auth".into(), "web".into()],
            parent: Some("ENG-1 \u{2014} Login epic".into()),
            project: Some("Q3 hardening".into()),
        }
    }

    fn ctx() -> serde_json::Value {
        context(
            &details(),
            "/w/eng-42",
            "lev/eng-42-fix",
            "eng-42-fix",
            "lev/",
            &["api".into()],
        )
    }

    #[test]
    fn relative_path_lands_beside_the_worktree_not_inside_it() {
        let p = resolve_path(
            "ISSUE_SUMMARY_{{ issue }}.md",
            Path::new("/w"),
            &ctx(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(p, PathBuf::from("/w/ISSUE_SUMMARY_ENG-42.md"));
    }

    #[test]
    fn a_worktree_rooted_template_keeps_the_file_inside() {
        let p = resolve_path(
            "{{ worktree }}/ISSUE.md",
            Path::new("/w"),
            &ctx(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(p, PathBuf::from("/w/eng-42/ISSUE.md"));
    }

    #[test]
    fn default_template_carries_the_facts_and_the_empty_sections() {
        let out = devkit_common::template::render(
            devkit_ports::config::DEFAULT_ISSUE_SUMMARY,
            &ctx(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(out.starts_with("# ENG-42: Fix the login redirect\n"));
        assert!(out.contains("- **Parent:** ENG-1 \u{2014} Login epic\n"));
        assert!(out.contains("- **Project:** Q3 hardening\n"));
        assert!(out.contains("- **Branch:** lev/eng-42-fix\n"));
        assert!(out.contains("- **Apps in scope:** api\n"));
        assert!(out.contains("- **State / assignee:** Todo / Lev\n"));
        assert!(out.contains("- **Priority / estimate:** High / 3\n"));
        assert!(out.contains("- **Labels:** auth, web\n"));
        assert!(out.contains("## Linear description\n\nClicking sign-in bounces to /.\n"));
        assert!(out.trim_end().ends_with("## Summary\n\n## Pointers"));
    }

    #[test]
    fn absent_linear_fields_drop_their_lines_rather_than_render_empty() {
        let mut d = details();
        d.parent = None;
        d.project = None;
        d.estimate = None;
        d.labels.clear();
        d.assignee = None;
        let c = context(&d, "/w/eng-42", "b", "s", "lev/", &[]);
        let out = devkit_common::template::render(
            devkit_ports::config::DEFAULT_ISSUE_SUMMARY,
            &c,
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(!out.contains("**Parent:**"));
        assert!(!out.contains("**Project:**"));
        assert!(!out.contains("**Labels:**"));
        assert!(!out.contains("**Apps in scope:**"));
        assert!(out.contains("- **Priority:** High\n"));
        assert!(out.contains("- **State / assignee:** Todo / \n"));
    }

    #[test]
    fn write_creates_the_file_and_reports_it_written() {
        let dir = scratch("write");
        let p = dir.join("ISSUE_SUMMARY_ENG-42.md");
        assert!(write_if_absent(&p, "body\n").unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "body\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_existing_summary_is_never_overwritten() {
        let dir = scratch("keep");
        let p = dir.join("ISSUE_SUMMARY_ENG-42.md");
        std::fs::write(&p, "months of investigation\n").unwrap();
        assert!(!write_if_absent(&p, "fresh scaffold\n").unwrap());
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "months of investigation\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_creates_missing_parent_directories() {
        let dir = scratch("parents");
        let p = dir.join("nested/deeper/ISSUE.md");
        assert!(write_if_absent(&p, "body\n").unwrap());
        assert!(p.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
