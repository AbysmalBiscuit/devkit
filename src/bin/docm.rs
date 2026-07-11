use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use devkit_docs::manifest::{self, Discovered, Ecosystem, LibEntry};
use devkit_docs::{cache, lockfiles, lookup, refs, resolve};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "docm",
    about = "Version-correct local library docs and source checkouts"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Register a library: a package name (looked up on crates.io/npm/PyPI) or a git URL.
    Add {
        target: String,
        /// Ecosystem; omitted → probe crates.io, npm, PyPI in order.
        #[arg(long)]
        eco: Option<Ecosystem>,
        /// Registry package name when it differs from the lib name (e.g. @types/node).
        #[arg(long)]
        package: Option<String>,
        /// Repo URL override (skips the registry lookup).
        #[arg(long)]
        repo: Option<String>,
        /// Pin a git ref (tag/branch/sha) instead of lockfile resolution.
        #[arg(long = "ref")]
        git_ref: Option<String>,
        /// Layout override: source directory inside the checkout.
        #[arg(long)]
        src_dir: Option<String>,
        /// Layout override: docs directory inside the checkout.
        #[arg(long)]
        docs_dir: Option<String>,
        /// Freeform notes surfaced by `docm info`.
        #[arg(long)]
        notes: Option<String>,
        /// Write to the nearest devkit.toml [docs] section instead of the global manifest.
        #[arg(long)]
        project: bool,
    },
    /// Remove a library from the manifest (checkouts are reclaimed by prune).
    Rm {
        name: String,
        #[arg(long)]
        project: bool,
    },
    /// List registered libraries and their synced checkouts.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Fetch cloned repos and move default worktrees to their target.
    Sync { names: Vec<String> },
    /// Print the version-resolved checkout path (exactly one line on stdout).
    Path { name: String },
    /// Print checkout path, resolved version, layout map, and notes.
    Info {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Delete version checkouts no existing project references.
    Prune {
        /// Also delete unregistered libs without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Print a shell-completion script (bash, zsh, fish, …) to stdout.
    Completions { shell: Shell },
}

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("docm");
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Add {
            target,
            eco,
            package,
            repo,
            git_ref,
            src_dir,
            docs_dir,
            notes,
            project,
        } => cmd_add(
            target, eco, package, repo, git_ref, src_dir, docs_dir, notes, project,
        ),
        Cmd::Rm { name, project } => cmd_rm(&name, project),
        Cmd::List { json } => cmd_list(json),
        Cmd::Sync { names } => cmd_sync(&names),
        Cmd::Path { name } => cmd_path(&name),
        Cmd::Info { name, json } => cmd_info(&name, json),
        Cmd::Prune { yes } => cmd_prune(yes),
        Cmd::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "docm", &mut std::io::stdout());
            Ok(())
        }
    }
}

fn cwd() -> Result<PathBuf> {
    std::env::current_dir().context("resolving current directory")
}

fn discovered() -> Result<Discovered> {
    manifest::discover(&cwd()?, None)
}

fn find_entry(d: &Discovered, name: &str) -> Result<LibEntry> {
    d.manifest
        .libs
        .iter()
        .find(|l| l.name == name)
        .cloned()
        .with_context(|| {
            format!("`{name}` is not registered — see `docm list`, or `docm add {name}`")
        })
}

#[allow(clippy::too_many_arguments)]
fn cmd_add(
    target: String,
    eco: Option<Ecosystem>,
    package: Option<String>,
    repo: Option<String>,
    git_ref: Option<String>,
    src_dir: Option<String>,
    docs_dir: Option<String>,
    notes: Option<String>,
    project: bool,
) -> Result<()> {
    let is_url = target.contains("://") || target.starts_with("git@");
    let mut entry = if is_url {
        LibEntry {
            name: lookup::name_from_url(&target),
            ecosystem: Some(Ecosystem::Git),
            repo: Some(lookup::normalize(&target)),
            ..Default::default()
        }
    } else {
        let pkg = package.clone().unwrap_or_else(|| target.clone());
        let (eco, repo) = match (eco, repo) {
            (Some(e), Some(r)) => (e, r),
            (Some(Ecosystem::Git), None) => {
                anyhow::bail!("--eco git needs a git URL target or --repo")
            }
            (Some(e), None) => (e, lookup::Registry::repo_url(&lookup::Http, e, &pkg)?),
            (None, r) => {
                let (e, url) = lookup::detect(&lookup::Http, &pkg)?;
                (e, r.unwrap_or(url))
            }
        };
        LibEntry {
            name: target.clone(),
            ecosystem: Some(eco),
            package,
            repo: Some(lookup::normalize(&repo)),
            ..Default::default()
        }
    };
    entry.r#ref = git_ref;
    entry.src_dir = src_dir;
    entry.docs_dir = docs_dir;
    entry.notes = notes;

    let dest = if project {
        let d = discovered()?;
        let path = d
            .project_devkit_toml
            .context("no devkit.toml found walking up from CWD (required for --project)")?;
        manifest::upsert_project(&path, &entry)?;
        path
    } else {
        let path = manifest::global_docs_path();
        manifest::upsert_global(&path, &entry)?;
        path
    };
    println!(
        "registered {} ({}) -> {} in {}",
        entry.name,
        entry.ecosystem.map(|e| e.to_string()).unwrap_or_default(),
        entry.repo.as_deref().unwrap_or("-"),
        dest.display()
    );
    Ok(())
}

fn cmd_rm(name: &str, project: bool) -> Result<()> {
    let removed = if project {
        let d = discovered()?;
        let path = d
            .project_devkit_toml
            .context("no devkit.toml found walking up from CWD (required for --project)")?;
        manifest::remove_project(&path, name)?
    } else {
        manifest::remove_global(&manifest::global_docs_path(), name)?
    };
    if removed {
        println!("removed {name}; run `docm prune` to reclaim its checkouts");
        Ok(())
    } else {
        anyhow::bail!(
            "`{name}` was not in the {} manifest",
            if project { "project" } else { "global" }
        )
    }
}

fn cmd_list(json: bool) -> Result<()> {
    let d = discovered()?;
    let root = cache::docs_cache_root();
    if json {
        let items: Vec<serde_json::Value> = d
            .manifest
            .libs
            .iter()
            .map(|l| {
                let synced: Vec<String> = cache::LibCache::new(&root, &l.name)
                    .version_worktrees()
                    .into_iter()
                    .map(|(n, _)| n)
                    .collect();
                serde_json::json!({
                    "name": l.name,
                    "ecosystem": l.ecosystem,
                    "package": l.package_name(),
                    "repo": l.repo,
                    "ref": l.r#ref,
                    "synced": synced,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }
    if d.manifest.libs.is_empty() {
        println!("no libraries registered — `docm add <package>` or `docm add <git-url>`");
        return Ok(());
    }
    for l in &d.manifest.libs {
        let eco = l
            .ecosystem
            .map(|e| e.to_string())
            .unwrap_or_else(|| "?".into());
        let synced: Vec<String> = cache::LibCache::new(&root, &l.name)
            .version_worktrees()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        let synced = if synced.is_empty() {
            "(not synced)".to_string()
        } else {
            synced.join(", ")
        };
        println!(
            "{:<24} {:<7} {:<16} {synced}",
            l.name,
            eco,
            l.r#ref.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}

fn cmd_sync(names: &[String]) -> Result<()> {
    let d = discovered()?;
    let root = cache::docs_cache_root();
    let selected: Vec<&LibEntry> = d
        .manifest
        .libs
        .iter()
        .filter(|l| names.is_empty() || names.contains(&l.name))
        .collect();
    if let Some(unknown) = names
        .iter()
        .find(|n| !d.manifest.libs.iter().any(|l| &&l.name == n))
    {
        anyhow::bail!("`{unknown}` is not registered — see `docm list`");
    }
    for l in selected {
        let lib = cache::LibCache::new(&root, &l.name);
        if !lib.cloned() {
            eprintln!(
                "docm: {} not cloned yet (materialized on first lookup); skipping",
                l.name
            );
            continue;
        }
        lib.fetch()
            .with_context(|| format!("fetching {}", l.name))?;
        lib.sync_default(l.r#ref.as_deref())?;
        println!("synced {}", l.name);
    }
    Ok(())
}

fn resolve_one(name: &str) -> Result<resolve::Resolved> {
    let d = discovered()?;
    let entry = find_entry(&d, name)?;
    let r = resolve::resolve(&entry, &cwd()?, &cache::docs_cache_root())?;
    for w in &r.warnings {
        eprintln!("docm: {w}");
    }
    Ok(r)
}

fn cmd_path(name: &str) -> Result<()> {
    let r = resolve_one(name)?;
    println!("{}", r.path.display());
    Ok(())
}

fn cmd_info(name: &str, json: bool) -> Result<()> {
    let r = resolve_one(name)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(());
    }
    println!("name     {}", r.name);
    println!("version  {}", r.version);
    println!("path     {}", r.path.display());
    if let Some(d) = &r.layout.docs_dir {
        println!("docs     {d}");
    }
    if let Some(s) = &r.layout.src_dir {
        println!("src      {s}");
    }
    if let Some(e) = &r.layout.examples_dir {
        println!("examples {e}");
    }
    if let Some(k) = &r.layout.kind {
        println!("kind     {k}");
    }
    if let Some(n) = &r.notes {
        println!("notes    {n}");
    }
    Ok(())
}

fn cmd_prune(yes: bool) -> Result<()> {
    let d = discovered()?;
    let root = cache::docs_cache_root();
    if !root.is_dir() {
        println!("cache is empty");
        return Ok(());
    }
    let store = refs::RefStore::at(&root);
    let data = store.snapshot();

    let mut worktrees: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in std::fs::read_dir(&root)?.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let dirs = cache::LibCache::new(&root, &name)
            .version_worktrees()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        worktrees.insert(name, dirs);
    }
    let manifest_libs: BTreeSet<String> = d.manifest.libs.iter().map(|l| l.name.clone()).collect();

    let plan = refs::plan(&data, &worktrees, &manifest_libs, |project, lib| {
        let entry = d.manifest.libs.iter().find(|l| l.name == lib)?;
        current_version(entry, Path::new(project))
    });

    for (lib, wt) in &plan.delete {
        cache::LibCache::new(&root, lib).remove_worktree(wt)?;
        println!("removed {lib}/{wt}");
    }
    if !plan.removable_libs.is_empty() {
        println!(
            "unregistered libs with no references: {}",
            plan.removable_libs.join(", ")
        );
        if yes || confirm("delete them entirely? [y/N] ")? {
            for lib in &plan.removable_libs {
                std::fs::remove_dir_all(root.join(lib))
                    .with_context(|| format!("deleting {lib}"))?;
                println!("deleted {lib}");
            }
        }
    }
    // A resolution racing this rewrite re-records itself on its next lookup,
    // so replacing rows with the plan's survivors is safe.
    store.commit(|data| {
        data.rows = plan.keep.clone();
        Ok(())
    })?;
    if plan.delete.is_empty() && plan.removable_libs.is_empty() {
        println!("nothing to prune");
    }
    Ok(())
}

/// What a live project pins right now; `None` = it no longer references the lib.
fn current_version(entry: &LibEntry, project: &Path) -> Option<String> {
    if entry.r#ref.is_some() {
        return Some("default".into());
    }
    let eco = entry.ecosystem?;
    if eco == Ecosystem::Git {
        return Some("default".into());
    }
    let (_, versions) = lockfiles::find_version(project, eco, &entry.package_name())?;
    lockfiles::highest(versions)
}

fn confirm(prompt: &str) -> Result<bool> {
    use std::io::Write;
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(matches!(s.trim(), "y" | "Y" | "yes"))
}
