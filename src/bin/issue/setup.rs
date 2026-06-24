use anyhow::{Context, Result};
use devkit_common::cmd::{capture, git};
use devkit_ports::config::{expand_tilde, PrepFile};
use devkit_ports::load;
use devkit_ports::registry::{self, Data, Role};
use std::collections::BTreeMap;
use std::path::Path;

pub struct SetupArgs {
    pub issue: String,
    pub slug: String,
    pub apps: Vec<String>,
    pub dry_run: bool,
    pub dir: Option<String>,
    pub config: Option<String>,
}

#[derive(serde::Serialize)]
struct Prepared {
    issue: String,
    worktree: String,
    branch: String,
    ports: BTreeMap<String, u16>,
}

fn branch_name(prefix: &str, slug: &str) -> String {
    format!("{prefix}{slug}")
}

/// Write each prep file into `app_dir`. Content is written verbatim; parent
/// directories are created; an existing file is left untouched unless the entry
/// opts into `overwrite`.
fn write_prep_files(app_dir: &Path, files: &[PrepFile]) -> Result<()> {
    for pf in files {
        let target = app_dir.join(&pf.path);
        if pf.overwrite || !target.exists() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating parent dir for prep file `{}`", pf.path))?;
            }
            std::fs::write(&target, &pf.content)
                .with_context(|| format!("writing prep file `{}`", pf.path))?;
        }
    }
    Ok(())
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
    let worktree = wt_root.join(&args.slug);
    let monorepo = wt_root.join("monorepo");
    let branch = branch_name(&cfg.defaults.branch_prefix, &args.slug);
    let holder = worktree.to_string_lossy().into_owned();

    if args.dry_run {
        // Compute would-be ports against a snapshot WITHOUT reserving them.
        let mut data: Data = registry::snapshot()?;
        let mut ports = BTreeMap::new();
        for a in &args.apps {
            let base = catalog[a].base_port;
            ports.insert(a.clone(), data.alloc_one(&holder, a, base, Role::Issue));
        }
        let out = Prepared {
            issue: args.issue.clone(),
            worktree: holder,
            branch,
            ports,
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
        eprintln!("(dry-run: no worktree created, no ports reserved)");
        return Ok(());
    }

    anyhow::ensure!(
        !worktree.exists(),
        "worktree path already exists: {}",
        worktree.display()
    );
    let monorepo_s = monorepo.to_str().context("monorepo path not UTF-8")?;
    git(&["fetch", "origin"], monorepo_s)?;
    if git(
        &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
        monorepo_s,
    )
    .is_ok()
    {
        anyhow::bail!("branch {branch} already exists — let /issue-setup decide how to proceed");
    }
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
    )?;

    // Per-app bootstrap: write the app's configured prep files, then run its
    // setup commands in its directory. Everything project-specific — filenames,
    // file contents, installs, doppler wiring — lives in config, not here.
    for a in &args.apps {
        let app = &catalog[a];
        let app_dir = worktree.join(&app.path);
        std::fs::create_dir_all(&app_dir).ok();

        write_prep_files(&app_dir, &app.prep_files)
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

    // reserve ports
    let reqs: Vec<(String, u16)> = args
        .apps
        .iter()
        .map(|a| (a.clone(), catalog[a].base_port))
        .collect();
    let ports: BTreeMap<String, u16> = registry::alloc(&holder, &reqs, Role::Issue)?
        .into_iter()
        .collect();

    let out = Prepared {
        issue: args.issue.clone(),
        worktree: holder,
        branch,
        ports,
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        // Unique per process + tag; no tempfile dependency.
        let dir = std::env::temp_dir().join(format!("devkit-prep-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn branch_uses_prefix_and_slug() {
        assert_eq!(branch_name("lev/", "eng-1234-fix"), "lev/eng-1234-fix");
    }

    #[test]
    fn writes_content_verbatim_and_creates_parents() {
        let dir = scratch("verbatim");
        let files = vec![PrepFile {
            path: "config/local.json".into(),
            content: "{\"mode\":\"local\"}\n".into(),
            overwrite: false,
        }];
        write_prep_files(&dir, &files).unwrap();
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
        write_prep_files(&dir, &files).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(".env.local")).unwrap(), "ORIGINAL\n");
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
        write_prep_files(&dir, &files).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(".env.local")).unwrap(), "REPLACED\n");
        std::fs::remove_dir_all(&dir).ok();
    }
}
