use anyhow::{Context, Result};
use devkit_common::cmd::{capture, git};
use devkit_ports::config::expand_tilde;
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
        let out = Prepared { issue: args.issue.clone(), worktree: holder, branch, ports };
        println!("{}", serde_json::to_string_pretty(&out)?);
        eprintln!("(dry-run: no worktree created, no ports reserved)");
        return Ok(());
    }

    anyhow::ensure!(!worktree.exists(), "worktree path already exists: {}", worktree.display());
    let monorepo_s = monorepo.to_str().context("monorepo path not UTF-8")?;
    git(&["fetch", "origin"], monorepo_s)?;
    if git(&["rev-parse", "--verify", &format!("refs/heads/{branch}")], monorepo_s).is_ok() {
        anyhow::bail!("branch {branch} already exists — let /issue-setup decide how to proceed");
    }
    git(
        &["worktree", "add", "-b", &branch, worktree.to_str().unwrap(), &cfg.defaults.baseline_ref],
        monorepo_s,
    )?;

    // env symlinks (skip if present); apps with configured prep_env get a .env.local.
    let env_local = wt_root.join(".env.local");
    for a in &args.apps {
        let app = &catalog[a];
        let app_dir = worktree.join(&app.path);
        std::fs::create_dir_all(&app_dir).ok();
        let dotenv = app_dir.join(".env");
        if !dotenv.exists() {
            std::os::unix::fs::symlink(&env_local, &dotenv)
                .with_context(|| format!("symlinking {} -> {}", dotenv.display(), env_local.display()))?;
        }
        if !app.prep_env.is_empty() {
            let f = app_dir.join(".env.local");
            if !f.exists() {
                let body: String = app.prep_env.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
                std::fs::write(&f, body)?;
            }
        }
    }

    // bun install once, in the first app's dir.
    if let Some(first) = args.apps.first() {
        let app_dir = worktree.join(&catalog[first].path);
        capture("bun", &["install"], Some(app_dir.to_str().unwrap()))
            .with_context(|| "running `bun install`")?;
    }

    // reserve ports
    let reqs: Vec<(String, u16)> =
        args.apps.iter().map(|a| (a.clone(), catalog[a].base_port)).collect();
    let ports: BTreeMap<String, u16> =
        registry::alloc(&holder, &reqs, Role::Issue)?.into_iter().collect();

    let out = Prepared { issue: args.issue.clone(), worktree: holder, branch, ports };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn branch_uses_prefix_and_slug() {
        assert_eq!(branch_name("lev/", "eng-1234-fix"), "lev/eng-1234-fix");
    }
}
