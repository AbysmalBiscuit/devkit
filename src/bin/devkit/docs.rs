use anyhow::{Context, Result};
use clap::Subcommand;
use devkit::completions::Shell;
use devkit_docs::manifest::{self, Discovered, Ecosystem, LibEntry};
use devkit_docs::{ManifestTarget, cache, lookup, refs, resolve, upgrade};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct DocsCli {
    #[command(subcommand)]
    pub cmd: Cmd,
    /// Check out the default branch when no tag or ref pins a version,
    /// instead of failing with a hard error.
    #[arg(long, global = true)]
    pub allow_default_branch: bool,
}

#[derive(Subcommand)]
pub(crate) enum Cmd {
    /// Register a library by package name or git URL.
    ///
    /// A package name is looked up on crates.io, npm, or PyPI in turn.
    Add {
        /// Package name to look up, or a git URL to clone directly.
        target: String,
        /// Ecosystem. Omitted probes crates.io, npm, PyPI in order.
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
        /// Freeform notes surfaced by `info`.
        #[arg(long)]
        notes: Option<String>,
        /// Write to the nearest devkit.toml [docs] section instead of the global manifest.
        #[arg(long)]
        project: bool,
    },
    /// Remove a library from the manifest (checkouts are reclaimed by prune).
    #[command(visible_alias = "remove", visible_alias = "delete")]
    Rm {
        /// Registered library to drop.
        name: String,
        /// Remove from the nearest devkit.toml [docs] section instead of the
        /// global manifest.
        #[arg(long)]
        project: bool,
    },
    /// List registered libraries and their synced checkouts.
    List {
        /// Emit the rows as JSON instead of a table.
        #[arg(long)]
        json: bool,
        /// Show only what this checkout evidences, with the resolved version
        /// each manifest and lockfile names, instead of the whole catalog.
        #[arg(long)]
        project: bool,
    },
    /// Fetch, re-resolve, re-materialize and verify registered libraries.
    Sync {
        /// Libraries to sync; omit for every registered library.
        names: Vec<String>,
    },
    /// Print the version-resolved checkout path (exactly one line on stdout).
    Path {
        /// Registered library to locate.
        name: String,
    },
    /// Print checkout path, resolved version, layout map, and notes.
    Info {
        /// Registered library to describe.
        name: String,
        /// Emit the report as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Release this project's reference to libraries.
    ///
    /// Checkouts are reclaimed later by `prune`.
    Forget {
        /// Libraries this project no longer needs.
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Delete version checkouts no existing project references.
    Prune {
        /// Also delete unregistered libs without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Print a shell-completion script (bash, zsh, fish, ...) to stdout.
    Completions {
        /// Shell to emit the script for.
        shell: Shell,
    },
}

pub fn run(cli: DocsCli) -> Result<()> {
    // A shell rc sources the completion script on every new shell, and the
    // script is written from the CLI definition alone — migrating the cache
    // for it would put cache diagnostics, or a cache failure, in shell startup.
    if !matches!(cli.cmd, Cmd::Completions { .. }) {
        for line in upgrade::run(&cache::docs_root())? {
            eprintln!("docm: {line}");
        }
    }
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
            target,
            eco,
            package,
            repo,
            git_ref,
            src_dir,
            docs_dir,
            notes,
            project,
            cli.allow_default_branch,
        ),
        Cmd::Rm { name, project } => cmd_rm(&name, project),
        Cmd::List { json, project } => cmd_list(json, project),
        Cmd::Sync { names } => cmd_sync(&names, cli.allow_default_branch),
        Cmd::Path { name } => cmd_path(&name, cli.allow_default_branch),
        Cmd::Info { name, json } => cmd_info(&name, json, cli.allow_default_branch),
        Cmd::Forget { names } => cmd_forget(&names),
        Cmd::Prune { yes } => cmd_prune(yes),
        Cmd::Completions { shell } => crate::emit_completions(shell, "docs", "docm"),
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
    allow_default_branch: bool,
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
    let cache_root = cache::docs_root();

    let dest = manifest_path(project)?;
    let added = devkit_docs::add_library(
        manifest_target(project, &dest),
        &cache_root,
        &cwd()?,
        &entry,
        &resolve::Options {
            allow_default_branch,
        },
    )?;
    for w in &added.resolved.warnings {
        eprintln!("docm: {w}");
    }
    let r = &added.resolved;
    println!(
        "registered {} ({}) -> {}",
        r.name,
        entry.ecosystem.map(|e| e.to_string()).unwrap_or_default(),
        r.origin
    );
    let inferred = if added.inferred_ref {
        " (inferred default branch; moves on `docm sync`)"
    } else {
        ""
    };
    println!("  ref       {}{inferred}", r.git_ref);
    if let Some(source) = &r.source {
        println!("  source    {source}");
    }
    println!("  commit    {}", r.commit);
    println!("  path      {}", r.path.display());
    println!("  manifest  {}", dest.display());
    Ok(())
}

/// The manifest a mutation writes to: the nearest `devkit.toml` under
/// `--project`, else the global file.
fn manifest_path(project: bool) -> Result<PathBuf> {
    if !project {
        return Ok(manifest::global_docs_path());
    }
    discovered()?
        .project_devkit_toml
        .context("no devkit.toml found walking up from CWD (required for --project)")
}

fn manifest_target(project: bool, path: &Path) -> ManifestTarget<'_> {
    if project {
        ManifestTarget::Project(path)
    } else {
        ManifestTarget::Global(path)
    }
}

fn cmd_rm(name: &str, project: bool) -> Result<()> {
    let cache_root = cache::docs_root();
    let dest = manifest_path(project)?;
    let removed = devkit_docs::rm_library(manifest_target(project, &dest), &cache_root, name)?;
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

struct LibState {
    /// The URL the bare clone was made from, which is what a checkout actually
    /// holds — not the manifest's declared repo.
    origin: Option<String>,
    checkouts: Vec<(String, cache::WorktreeMeta)>,
}

fn lib_state(root: &Path, name: &str) -> Result<LibState> {
    let lib = cache::LibCache::new(root, name)?;
    let meta = cache::read_meta(&lib.dir)?;
    Ok(LibState {
        origin: meta.origin.clone(),
        checkouts: lib
            .version_worktrees()
            .into_iter()
            .map(|(dirname, _)| {
                let recorded = meta.worktrees.get(&dirname).cloned().unwrap_or_default();
                (dirname, recorded)
            })
            .collect(),
    })
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

fn cmd_list(json: bool, project: bool) -> Result<()> {
    if project {
        // `pins` itself takes no lock and touches no cache; `main` still runs
        // `upgrade::run` before every subcommand, including this one.
        let pins = devkit_docs::pins::pins(&cwd()?, None)?;
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&devkit_docs::pins::envelope(&pins))?
            );
        } else {
            print!("{}", devkit_docs::pins::render(&pins));
        }
        return Ok(());
    }
    let d = discovered()?;
    let root = cache::docs_root();
    if json {
        let items: Vec<serde_json::Value> = d
            .manifest
            .libs
            .iter()
            .map(|l| -> Result<serde_json::Value> {
                let state = lib_state(&root, &l.name)?;
                let checkouts: Vec<serde_json::Value> = state
                    .checkouts
                    .into_iter()
                    .map(|(dirname, recorded)| {
                        serde_json::json!({
                            "worktree": dirname,
                            "ref": recorded.raw_ref,
                            "commit": recorded.commit,
                        })
                    })
                    .collect();
                Ok(serde_json::json!({
                    "name": l.name,
                    "ecosystem": l.ecosystem,
                    "package": l.package_name(),
                    "repo": l.repo,
                    "ref": l.r#ref,
                    "origin": state.origin,
                    "manifest": l.origin_file,
                    "checkouts": checkouts,
                }))
            })
            .collect::<Result<_>>()?;
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
        let state = lib_state(&root, &l.name)?;
        println!(
            "{:<24} {:<7} {:<16} {}",
            l.name,
            eco,
            l.r#ref.as_deref().unwrap_or("-"),
            state.origin.as_deref().unwrap_or("-")
        );
        if state.checkouts.is_empty() {
            println!("    (not synced)");
        }
        for (dirname, recorded) in state.checkouts {
            println!(
                "    {:<24} {:<12} {}",
                dirname,
                short(&recorded.commit),
                recorded.raw_ref
            );
        }
    }
    Ok(())
}

fn cmd_sync(names: &[String], allow_default_branch: bool) -> Result<()> {
    let d = discovered()?;
    let root = cache::docs_root();
    let start = cwd()?;
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
    let opts = resolve::Options {
        allow_default_branch,
    };
    let mut failed: Vec<&str> = Vec::new();
    for l in selected {
        match sync_one(l, &root, &start, &opts) {
            Ok(r) => println!(
                "synced {} {} ({}) -> {}",
                l.name,
                r.git_ref,
                short(&r.commit),
                r.path.display()
            ),
            Err(e) => {
                eprintln!("docm: {}: {e:#}", l.name);
                failed.push(&l.name);
            }
        }
    }
    if !failed.is_empty() {
        anyhow::bail!("could not sync: {}", failed.join(", "));
    }
    Ok(())
}

/// Fetch, then re-resolve, re-materialize and re-verify one library, all under
/// its library lock so a concurrent prune cannot reclaim what this just built.
fn sync_one(
    entry: &LibEntry,
    cache_root: &Path,
    start: &Path,
    opts: &resolve::Options,
) -> Result<resolve::Resolved> {
    devkit_docs::locks::with_lib(cache_root, &entry.name, || {
        let lib = cache::LibCache::new(cache_root, &entry.name)?;
        if lib.cloned() {
            lib.fetch()
                .with_context(|| format!("fetching {}", entry.name))?;
        }
        let mut entry = entry.clone();
        record_default_branch(&mut entry, &lib, cache_root)?;
        let r = resolve::resolve_locked(&entry, start, cache_root, opts)?;
        for w in &r.warnings {
            eprintln!("docm: {w}");
        }
        Ok(r)
    })
}

/// Pin a git entry that predates mandatory refs to the repo's current default
/// branch. Only the global manifest is written: a `devkit.toml` is committed to
/// a repo, where an inferred branch would read as a deliberate team decision.
fn record_default_branch(
    entry: &mut LibEntry,
    lib: &cache::LibCache,
    cache_root: &Path,
) -> Result<()> {
    if entry.ecosystem != Some(Ecosystem::Git) || entry.r#ref.is_some() {
        return Ok(());
    }
    let global = manifest::global_docs_path();
    if entry.origin_file.as_deref() != Some(global.as_path()) {
        anyhow::bail!(
            "`{}` is a git entry with no ref, declared in {}; docm will not write an \
             inferred default branch into a devkit.toml, because a repo-committed pin \
             reads as a team decision — add one there with \
             `docm add {} --project --ref <tag|branch|sha>`",
            entry.name,
            entry
                .origin_file
                .as_deref()
                .unwrap_or(Path::new("an unknown manifest"))
                .display(),
            entry.name
        );
    }
    let repo = entry
        .repo
        .as_deref()
        .with_context(|| format!("lib `{}` has no repo url", entry.name))?;
    let mut meta = cache::read_meta(&lib.dir)?;
    lib.ensure_clone(repo, &mut meta)?;
    entry.r#ref = Some(lib.default_branch()?);
    manifest::upsert_global(&global, entry, cache_root)?;
    eprintln!(
        "docm: recorded {} ref {} in {} (inferred default branch)",
        entry.name,
        entry.r#ref.as_deref().unwrap_or_default(),
        global.display()
    );
    Ok(())
}

fn resolve_one(name: &str, allow_default_branch: bool) -> Result<resolve::Resolved> {
    let d = discovered()?;
    let entry = find_entry(&d, name)?;
    let opts = resolve::Options {
        allow_default_branch,
    };
    let r = resolve::resolve(&entry, &cwd()?, &cache::docs_root(), &opts)?;
    for w in &r.warnings {
        eprintln!("docm: {w}");
    }
    Ok(r)
}

fn cmd_path(name: &str, allow_default_branch: bool) -> Result<()> {
    let r = resolve_one(name, allow_default_branch)?;
    println!("{}", r.path.display());
    Ok(())
}

fn cmd_info(name: &str, json: bool, allow_default_branch: bool) -> Result<()> {
    let r = resolve_one(name, allow_default_branch)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(());
    }
    println!("name     {}", r.name);
    println!("repo     {}", r.origin);
    println!("ref      {}", r.git_ref);
    println!("version  {}", r.version);
    if let Some(source) = &r.source {
        println!("source   {source}");
    }
    println!("commit   {}", r.commit);
    println!("status   {}", r.status);
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

/// Reads the reference registry, not the manifest: a row outlives the
/// registration that created it, and on its own keeps a library in this
/// project's `docm list --project`.
fn cmd_forget(names: &[String]) -> Result<()> {
    let root = cache::docs_root();
    let keys = devkit_docs::pins::project_keys(&cwd()?, None)?;
    let mut unreferenced: Vec<&str> = Vec::new();
    let mut released = false;
    for name in names {
        let dropped = refs::forget(&root, &keys, name)?;
        if dropped.is_empty() {
            unreferenced.push(name);
            continue;
        }
        released = true;
        for row in dropped {
            println!(
                "forgot {} {} for {}",
                row.lib,
                devkit_docs::names::decode(&row.version),
                row.project
            );
        }
    }
    if released {
        println!("run `docm prune` to reclaim what nothing references now");
    }
    if !unreferenced.is_empty() {
        anyhow::bail!(
            "this project references no {} — `docm list --project` shows what it does reference",
            unreferenced.join(", ")
        );
    }
    Ok(())
}

fn cmd_prune(yes: bool) -> Result<()> {
    let d = discovered()?;
    let root = cache::docs_root();
    if !root.is_dir() {
        println!("cache is empty");
        return Ok(());
    }
    let manifest_libs: BTreeSet<String> = d.manifest.libs.iter().map(|l| l.name.clone()).collect();

    let pruned = refs::prune_with_lock(&root, &manifest_libs, None)?;

    for removed in &pruned.removed {
        println!("removed {removed}");
    }
    for skipped in &pruned.skipped {
        println!("skipped {}: {}", skipped.entry, skipped.reason);
    }
    if !pruned.removable_libs.is_empty() {
        println!(
            "unregistered libs with no references: {}",
            pruned.removable_libs.join(", ")
        );
        if yes || confirm("delete them entirely? [y/N] ")? {
            for lib in &pruned.removable_libs {
                if cache::LibCache::from_dir(&root, &devkit_docs::names::encode(lib))
                    .remove_if_unreferenced()?
                {
                    println!("deleted {lib}");
                }
            }
        }
    }
    if pruned.removed.is_empty() && pruned.removable_libs.is_empty() {
        println!("nothing to prune");
    }
    Ok(())
}

fn confirm(prompt: &str) -> Result<bool> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        eprintln!("docm: not a terminal; skipping (pass --yes to delete non-interactively)");
        return Ok(false);
    }
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(matches!(s.trim(), "y" | "Y" | "yes"))
}
