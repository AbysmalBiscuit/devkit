use anyhow::{Context, Result};
use devkit_common::cmd::{capture, git};
use devkit_common::progress::Steps;
use devkit_ports::config::{PrepFile, expand_tilde};
use devkit_ports::load;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

pub struct SetupArgs {
    pub issue: String,
    pub slug: String,
    pub apps: Vec<String>,
    pub dry_run: bool,
    pub no_gitignore: bool,
    pub dir: Option<String>,
    pub config: Option<String>,
}

#[derive(serde::Serialize)]
struct Prepared {
    issue: String,
    worktree: String,
    branch: String,
}

/// Write each prep file into `app_dir`. `content` is rendered as a minijinja
/// template against `ctx`/`vars` (strict undefined) before writing; parent
/// directories are created; an existing file is left untouched unless the entry
/// opts into `overwrite`. Only files that will be written are rendered.
fn write_prep_files(
    app_dir: &Path,
    files: &[PrepFile],
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
) -> Result<()> {
    for pf in files {
        let target = app_dir.join(&pf.path);
        if pf.overwrite || !target.exists() {
            let rendered = devkit_common::template::render(&pf.content, ctx, vars)
                .with_context(|| format!("rendering prep file `{}`", pf.path))?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating parent dir for prep file `{}`", pf.path))?;
            }
            std::fs::write(&target, &rendered)
                .with_context(|| format!("writing prep file `{}`", pf.path))?;
        }
    }
    Ok(())
}

/// Per-app bootstrap shared by `setup` and `checkout-pr --setup`: write each
/// app's prep files (rendered against `base_ctx` plus `app`/`branch`/`worktree`),
/// then run its setup commands in its directory.
pub(crate) fn prep_apps(
    worktree: &Path,
    branch: &str,
    apps: &[String],
    catalog: &HashMap<String, devkit_ports::apps::App>,
    base_ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
) -> Result<()> {
    for a in apps {
        let app = &catalog[a];
        let app_dir = worktree.join(&app.path);
        std::fs::create_dir_all(&app_dir).ok();

        let mut file_ctx = base_ctx.clone();
        if let Some(obj) = file_ctx.as_object_mut() {
            obj.insert("app".into(), serde_json::Value::String(a.clone()));
            obj.insert(
                "branch".into(),
                serde_json::Value::String(branch.to_string()),
            );
            obj.insert(
                "worktree".into(),
                serde_json::Value::String(worktree.to_string_lossy().into_owned()),
            );
        }
        write_prep_files(&app_dir, &app.prep_files, &file_ctx, vars)
            .with_context(|| format!("preparing files for app `{a}`"))?;

        for cmd in &app.setup {
            let (prog, rest) = cmd.split_first().context("empty setup command")?;
            capture(
                prog,
                &rest.iter().map(String::as_str).collect::<Vec<_>>(),
                app_dir.to_str(),
            )
            .with_context(|| format!("running setup `{}` for app `{a}`", cmd.join(" ")))?;
        }
    }
    Ok(())
}

/// Copy the configured `worktree_include` globs from the monorepo into a freshly
/// created worktree, printing each fail-open warning to stderr. A no-op when the
/// include list is empty.
pub fn backfill_includes(monorepo: &str, worktree: &std::path::Path, patterns: &[String]) {
    if patterns.is_empty() {
        return;
    }
    let (_copied, warnings) =
        devkit_common::worktree::copy_includes(std::path::Path::new(monorepo), worktree, patterns);
    for w in warnings {
        eprintln!("warning: {w}");
    }
}

pub fn run(args: SetupArgs) -> Result<()> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());
    let loaded = load::load(args.config.as_deref().map(Path::new), Path::new(&start))?;
    let cfg = &loaded.config;
    let catalog = &loaded.catalog;

    for a in &args.apps {
        anyhow::ensure!(catalog.contains_key(a), "unknown app `{a}`");
    }

    let wt_root = expand_tilde(&cfg.defaults.worktree_root);
    let ctx = serde_json::json!({
        "prefix": cfg.defaults.branch_prefix,
        "issue": args.issue,
        "slug": args.slug,
        "apps": args.apps,
    });
    let vars = &cfg.templates.variables;
    let branch = devkit_common::template::render(cfg.templates.branch(), &ctx, vars)
        .context("rendering `branch` template")?
        .trim()
        .to_string();
    let wt_name = devkit_common::template::render(cfg.templates.worktree_dir(), &ctx, vars)
        .context("rendering `worktree_dir` template")?
        .trim()
        .to_string();
    let worktree = wt_root.join(&wt_name);
    let monorepo = wt_root.join("monorepo");
    let holder = worktree.to_string_lossy().into_owned();

    if args.dry_run {
        let out = Prepared {
            issue: args.issue.clone(),
            worktree: holder,
            branch,
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
        eprintln!("(dry-run: no worktree created)");
        return Ok(());
    }

    anyhow::ensure!(
        !worktree.exists(),
        "worktree path already exists: {}",
        worktree.display()
    );
    let monorepo_s = monorepo.to_str().context("monorepo path not UTF-8")?;
    let total = 2 + usize::from(!args.apps.is_empty());
    let steps = Steps::with_total(total);
    steps.during("Fetching from origin…", || {
        git(&["fetch", "origin"], monorepo_s)
    })?;
    if git(
        &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
        monorepo_s,
    )
    .is_ok()
    {
        anyhow::bail!("branch {branch} already exists — let /issue-setup decide how to proceed");
    }
    steps.during("Creating worktree…", || {
        git(
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                worktree.to_str().unwrap(),
                &cfg.defaults.baseline_ref,
            ],
            monorepo_s,
        )
    })?;

    crate::record::write(
        &worktree,
        &crate::record::IssueRecord {
            issue: args.issue.clone(),
            slug: args.slug.clone(),
            apps: args.apps.clone(),
        },
    )?;
    if !args.no_gitignore
        && let Err(e) = crate::gitignore::ensure_devkit_ignored()
    {
        eprintln!("warning: could not update global gitignore: {e:#}");
    }

    backfill_includes(monorepo_s, &worktree, &cfg.defaults.worktree_include);

    // Per-app bootstrap: write the app's configured prep files, then run its
    // setup commands in its directory. Everything project-specific — filenames,
    // file contents, installs, doppler wiring — lives in config, not here.
    if args.apps.is_empty() {
        prep_apps(&worktree, &branch, &args.apps, catalog, &ctx, vars)?;
    } else {
        steps.during("Preparing apps…", || {
            prep_apps(&worktree, &branch, &args.apps, catalog, &ctx, vars)
        })?;
    }

    // Ports are not reserved here. A worktree's servers get their ports
    // dynamically from `devrun up`, which allocates against the live registry at
    // start time — so the numbers always reflect what is actually free and no
    // unused reservation can be reclaimed by another session in the meantime.
    let out = Prepared {
        issue: args.issue.clone(),
        worktree: holder,
        branch,
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use devkit_ports::config::Templates;
    use serde_json::json;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        // Unique per process + tag; no tempfile dependency.
        let dir = std::env::temp_dir().join(format!("devkit-prep-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn novars() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    fn ctx() -> serde_json::Value {
        json!({"prefix": "lev/", "issue": "eng-1", "slug": "fix", "apps": ["web"], "app": "web"})
    }

    #[test]
    fn renders_issue_context() {
        let dir = scratch("render");
        let files = vec![PrepFile {
            path: ".env.local".into(),
            content: "ISSUE={{ issue }}\n".into(),
            overwrite: false,
        }];
        write_prep_files(&dir, &files, &ctx(), &novars()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(".env.local")).unwrap(),
            "ISSUE=eng-1\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn default_branch_renders_prefix_and_slug() {
        let t = Templates::default();
        let ctx = json!({"prefix": "lev/", "issue": "eng-1", "slug": "fix"});
        let out = devkit_common::template::render(t.branch(), &ctx, &t.variables).unwrap();
        assert_eq!(out, "lev/fix");
    }

    #[test]
    fn default_worktree_dir_renders_slug() {
        let t = Templates::default();
        let ctx = json!({"prefix": "lev/", "issue": "eng-1", "slug": "fix"});
        let out = devkit_common::template::render(t.worktree_dir(), &ctx, &t.variables).unwrap();
        assert_eq!(out, "fix");
    }

    #[test]
    fn writes_content_verbatim_and_creates_parents() {
        let dir = scratch("verbatim");
        let files = vec![PrepFile {
            path: "config/local.json".into(),
            content: "{\"mode\":\"local\"}\n".into(),
            overwrite: false,
        }];
        write_prep_files(&dir, &files, &ctx(), &novars()).unwrap();
        let got = std::fs::read_to_string(dir.join("config/local.json")).unwrap();
        assert_eq!(got, "{\"mode\":\"local\"}\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_if_absent_preserves_existing() {
        let dir = scratch("absent");
        std::fs::write(dir.join(".env.local"), "ORIGINAL\n").unwrap();
        let files = vec![PrepFile {
            path: ".env.local".into(),
            content: "REPLACED\n".into(),
            overwrite: false,
        }];
        write_prep_files(&dir, &files, &ctx(), &novars()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(".env.local")).unwrap(),
            "ORIGINAL\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn overwrite_replaces_existing() {
        let dir = scratch("overwrite");
        std::fs::write(dir.join(".env.local"), "ORIGINAL\n").unwrap();
        let files = vec![PrepFile {
            path: ".env.local".into(),
            content: "REPLACED\n".into(),
            overwrite: true,
        }];
        write_prep_files(&dir, &files, &ctx(), &novars()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(".env.local")).unwrap(),
            "REPLACED\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn renders_app_name() {
        let dir = scratch("appvar");
        let files = vec![PrepFile {
            path: "app.txt".into(),
            content: "{{ app }}".into(),
            overwrite: false,
        }];
        write_prep_files(&dir, &files, &ctx(), &novars()).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("app.txt")).unwrap(), "web");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_var_is_an_error() {
        let dir = scratch("badvar");
        let files = vec![PrepFile {
            path: ".env.local".into(),
            content: "{{ nope }}".into(),
            overwrite: false,
        }];
        assert!(write_prep_files(&dir, &files, &ctx(), &novars()).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn skipped_existing_file_is_not_rendered() {
        let dir = scratch("skiprender");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".env.local"), "ORIGINAL\n").unwrap();
        // A malformed template on an existing, non-overwrite file must not be
        // rendered (and so must not error) — the file is left untouched.
        let files = vec![PrepFile {
            path: ".env.local".into(),
            content: "{{ nope }}".into(),
            overwrite: false,
        }];
        write_prep_files(&dir, &files, &ctx(), &novars()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(".env.local")).unwrap(),
            "ORIGINAL\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
