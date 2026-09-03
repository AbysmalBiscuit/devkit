
use anyhow::{Context, Result};
use clap::{Subcommand, ValueEnum};
use devkit::completions::Shell;
use devkit_common::git::Git;
use devkit_common::progress::Steps;
use devkit_common::supervise;
use devkit_common::ui;
use devkit_ports::load;
use devkit_ports::registry::{self, Role};
use devkit_ports::run;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

/// `--timing` verbosity, parsed by clap. `--timing` alone = summary,
/// `--timing=trace` = per-op detail.
#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum TimingFlag {
    Summary,
    Trace,
}

/// Resolve the timing mode: the flag wins; otherwise fall back to `DEVKIT_TIMING`.
fn timing_mode(flag: Option<TimingFlag>) -> devkit_common::timing::Mode {
    use devkit_common::timing::Mode;
    match flag {
        Some(TimingFlag::Summary) => Mode::Summary,
        Some(TimingFlag::Trace) => Mode::Trace,
        None => devkit_common::timing::mode_from_env(),
    }
}

#[derive(clap::Args)]
pub struct RunCli {
    /// Run as if this command had started in DIR instead of the current directory.
    #[arg(short = 'C', long = "dir", global = true)]
    pub dir: Option<String>,
    /// devkit.toml to load instead of the one discovered from the start directory.
    #[arg(long, global = true)]
    pub config: Option<String>,
    /// Print IO timing to stderr. `--timing` = summary, `--timing=trace` = per-op.
    #[arg(long, global = true, value_name = "MODE", num_args = 0..=1, default_missing_value = "summary")]
    pub timing: Option<TimingFlag>,
    /// Write one JSON record per timed IO op to FILE.
    #[arg(long = "timing-log", global = true, value_name = "FILE")]
    pub timing_log: Option<PathBuf>,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand)]
pub(crate) enum Cmd {
    /// Bring up dev servers for the selected apps.
    Up {
        /// Apps to start; omit to infer them from the diff against the baseline
        /// ref. A fresh worktree has no diff yet, so name them there.
        apps: Vec<String>,
        /// Which side to run.
        #[arg(long, value_enum, default_value = "issue")]
        role: RoleSelector,
        /// Environment override for the launched servers, above the app's
        /// `static_env`. Repeatable.
        #[arg(long = "env", value_name = "K=V")]
        env: Vec<String>,
        /// Read K=V overrides from a file (blank lines and `#` comments
        /// skipped). A key also passed with --env wins.
        #[arg(long = "env-file")]
        env_file: Option<String>,
        /// Print the launch plan without starting anything.
        #[arg(long)]
        dry_run: bool,
        /// Hand servers to the supervisor daemon (autostarting it) so they restart on crash.
        #[arg(long)]
        supervise: bool,
    },
    /// Stop servers and release ports.
    ///
    /// Defaults to this worktree and the baseline only it names; reaching
    /// another worktree, or a shared baseline, needs
    /// --all/--others/--holder and prompts (requires a terminal).
    Down {
        /// Fuzzy selectors matched (substring) across columns. Mutually exclusive
        /// with the column filters below.
        #[arg(conflicts_with_all = ["app", "port", "role", "pid", "listening", "not_listening", "older_than"])]
        selectors: Vec<String>,
        /// Every holder, including this worktree.
        #[arg(long)]
        all: bool,
        /// Every holder except this worktree.
        #[arg(long, conflicts_with = "all")]
        others: bool,
        /// One specific worktree (repeatable), by path.
        #[arg(long = "holder", conflicts_with_all = ["all", "others"])]
        holders: Vec<String>,
        /// Collapse cross-worktree confirmation into one combined prompt.
        #[arg(long)]
        batch: bool,
        /// Filter: app name (repeatable).
        #[arg(long)]
        app: Vec<String>,
        /// Filter: port (repeatable).
        #[arg(long)]
        port: Vec<u16>,
        /// Filter: role.
        #[arg(long, value_enum)]
        role: Option<RoleSelector>,
        /// Filter: pid.
        #[arg(long)]
        pid: Option<u32>,
        /// Filter: only servers currently listening.
        #[arg(long, conflicts_with = "not_listening")]
        listening: bool,
        /// Filter: only servers not currently listening.
        #[arg(long = "not-listening")]
        not_listening: bool,
        /// Filter: only servers older than this (90s, 30m, 2h, 1d).
        #[arg(long = "older-than")]
        older_than: Option<String>,
    },
    /// Show tracked servers (this worktree, or --all).
    Status {
        /// Report every worktree's servers, not only this one's.
        #[arg(long)]
        all: bool,
    },
    /// Kill untracked dev servers (interactive terminal only).
    ///
    /// This worktree by default; `--all` reaches every worktree. No agent
    /// path exists, since the terminal requirement has no bypass.
    Reap {
        /// Reach every worktree, not only this one.
        #[arg(long)]
        all: bool,
    },
    /// Print (or follow) the log for one app.
    Logs {
        /// App whose log to read.
        app: String,
        /// Which side's log; omit to take whichever role is tracked.
        #[arg(long, value_enum)]
        role: Option<RoleSelector>,
        /// Follow the log instead of printing the last 200 lines.
        #[arg(short = 'f', long)]
        follow: bool,
    },
    /// Print a shell-completion script (bash, zsh, fish, ...) to stdout.
    Completions {
        /// Shell to emit the script for.
        shell: Shell,
    },
    /// Inspect and reclaim this project's baseline worktrees.
    Baseline {
        #[command(subcommand)]
        cmd: BaselineCmd,
    },
    /// Run a canned task from `[tasks]` (no name: list the configured tasks).
    Task {
        /// Task to run; omit to list the configured tasks.
        name: Option<String>,
        /// Environment override applied to every step of the task. Repeatable.
        #[arg(long = "env", value_name = "K=V")]
        env: Vec<String>,
        /// Read K=V overrides from a file (blank lines and `#` comments
        /// skipped). A key also passed with --env wins.
        #[arg(long = "env-file")]
        env_file: Option<String>,
        /// Print the rendered plan (cwd, argv, env, resolved ports) without executing.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum BaselineCmd {
    /// List the baseline worktrees, with the worktrees referencing each.
    List,
    /// Remove every baseline no worktree references any more.
    Prune {
        /// Report what a real sweep would remove, removing nothing.
        #[arg(long)]
        dry_run: bool,
        /// Remove a baseline even while servers are running under it.
        #[arg(long)]
        force: bool,
        /// Remove a baseline even though tracked files in it were edited,
        /// discarding those edits with the tree.
        #[arg(long)]
        discard_edits: bool,
    },
}

/// CLI selector over registry roles. `Both` runs/affects the issue branch and a
/// fresh baseline side-by-side; it is not itself a registry `Role`.
#[derive(Clone, Copy, ValueEnum, PartialEq)]
pub(crate) enum RoleSelector {
    /// The issue branch's servers.
    Issue,
    /// The baseline checkout's servers, for A/B comparison.
    Baseline,
    /// Both roles: `up` runs them side-by-side on separate ports, filters match
    /// either.
    Both,
}

impl RoleSelector {
    /// Registry roles this selector expands to (for `up`).
    fn roles(self) -> &'static [Role] {
        match self {
            RoleSelector::Issue => &[Role::Issue],
            RoleSelector::Baseline => &[Role::Baseline],
            RoleSelector::Both => &[Role::Issue, Role::Baseline],
        }
    }

    /// Registry-role filter for `down`/`logs`: `None` means "all roles".
    fn filter(self) -> Option<Role> {
        match self {
            RoleSelector::Issue => Some(Role::Issue),
            RoleSelector::Baseline => Some(Role::Baseline),
            RoleSelector::Both => None,
        }
    }
}

fn cwd_of(cli: &RunCli) -> String {
    cli.dir.clone().unwrap_or_else(|| ".".to_string())
}

fn toplevel(cwd: &str) -> Result<String> {
    Ok(devkit_common::git::checkout_root(Path::new(cwd))?
        .to_string_lossy()
        .into_owned())
}

/// Pick known apps whose files appear in a `git diff --stat` against the baseline.
pub fn apps_from_diff(diff_stat: &str, known: &[String], apps_dir: &str) -> Vec<String> {
    let prefix = format!("{apps_dir}/");
    let mut found = Vec::new();
    for line in diff_stat.lines() {
        if let Some(rest) = line.trim().strip_prefix(&prefix)
            && let Some(name) = rest.split('/').next()
            && known.iter().any(|k| k == name)
            && !found.contains(&name.to_string())
        {
            found.push(name.to_string());
        }
    }
    found
}

/// The configured app names, appended to an error that leaves the caller
/// without a usable one. Sorted, because the catalog is a hash map and an
/// arbitrary order reads as noise.
fn available_apps(known: &[String]) -> String {
    if known.is_empty() {
        return "no apps configured (add [apps.<name>] to devkit.toml)".into();
    }
    let mut names: Vec<&str> = known.iter().map(String::as_str).collect();
    names.sort_unstable();
    format!("available apps: {}", names.join(", "))
}

fn parse_user_env(pairs: &[String], file: Option<&str>) -> Result<BTreeMap<String, String>> {
    let mut m = BTreeMap::new();
    if let Some(f) = file {
        let body = std::fs::read_to_string(f).with_context(|| format!("reading env-file {f}"))?;
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                m.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    for p in pairs {
        let (k, v) = p
            .split_once('=')
            .with_context(|| format!("--env must be K=V, got `{p}`"))?;
        m.insert(k.to_string(), v.to_string());
    }
    Ok(m)
}

/// Parse an age threshold like `90s`, `30m`, `2h`, `1d` (bare number = seconds) to seconds.
fn parse_age(s: &str) -> Result<u64> {
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix('s') {
        (n, 1u64)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3600)
    } else if let Some(n) = s.strip_suffix('d') {
        (n, 86_400)
    } else {
        (s, 1)
    };
    let v: u64 = num
        .trim()
        .parse()
        .with_context(|| format!("invalid --older-than `{s}`: expected e.g. 90s, 30m, 2h, 1d"))?;
    let secs = v
        .checked_mul(mult)
        .with_context(|| format!("--older-than `{s}` is too large"))?;
    Ok(secs)
}

/// CLI inputs for `down`, normalized (role already collapsed to a registry `Role`,
/// `--older-than` already parsed to seconds). Kept separate from the clap variant so
/// the selector builder is unit-testable.
#[derive(Default)]
struct DownArgs {
    selectors: Vec<String>,
    all: bool,
    others: bool,
    holders: Vec<String>,
    batch: bool,
    app: Vec<String>,
    port: Vec<u16>,
    role: Option<Role>,
    pid: Option<u32>,
    listening: bool,
    not_listening: bool,
    older_than_secs: Option<u64>,
}

/// Build the registry selector from CLI args. `--holder` paths resolve to their git
/// toplevel when possible, else are used verbatim.
fn build_selector(
    a: &DownArgs,
    current: &str,
    own_baseline: Option<&Path>,
) -> registry::DownSelector {
    let scope = if !a.holders.is_empty() {
        registry::Scope::Holders(
            a.holders
                .iter()
                .map(|h| toplevel(h).unwrap_or_else(|_| h.clone()))
                .collect(),
        )
    } else if a.all {
        registry::Scope::All
    } else if a.others {
        registry::Scope::Others(current.to_string())
    } else if let Some(b) = own_baseline {
        registry::Scope::Holders(vec![current.to_string(), b.to_string_lossy().into_owned()])
    } else {
        registry::Scope::Current(current.to_string())
    };

    let has_columns = !a.app.is_empty()
        || !a.port.is_empty()
        || a.role.is_some()
        || a.pid.is_some()
        || a.listening
        || a.not_listening
        || a.older_than_secs.is_some();

    let filter = if !a.selectors.is_empty() {
        registry::Filter::Tokens(a.selectors.clone())
    } else if has_columns {
        let listening = if a.listening {
            Some(true)
        } else if a.not_listening {
            Some(false)
        } else {
            None
        };
        registry::Filter::Columns(registry::ColumnFilter {
            app: a.app.clone(),
            port: a.port.clone(),
            role: a.role,
            pid: a.pid,
            listening,
            older_than_secs: a.older_than_secs,
        })
    } else {
        registry::Filter::All
    };

    registry::DownSelector { scope, filter }
}

/// Options controlling how `cmd_up` launches apps.
struct UpFlags {
    dry_run: bool,
    supervise: bool,
}

struct Row {
    role: Role,
    app: String,
    port: u16,
    url: String,
    pid: Option<u32>,
    log: PathBuf,
    ready: Option<bool>,
}

fn print_summary(rows: &[Row]) {
    let mut t = ui::table(&["ROLE", "APP", "PORT", "URL", "PID", "READY", "LOG"]);
    let table_rows = rows
        .iter()
        .map(|r| {
            vec![
                r.role.to_string(),
                r.app.clone(),
                r.port.to_string(),
                r.url.clone(),
                r.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                match r.ready {
                    Some(true) => "yes".into(),
                    Some(false) => "NO".into(),
                    None => "-".into(),
                },
                r.log.display().to_string(),
            ]
        })
        .collect();
    ui::add_rows_linking_urls(&mut t, table_rows, 3);
    println!("{t}");
}

pub fn run(cli: RunCli) -> Result<()> {
    let _timing = devkit_common::timing::init(timing_mode(cli.timing), cli.timing_log.clone());
    let cwd = cwd_of(&cli);
    match &cli.cmd {
        Cmd::Up {
            apps,
            role,
            env,
            env_file,
            dry_run,
            supervise,
        } => cmd_up(
            &cli,
            &cwd,
            apps,
            *role,
            env,
            env_file.as_deref(),
            UpFlags {
                dry_run: *dry_run,
                supervise: *supervise,
            },
        ),
        Cmd::Down {
            selectors,
            all,
            others,
            holders,
            batch,
            app,
            port,
            role,
            pid,
            listening,
            not_listening,
            older_than,
        } => {
            let older_than_secs = match older_than {
                Some(s) => Some(parse_age(s)?),
                None => None,
            };
            let args = DownArgs {
                selectors: selectors.clone(),
                all: *all,
                others: *others,
                holders: holders.clone(),
                batch: *batch,
                app: app.clone(),
                port: port.clone(),
                role: role.and_then(RoleSelector::filter),
                pid: *pid,
                listening: *listening,
                not_listening: *not_listening,
                older_than_secs,
            };
            cmd_down(&cwd, &args)
        }
        Cmd::Status { all } => cmd_status(&cwd, *all),
        Cmd::Reap { all } => cmd_reap(&cwd, *all),
        Cmd::Logs { app, role, follow } => {
            cmd_logs(&cwd, app, role.and_then(RoleSelector::filter), *follow)
        }
        Cmd::Completions { shell } => crate::emit_completions(*shell, "run", "devrun"),
        Cmd::Baseline { cmd } => match cmd {
            BaselineCmd::List => cmd_baseline_list(&cli, &cwd),
            BaselineCmd::Prune {
                dry_run,
                force,
                discard_edits,
            } => cmd_baseline_prune(
                &cli,
                &cwd,
                *dry_run,
                crate::baseline::Gates::sweep(*force, *discard_edits),
            ),
        },
        Cmd::Task {
            name,
            env,
            env_file,
            dry_run,
        } => cmd_task(
            &cli,
            &cwd,
            name.as_deref(),
            env,
            env_file.as_deref(),
            *dry_run,
        ),
    }
}

/// The baseline directory this project's config names, and the checkout every
/// referencer scan and worktree removal is run against.
fn baseline_scope(cli: &RunCli, cwd: &str) -> Result<(PathBuf, String)> {
    let loaded = load::load(cli.config.as_deref().map(Path::new), Path::new(cwd))?;
    let dir = crate::baseline::dir(&loaded.config)?;
    let repo = devkit_common::git::primary_checkout(Path::new(cwd))?;
    Ok((dir, repo.to_string_lossy().into_owned()))
}

/// A baseline holds a dependency tree, so MiB is the useful unit until the tree
/// is small enough for it to read as nothing at all.
fn human_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * KIB;
    match bytes {
        b if b >= MIB => format!("{} MiB", b / MIB),
        b if b >= KIB => format!("{} KiB", b / KIB),
        b => format!("{b} B"),
    }
}

fn cmd_baseline_list(cli: &RunCli, cwd: &str) -> Result<()> {
    let (dir, repo) = baseline_scope(cli, cwd)?;
    let listing = crate::baseline::list(&dir, &repo)?;
    // The note goes out whether or not there is a table under it: it is the
    // reason a sweep would reclaim nothing, and it outlives an empty directory.
    if let Some(note) = &listing.note {
        eprintln!("warning: {note}");
    }
    if listing.baselines.is_empty() {
        println!("no baselines under {}", dir.display());
        return Ok(());
    }
    println!("{}", dir.display());
    let mut t = ui::table(&["BASELINE", "SHA", "STATE", "SIZE", "REFERENCED BY"]);
    for r in &listing.baselines {
        let referencers = if r.referencers.is_empty() {
            "-".to_string()
        } else {
            r.referencers
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        t.add_row(vec![
            r.path.file_name().unwrap_or_default().to_string_lossy(),
            match &r.sha {
                Some(sha) => crate::baseline::short(sha).into(),
                None => "-".into(),
            },
            r.state.to_string().into(),
            human_size(r.bytes).into(),
            referencers.into(),
        ]);
    }
    println!("{t}");
    Ok(())
}

fn cmd_baseline_prune(
    cli: &RunCli,
    cwd: &str,
    dry_run: bool,
    gates: crate::baseline::Gates,
) -> Result<()> {
    let (dir, repo) = baseline_scope(cli, cwd)?;
    let ports = registry::snapshot()?;
    let out = crate::baseline::prune_all(&dir, &repo, &ports, dry_run, gates)?;
    if out.removed.is_empty() {
        println!("no baseline to remove under {}", dir.display());
    } else {
        println!("{}:", if dry_run { "would remove" } else { "removed" });
        for p in &out.removed {
            println!("  {}", p.display());
        }
    }
    if !out.reported.is_empty() {
        println!(
            "left alone, no {}:",
            devkit_common::worktree::BASELINE_MARKER
        );
        for p in &out.reported {
            println!("  {}", p.display());
        }
    }
    // A sweep that refused work did not do what it was asked, and a script or a
    // hook reading the exit status has no other way to learn that.
    anyhow::ensure!(
        out.refused.is_empty(),
        "{} baseline(s) refused; see the warnings above",
        out.refused.len()
    );
    Ok(())
}

fn cmd_task(
    cli: &RunCli,
    cwd: &str,
    name: Option<&str>,
    env_pairs: &[String],
    env_file: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    use devkit_ports::task::{self, Resolved, SeqItem};

    let loaded = load::load(cli.config.as_deref().map(Path::new), Path::new(cwd))?;
    let Some(name) = name else {
        let rows = task::list(&loaded.config);
        print!("{}", task::tasks_text(&rows));
        return Ok(());
    };

    let user = parse_user_env(env_pairs, env_file)?;
    let root = toplevel(cwd)?;
    let resolved = task::resolve(
        &loaded.config,
        &loaded.catalog,
        Path::new(&root),
        &root,
        name,
        &user,
    )?;
    let root_path = Path::new(&root);
    match resolved {
        Resolved::Command(plan) => {
            if dry_run {
                return run_task_step(&plan, true);
            }
            let fresh = task::resolve_step(
                &loaded.config,
                &loaded.catalog,
                root_path,
                &root,
                name,
                &user,
            )?;
            run_task_step(&fresh, false)
        }
        Resolved::Sequence(items) => {
            for item in &items {
                match item {
                    SeqItem::Run(plan) => {
                        if dry_run {
                            run_task_step(plan, true)?;
                        } else {
                            let fresh = task::resolve_step(
                                &loaded.config,
                                &loaded.catalog,
                                root_path,
                                &root,
                                &plan.name,
                                &user,
                            )?;
                            run_task_step(&fresh, false)?;
                        }
                    }
                    SeqItem::Up(app) => {
                        cmd_up(
                            cli,
                            cwd,
                            std::slice::from_ref(app),
                            RoleSelector::Issue,
                            env_pairs,
                            env_file,
                            UpFlags {
                                dry_run,
                                supervise: false,
                            },
                        )?;
                    }
                }
            }
            Ok(())
        }
    }
}

/// Print or execute one command step. A non-zero child exits the process with
/// the child's code, so a sequence stops at its first failure.
fn run_task_step(plan: &devkit_ports::task::CommandPlan, dry_run: bool) -> Result<()> {
    if dry_run {
        println!("[{}]", plan.name);
        println!("  cwd:  {}", plan.cwd.display());
        println!("  argv: {}", plan.argv.join(" "));
        let envs: Vec<String> = plan.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        println!("  env:  {}", envs.join(" "));
        return Ok(());
    }
    eprintln!("→ {}: {}", plan.name, plan.argv.join(" "));
    let status = devkit_ports::task::exec(plan)?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn cmd_up(
    cli: &RunCli,
    cwd: &str,
    apps_arg: &[String],
    role: RoleSelector,
    env_pairs: &[String],
    env_file: Option<&str>,
    flags: UpFlags,
) -> Result<()> {
    let UpFlags { dry_run, supervise } = flags;
    #[cfg(not(feature = "daemon"))]
    let _ = supervise;
    let loaded = load::load(cli.config.as_deref().map(Path::new), Path::new(cwd))?;
    let cfg = &loaded.config;
    let catalog = &loaded.catalog;
    let known: Vec<String> = catalog.keys().cloned().collect();

    let mut apps: Vec<String> = apps_arg.to_vec();
    if apps.is_empty() {
        // A run that names its apps needs no baseline; only diff-detection
        // does, so the target resolves here rather than up front.
        let baseline_target = crate::baseline::target(cfg, Path::new(cwd))?;
        let diff = Git::at(Path::new(cwd))
            .args(["diff", &format!("{baseline_target}...HEAD"), "--stat"])
            .output()
            .unwrap_or_default();
        apps = apps_from_diff(&diff, &known, &cfg.defaults.apps_dir);
        anyhow::ensure!(
            !apps.is_empty(),
            "no apps to run (none given and none detected in diff vs {baseline_target})\n{}",
            available_apps(&known)
        );
    }
    for a in &apps {
        anyhow::ensure!(
            catalog.contains_key(a),
            "unknown app `{a}`\n{}",
            available_apps(&known)
        );
    }
    run::ensure_provider(catalog, &mut apps);

    let user = parse_user_env(env_pairs, env_file)?;
    let issue_holder = toplevel(cwd)?;

    let steps = Steps::new();

    // (role, holder, base_dir) — base_dir is where <app.path> is rooted. Every
    // baseline is resolved here, before any port is, so a long bootstrap cannot
    // outlive a reservation taken alongside it.
    let groups: Vec<(Role, String, PathBuf)> = {
        let mut g = Vec::new();
        for r in role.roles() {
            match r {
                Role::Issue => {
                    g.push((
                        Role::Issue,
                        issue_holder.clone(),
                        PathBuf::from(&issue_holder),
                    ));
                }
                Role::Baseline => {
                    let wt = Path::new(&issue_holder);
                    // A baseline is its own baseline: bootstrapping from inside
                    // one would build a second tree for the same fork point and
                    // pin it to itself, under the `DETACHED` identity its HEAD
                    // reports.
                    if devkit_common::worktree::is_baseline(wt) {
                        g.push((
                            Role::Baseline,
                            issue_holder.clone(),
                            PathBuf::from(&issue_holder),
                        ));
                        continue;
                    }
                    let baseline_target = crate::baseline::target(cfg, Path::new(cwd))?;
                    let sha = crate::baseline::pin(wt, &baseline_target)?;
                    // A dry run reports the baseline it would use and leaves the
                    // world as it found it. Bootstrapping runs the project's
                    // `setup` commands and `after_worktree_create` hooks, and
                    // repinning stops the servers under the pin it replaces —
                    // effects nobody asking for a plan has consented to.
                    if dry_run {
                        let path = crate::baseline::planned_path(cfg, &sha)?;
                        g.push((Role::Baseline, path.to_string_lossy().into_owned(), path));
                        continue;
                    }
                    // Read before `ensure`, which moves the pin onto the
                    // baseline it built.
                    let previous = devkit_common::record::read(wt).and_then(|r| r.baseline);
                    let primary = devkit_common::git::primary_checkout(Path::new(cwd))?;
                    let path =
                        crate::baseline::ensure(cfg, catalog, &primary, wt, &sha, &apps, &steps)?;
                    // A rebase repoints this worktree at a different baseline.
                    // The servers it started there stay alive under a holder
                    // this worktree no longer reaches, so they come down once
                    // the pin has moved — unless another worktree is pinned to
                    // the same baseline, in which case they are still its.
                    //
                    // A failure here is warned about rather than propagated:
                    // the pin already names the new baseline, so a later run
                    // reads `previous` as the current baseline and never
                    // reaches this call again. Failing the run would leave the
                    // old servers orphaned with nothing left to notice them.
                    if let Some(prev) = previous.filter(|p| Path::new(&p.path) != path)
                        && let Err(e) = crate::baseline::release_abandoned(
                            &issue_holder,
                            wt,
                            Path::new(&prev.path),
                            |rows| run::bring_down_ports(rows).map(|_| ()),
                        )
                    {
                        eprintln!(
                            "warning: the servers under {} were left running: {e:#}\n\
                             stop them with `devrun down --holder {}` (needs a terminal); \
                             `devrun baseline prune --force` reclaims the directory and \
                             leaves them running",
                            prev.path, prev.path
                        );
                    }
                    g.push((Role::Baseline, path.to_string_lossy().into_owned(), path));
                }
            }
        }
        g
    };

    let provider = catalog
        .iter()
        .find(|(_, a)| a.provides_url)
        .map(|(n, _)| n.clone());

    let mut rows: Vec<Row> = Vec::new();
    for (grp_role, holder, base_dir) in &groups {
        let ports =
            run::resolve_ports(catalog, &apps, holder, *grp_role, &cfg.templates.variables)?;
        let plans = run::plan_group(
            catalog,
            &apps,
            &ports,
            provider.as_deref(),
            base_dir,
            *grp_role,
            &user,
            &cfg.templates.variables,
        )?;

        if dry_run {
            for p in &plans {
                println!("[{}] {} :{}", grp_role.as_str(), p.app, p.port);
                println!("  cwd:  {}", p.cwd.display());
                println!("  argv: {}", p.argv.join(" "));
                let envs: Vec<String> = p.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
                println!("  env:  {}", envs.join(" "));
                println!("  log:  {}", p.log.display());
                rows.push(Row {
                    role: *grp_role,
                    app: p.app.clone(),
                    port: p.port,
                    url: p.url.clone(),
                    pid: None,
                    log: p.log.clone(),
                    ready: None,
                });
            }
            continue;
        }

        let urls: HashMap<String, String> = plans
            .iter()
            .map(|p| (p.app.clone(), p.url.clone()))
            .collect();
        let statuses = steps.during(
            &format!("Starting {} server(s) [{}]…", apps.len(), grp_role.as_str()),
            || run::launch(&plans, holder, *grp_role, supervise, true),
        )?;
        for s in statuses {
            if s.state != devkit_ports::run::ServerState::Ready
                && let Some(log) = &s.logfile
            {
                eprintln!(
                    "--- {} ({}) did not become ready; last 30 log lines: ---",
                    s.app,
                    grp_role.as_str()
                );
                eprintln!("{}", supervise::tail(log, 30));
            }
            rows.push(Row {
                role: s.role,
                url: urls.get(&s.app).cloned().unwrap_or_default(),
                app: s.app,
                port: s.port,
                pid: s.pid,
                log: s.logfile.unwrap_or_default(),
                ready: Some(s.state == devkit_ports::run::ServerState::Ready),
            });
        }
    }

    print_summary(&rows);
    Ok(())
}

/// True if any matched row belongs to a holder other than `current` or the
/// baseline `current` is the sole referencer of.
fn touches_foreign(
    matched: &[(u16, &registry::Entry)],
    current: &str,
    own_baseline: Option<&Path>,
) -> bool {
    matched
        .iter()
        .any(|(_, e)| !is_own(e, current, own_baseline))
}

/// Whether a row belongs to this worktree: its own holder, or the baseline it
/// is the sole referencer of. Both sides of the baseline comparison are the
/// path the baseline was created at — the record stores it and `up` writes it
/// verbatim as the holder — so they match exactly.
fn is_own(e: &registry::Entry, current: &str, own_baseline: Option<&Path>) -> bool {
    e.holder == current || own_baseline.is_some_and(|b| Path::new(&e.holder) == b)
}

/// The baseline this worktree is the only referencer of, if any. `None` when
/// the worktree names no baseline, when the path it names is not one, when
/// another worktree names the same one, or when the scan could not read every
/// worktree it had to.
///
/// A baseline nobody else names is this worktree's own, so stopping its
/// servers reaches no further than stopping the worktree's own. A shared
/// baseline stays foreign to every referencer, which keeps the terminal gate
/// over every case where stopping it affects another worktree.
fn sole_referenced_baseline(repo: &str, current: &str) -> Option<PathBuf> {
    let path = pinned_baseline(Path::new(current))?;
    let refs = crate::baseline::referencers(repo).ok()?;
    // A worktree the scan could not read — a record that does not parse, a tree
    // whose classification failed — could name this baseline, so nothing here
    // is provably unshared while one exists.
    if !refs.unreadable.is_empty() {
        return None;
    }
    (!crate::baseline::shared_with_others(&refs, &path, Path::new(current))).then_some(path)
}

/// The baseline this worktree's record names, when the path it names provably
/// is one. The record is a plain file inside the worktree, so a pin can be made
/// to name a sibling worktree; a path whose marker cannot be read names servers
/// this worktree has no claim on, exactly as one carrying no marker does.
fn pinned_baseline(worktree: &Path) -> Option<PathBuf> {
    use devkit_common::worktree::BaselineState;
    let pin = devkit_common::record::read(worktree)?.baseline?;
    let path = PathBuf::from(pin.path);
    matches!(
        devkit_common::worktree::baseline_state(&path),
        BaselineState::Yes
    )
    .then_some(path)
}

/// Whether the registry tracks anything under `holder`.
fn holds_rows(holder: &Path, data: &registry::Data) -> bool {
    !crate::baseline::rows_for_holder(&holder.to_string_lossy(), data).is_empty()
}

/// Render a status table limited to the given ports.
fn preview_table(data: &registry::Data, ports: &[u16]) -> String {
    let mut d = registry::Data::default();
    for p in ports {
        if let Some(e) = data.entries.get(p) {
            d.entries.insert(*p, e.clone());
        }
    }
    registry::status_table(&d, None)
}

/// Foreign holders among the matched rows, in first-seen order.
fn foreign_holders(matched: &[(u16, &registry::Entry)], current: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for (_, e) in matched {
        if e.holder != current && !seen.contains(&e.holder) {
            seen.push(e.holder.clone());
        }
    }
    seen
}

fn confirm(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

fn report_down(out: &run::DownOutcome) {
    if out.via_daemon {
        println!("stopped via daemon; released ports {:?}", out.freed);
    } else {
        println!(
            "stopped {} process(es); released ports {:?}",
            out.stopped, out.freed
        );
    }
}

/// The exemption is resolved and acted on under the pinned baseline's slot
/// lock, and the lock is released before any prompt. Holding it across a `y/n`
/// would put an unbounded human wait under a lock that `remove_if_unreferenced`
/// blocks on while holding the *directory* lock, stalling every prune in the
/// baseline directory; and the exemption changes nothing about the prompting
/// path, where every row is offered by holder and confirmed one by one.
///
/// The lock is what closes the window between deciding who references the
/// baseline and stopping its servers: `up` takes this same lock across
/// `baseline::ensure` and writes its record under it, so a worktree adopting
/// this baseline either appears in the scan or waits for the lock this run
/// holds.
///
/// Anything that leaves the baseline unresolved leaves the cross-worktree gate
/// exactly where it was, so a repository that cannot be located and a record
/// that names nothing both fall through to an unexempted `down`.
fn cmd_down(cwd: &str, args: &DownArgs) -> Result<()> {
    let current = toplevel(cwd)?;
    let repo = devkit_common::git::primary_checkout(Path::new(cwd)).ok();
    // A baseline holding no rows has nothing to exempt, and the wait for its
    // lock is unbounded, so the decision is skipped rather than queued behind
    // whatever holds it. Rows appearing afterwards are outside the scope this
    // then builds and stay behind the terminal gate.
    let pinned = match pinned_baseline(Path::new(&current)) {
        Some(p) if holds_rows(&p, &registry::snapshot()?) => Some(p),
        _ => None,
    };
    if let (Some(pin), Some(repo)) = (pinned, repo) {
        let stopped = crate::baseline::with_slot_lock(&pin, || {
            // The record can be rewritten between the read that named the slot
            // and the lock over it. A baseline this run holds no lock for gets
            // no exemption.
            match sole_referenced_baseline(&repo.to_string_lossy(), &current).filter(|b| *b == pin)
            {
                Some(own) => down_own(&current, args, &own),
                None => Ok(false),
            }
        })?;
        if stopped {
            return Ok(());
        }
    }
    down_selection(&current, args)
}

/// Stop the selection when the exemption covers all of it, reporting whether it
/// did. `false` means this run did nothing — the selection is empty, or it
/// reaches past this worktree and its own baseline — and hands it to
/// [`down_selection`], which reports the empty case and prompts for the other.
/// That runs outside the lock and without the exemption: a selection reaching
/// another worktree can only do so through a scope flag, whose scope the
/// exemption does not widen.
fn down_own(current: &str, args: &DownArgs, baseline: &Path) -> Result<bool> {
    let selector = build_selector(args, current, Some(baseline));
    let data = registry::snapshot()?;
    let ports = registry::select(&data, &selector, registry::now());
    let matched: Vec<(u16, &registry::Entry)> = ports
        .iter()
        .filter_map(|p| data.entries.get(p).map(|e| (*p, e)))
        .collect();
    if ports.is_empty() || touches_foreign(&matched, current, Some(baseline)) {
        return Ok(false);
    }
    let out = run::bring_down_ports(&ports)?;
    report_down(&out);
    Ok(true)
}

fn down_selection(current: &str, args: &DownArgs) -> Result<()> {
    let selector = build_selector(args, current, None);
    let data = registry::snapshot()?;
    let now = registry::now();
    let ports = registry::select(&data, &selector, now);
    if ports.is_empty() {
        println!("no tracked servers match the selection");
        return Ok(());
    }
    let matched: Vec<(u16, &registry::Entry)> = ports
        .iter()
        .filter_map(|p| data.entries.get(p).map(|e| (*p, e)))
        .collect();

    // Entirely in the current worktree: stop directly, no prompt.
    if !touches_foreign(&matched, current, None) {
        let out = run::bring_down_ports(&ports)?;
        report_down(&out);
        return Ok(());
    }

    // Foreign holders present: require an interactive terminal.
    if !std::io::stdin().is_terminal() {
        eprintln!("{}", preview_table(&data, &ports));
        anyhow::bail!("cross-worktree down requires an interactive terminal");
    }

    let batch = args.batch || args.all;
    let mut chosen: Vec<u16> = Vec::new();
    if batch {
        println!("{}", preview_table(&data, &ports));
        let holders = foreign_holders(&matched, current);
        let includes_current = matched.iter().any(|(_, e)| e.holder == current);
        if confirm(&format!(
            "Stop {} server(s) across {} worktree(s)?",
            ports.len(),
            holders.len() + usize::from(includes_current)
        )) {
            chosen = ports.clone();
        }
    } else {
        // Per-worktree prompts for foreign holders; current worktree stops silently.
        for holder in foreign_holders(&matched, current) {
            let group: Vec<u16> = matched
                .iter()
                .filter(|(_, e)| e.holder == holder)
                .map(|(p, _)| *p)
                .collect();
            println!("{}", preview_table(&data, &group));
            let label = devkit_common::paths::leaf(&holder).unwrap_or(&holder);
            if confirm(&format!("Stop {} server(s) in {label}?", group.len())) {
                chosen.extend(group);
            } else {
                println!("    skipped");
            }
        }
        for (p, e) in &matched {
            if e.holder == current {
                chosen.push(*p);
            }
        }
    }

    if chosen.is_empty() {
        println!("nothing stopped");
        return Ok(());
    }
    let out = run::bring_down_ports(&chosen)?;
    report_down(&out);
    Ok(())
}

/// Strays visible under a status scope: all of them with `--all`, else only
/// those attributed to the current worktree (or with an unknown holder).
fn strays_in_scope(
    strays: &[devkit_ports::strays::Stray],
    current: Option<&str>,
) -> Vec<devkit_ports::strays::Stray> {
    strays
        .iter()
        .filter(|s| match (current, s.holder.as_deref()) {
            (None, _) => true, // --all
            (Some(c), Some(h)) => h == c,
            (Some(_), None) => true, // port-only, unknown holder
        })
        .cloned()
        .collect()
}

/// Render the untracked-servers section, or an empty string if there are none.
fn render_strays(strays: &[devkit_ports::strays::Stray]) -> String {
    if strays.is_empty() {
        return String::new();
    }
    let mut t = ui::table(&["PORT", "APP", "PID", "HOLDER", "SOURCE", "COMMAND"]);
    for s in strays {
        let holder = s
            .holder
            .as_deref()
            .and_then(devkit_common::paths::leaf)
            .or(s.holder.as_deref())
            .unwrap_or("-");
        t.add_row(vec![
            s.port.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            s.app.clone().unwrap_or_else(|| "-".into()),
            s.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            holder.to_string(),
            s.source.as_str().to_string(),
            s.command.clone().unwrap_or_else(|| "-".into()),
        ]);
    }
    format!("untracked (outside the registry):\n{t}")
}

/// Reap refuses to do anything destructive without an interactive terminal —
/// the same gate that protects cross-worktree `down`. There is deliberately no
/// flag that bypasses this, so an agent (no PTY) can never reap.
fn reap_allowed(is_tty: bool) -> bool {
    is_tty
}

/// Roots to kill from a scoped stray set: the resolved launch-root pids.
fn reap_roots(strays: &[devkit_ports::strays::Stray]) -> Vec<u32> {
    strays.iter().filter_map(|s| s.pid).collect()
}

/// The rendered URL per port for the rows `status` is about to show.
///
/// A template may reference a sibling's port via `ports[...]`, so the lookup
/// map is built per (holder, role) group — the same grouping `up` allocates
/// under. An app absent from the catalog, or a template that fails to render,
/// simply has no URL.
fn status_urls(
    data: &registry::Data,
    only_holder: Option<&str>,
    catalog: &HashMap<String, devkit_ports::apps::App>,
    variables: &BTreeMap<String, String>,
) -> BTreeMap<u16, String> {
    let mut groups: HashMap<(&str, Role), BTreeMap<String, u16>> = HashMap::new();
    for (port, e) in &data.entries {
        if only_holder.is_some_and(|h| e.holder != h) {
            continue;
        }
        groups
            .entry((e.holder.as_str(), e.role))
            .or_default()
            .insert(e.app.clone(), *port);
    }

    let mut urls = BTreeMap::new();
    for (port, e) in &data.entries {
        if only_holder.is_some_and(|h| e.holder != h) {
            continue;
        }
        let Some(app) = catalog.get(&e.app) else {
            continue;
        };
        let Some(group_ports) = groups.get(&(e.holder.as_str(), e.role)) else {
            continue;
        };
        if let Ok(url) = devkit_common::template::render_launch(
            app.url_template(),
            Some(*port),
            group_ports,
            variables,
        ) {
            urls.insert(*port, url);
        }
    }
    urls
}

/// The rendered URL per port for every holder in scope, each resolved against
/// the config that governs that holder.
///
/// `ports.json` is machine-global, so `--all` lists rows from unrelated
/// projects where an app name like `web` collides. Rendering a foreign row
/// through the caller's own templates would produce a link to a host that does
/// not serve that port. A holder whose config will not load contributes no URLs.
fn status_urls_by_holder(
    data: &registry::Data,
    only_holder: Option<&str>,
    cwd_holder: Option<&str>,
    cwd_loaded: Option<&load::Loaded>,
) -> BTreeMap<u16, String> {
    let holders: BTreeSet<&str> = data
        .entries
        .values()
        .map(|e| e.holder.as_str())
        .filter(|h| only_holder.is_none_or(|only| *h == only))
        .collect();

    let mut urls = BTreeMap::new();
    for holder in holders {
        if cwd_holder.is_some_and(|h| h == holder) {
            if let Some(l) = cwd_loaded {
                urls.extend(status_urls(
                    data,
                    Some(holder),
                    &l.catalog,
                    &l.config.templates.variables,
                ));
            }
            continue;
        }
        // Another worktree's config: its unresolvable apps are that project's
        // business, not a note to print on this terminal.
        let Ok(l) = load::load_quiet(None, Path::new(holder)) else {
            continue;
        };
        urls.extend(status_urls(
            data,
            Some(holder),
            &l.catalog,
            &l.config.templates.variables,
        ));
    }
    urls
}

fn cmd_status(cwd: &str, all: bool) -> Result<()> {
    let data = registry::snapshot()?;
    // `here` is which worktree the caller is in; `current` is which rows to
    // show. `--all` widens the display without changing where the caller is,
    // and the already-loaded config belongs to `here`.
    let here = toplevel(cwd).ok();
    let current = if all { None } else { here.clone() };
    let loaded = load::load(None, Path::new(cwd));
    match (&current, all) {
        (Some(h), _) => {
            let urls = status_urls_by_holder(&data, Some(h), here.as_deref(), loaded.as_ref().ok());
            println!(
                "{}",
                registry::status_table_linked(
                    &data,
                    Some(h),
                    &registry::listening_view(&data, Some(h)),
                    &urls
                )
            )
        }
        (None, true) => {
            let urls = status_urls_by_holder(&data, None, here.as_deref(), loaded.as_ref().ok());
            println!(
                "{}",
                registry::status_table_linked(
                    &data,
                    None,
                    &registry::listening_view(&data, None),
                    &urls
                )
            )
        }
        (None, false) => println!(
            "{}",
            registry::status_table(&registry::Data::default(), None)
        ),
    }
    // Untracked strays (best-effort; never fails status).
    if let Ok(loaded) = &loaded {
        let strays = devkit_ports::strays::scan(&loaded.config, &data);
        let scoped = strays_in_scope(&strays, current.as_deref());
        let rendered = render_strays(&scoped);
        if !rendered.is_empty() {
            println!("\n{rendered}");
        }
    }
    Ok(())
}

fn cmd_reap(cwd: &str, all: bool) -> Result<()> {
    let data = registry::snapshot()?;
    let loaded = load::load(None, Path::new(cwd))?;
    let current = if all { None } else { Some(toplevel(cwd)?) };
    let strays = devkit_ports::strays::scan(&loaded.config, &data);
    let scoped = strays_in_scope(&strays, current.as_deref());
    if scoped.is_empty() {
        println!("no stray servers found");
        return Ok(());
    }
    println!("{}", render_strays(&scoped));

    if !reap_allowed(std::io::stdin().is_terminal()) {
        anyhow::bail!("reap requires an interactive terminal");
    }
    let roots = reap_roots(&scoped);
    if roots.is_empty() {
        println!("no killable strays (port-only, no resolved pid) — investigate manually");
        return Ok(());
    }
    if !confirm(&format!("Kill {} stray server(s)?", roots.len())) {
        println!("nothing killed");
        return Ok(());
    }
    let procs = devkit_ports::strays::proc_table();
    let n = devkit_ports::strays::os::kill_tree(&roots, &procs);
    println!("killed {n} process(es)");
    Ok(())
}

fn cmd_logs(cwd: &str, app: &str, role: Option<Role>, follow: bool) -> Result<()> {
    let holder = toplevel(cwd)?;
    if follow {
        let data = registry::snapshot()?;
        let log = data
            .entries
            .values()
            .find(|e| e.holder == holder && e.app == app && role.is_none_or(|r| e.role == r))
            .and_then(|e| e.logfile.clone())
            .ok_or_else(|| anyhow::anyhow!("no tracked log for app `{app}` in this worktree"))?;
        let status = std::process::Command::new("tail")
            .arg("-f")
            .arg(&log)
            .status()
            .with_context(|| "running `tail -f`")?;
        // `tail -f` blocks until interrupted; exit with its status directly.
        // This bypasses the timing guard's Drop, but a --follow session is a
        // single snapshot then an indefinite block, not a timing target.
        std::process::exit(status.code().unwrap_or(1));
    }
    println!("{}", run::read_log(&holder, app, role, 200)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{apps_from_diff, available_apps};
    use devkit_ports::registry;
    use std::collections::{BTreeMap, HashMap};
    use std::path::{Path, PathBuf};

    #[test]
    fn reap_refused_without_tty() {
        use super::reap_allowed;
        assert!(!reap_allowed(false));
        assert!(reap_allowed(true));
    }

    #[test]
    fn reap_roots_are_resolved_pids_only() {
        use super::reap_roots;
        use devkit_ports::strays::{Source, Stray};
        let mk = |port: u16, pid: Option<u32>| Stray {
            port: Some(port),
            pid,
            holder: Some("/w1".into()),
            app: Some("api".into()),
            command: Some("doppler run -- bun nitro dev".into()),
            source: Source::ProcessPattern,
        };
        // s1 has a pid, s2 is port-only (no pid).
        let roots = reap_roots(&[mk(9200, Some(1)), mk(9201, None)]);
        assert_eq!(roots, vec![1]);
        let roots2 = reap_roots(&[mk(9202, Some(42))]);
        assert_eq!(roots2, vec![42]);
    }

    #[test]
    fn render_strays_names_source_in_snake_case() {
        use super::render_strays;
        use devkit_ports::strays::{Source, Stray};
        let out = render_strays(&[Stray {
            port: Some(9200),
            pid: Some(1),
            holder: Some("/w1".into()),
            app: Some("api".into()),
            command: Some("doppler run -- bun nitro dev".into()),
            source: Source::ProcessPattern,
        }]);
        assert!(out.contains("process_pattern"));
        assert!(!out.contains("processpattern"));
    }

    #[test]
    fn scope_all_shows_every_stray() {
        use super::strays_in_scope;
        use devkit_ports::strays::{Source, Stray};
        let stray = |port: u16, holder: Option<&str>| Stray {
            port: Some(port),
            pid: Some(1),
            holder: holder.map(String::from),
            app: Some("api".into()),
            command: Some("doppler run -- bun nitro dev".into()),
            source: Source::ProcessPattern,
        };
        let strays = vec![stray(9200, Some("/w1")), stray(9201, Some("/w2"))];
        assert_eq!(strays_in_scope(&strays, None).len(), 2);
    }

    #[test]
    fn scope_current_filters_to_this_worktree_plus_unknown() {
        use super::strays_in_scope;
        use devkit_ports::strays::{Source, Stray};
        let stray = |port: u16, holder: Option<&str>| Stray {
            port: Some(port),
            pid: Some(1),
            holder: holder.map(String::from),
            app: Some("api".into()),
            command: Some("doppler run -- bun nitro dev".into()),
            source: Source::ProcessPattern,
        };
        let strays = vec![
            stray(9200, Some("/w1")),
            stray(9201, Some("/w2")),
            stray(9202, None),
        ];
        let scoped = strays_in_scope(&strays, Some("/w1"));
        assert_eq!(scoped.len(), 2); // /w1 + the unknown-holder one
        assert!(scoped.iter().all(|s| s.holder.as_deref() != Some("/w2")));
    }

    #[test]
    fn parse_age_handles_units() {
        use super::parse_age;
        assert_eq!(parse_age("90s").unwrap(), 90);
        assert_eq!(parse_age("30m").unwrap(), 1800);
        assert_eq!(parse_age("2h").unwrap(), 7200);
        assert_eq!(parse_age("1d").unwrap(), 86400);
        assert_eq!(parse_age("45").unwrap(), 45, "bare number is seconds");
        assert!(parse_age("nope").is_err());
    }

    /// The unit changes at each boundary and the division truncates, so a tree
    /// just short of the next unit must not round up into it.
    #[test]
    fn human_size_changes_unit_at_each_boundary() {
        use super::human_size;
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1 KiB");
        assert_eq!(human_size(1024 * 1024 - 1), "1023 KiB");
        assert_eq!(human_size(1024 * 1024), "1 MiB");
        assert_eq!(human_size(2 * 1024 * 1024 - 1), "1 MiB");
    }

    #[test]
    fn build_selector_maps_scope_and_filter() {
        use super::{DownArgs, build_selector};
        use devkit_ports::registry::{Filter, Scope};

        // Default: current worktree, no filter.
        let a = DownArgs::default();
        let s = build_selector(&a, "/wt/cur", None);
        assert!(matches!(s.scope, Scope::Current(ref h) if h == "/wt/cur"));
        assert!(matches!(s.filter, Filter::All));

        // --all + positional token.
        let a = DownArgs {
            all: true,
            selectors: vec!["api".into()],
            ..Default::default()
        };
        let s = build_selector(&a, "/wt/cur", None);
        assert!(matches!(s.scope, Scope::All));
        assert!(matches!(s.filter, Filter::Tokens(ref t) if t == &vec!["api".to_string()]));

        // --others + column filter.
        let a = DownArgs {
            others: true,
            app: vec!["web".into()],
            ..Default::default()
        };
        let s = build_selector(&a, "/wt/cur", None);
        assert!(matches!(s.scope, Scope::Others(ref h) if h == "/wt/cur"));
        match s.filter {
            Filter::Columns(c) => assert_eq!(c.app, vec!["web".to_string()]),
            _ => panic!("expected Columns filter"),
        }
    }

    fn entry(holder: &str, role: registry::Role) -> registry::Entry {
        registry::Entry {
            app: "api".into(),
            holder: holder.into(),
            role,
            pid: Some(1),
            logfile: None,
            ts: 0,
        }
    }

    #[test]
    fn touches_foreign_detects_other_holders() {
        use super::touches_foreign;
        let cur = entry("/wt/cur", registry::Role::Issue);
        let other = entry("/wt/other", registry::Role::Issue);
        assert!(!touches_foreign(&[(1, &cur)], "/wt/cur", None));
        assert!(touches_foreign(&[(1, &cur), (2, &other)], "/wt/cur", None));
    }

    #[test]
    fn a_sole_referenced_baseline_is_not_foreign() {
        use super::touches_foreign;
        let bl = std::path::PathBuf::from("/b/d13d90b724bf");
        let e = entry("/b/d13d90b724bf", registry::Role::Baseline);
        let matched = vec![(3000u16, &e)];
        assert!(
            touches_foreign(&matched, "/wt/cur", None),
            "no baseline: foreign"
        );
        assert!(
            !touches_foreign(&matched, "/wt/cur", Some(&bl)),
            "sole referencer stops its own baseline without a terminal"
        );
    }

    #[test]
    fn another_worktrees_baseline_stays_foreign() {
        use super::touches_foreign;
        let mine = std::path::PathBuf::from("/b/d13d90b724bf");
        let e = entry("/b/0123456789ab", registry::Role::Baseline);
        let matched = vec![(3000u16, &e)];
        assert!(touches_foreign(&matched, "/wt/cur", Some(&mine)));
    }

    #[test]
    fn the_default_scope_covers_the_worktree_and_its_own_baseline() {
        use super::{DownArgs, build_selector};
        let bl = std::path::PathBuf::from("/b/d13d90b724bf");
        let a = DownArgs::default();
        let s = build_selector(&a, "/wt/cur", Some(&bl));
        match s.scope {
            registry::Scope::Holders(hs) => {
                assert!(hs.iter().any(|h| h == "/wt/cur"));
                assert!(hs.iter().any(|h| h == "/b/d13d90b724bf"));
            }
            other => panic!("expected Holders, got {other:?}"),
        }
    }

    #[test]
    fn no_baseline_leaves_the_default_scope_current() {
        use super::{DownArgs, build_selector};
        let a = DownArgs::default();
        let s = build_selector(&a, "/wt/cur", None);
        assert!(matches!(s.scope, registry::Scope::Current(ref h) if h == "/wt/cur"));
    }

    /// A primary checkout, a baseline directory carrying the marker that makes
    /// it one, and the worktrees `a` and `b` whose records both name it.
    struct Pins {
        _tmp: tempfile::TempDir,
        repo: String,
        baseline: PathBuf,
        a: PathBuf,
        b: PathBuf,
    }

    fn fixture_git(cwd: &Path, args: &[&str]) -> String {
        devkit_common::git::Git::fixture(cwd)
            .args(args.iter().copied())
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"))
    }

    fn mark_baseline(dir: &Path) {
        std::fs::create_dir_all(dir.join(".devkit")).unwrap();
        std::fs::write(
            dir.join(".devkit").join("baseline.toml"),
            "sha = \"d13d90b\"\n",
        )
        .unwrap();
    }

    fn pin(worktree: &Path, baseline: &Path) {
        devkit_common::record::write(
            worktree,
            &devkit_common::record::IssueRecord {
                issue: "i-1".into(),
                slug: "i-1".into(),
                apps: vec![],
                summary: None,
                pr: None,
                baseline: Some(devkit_common::record::BaselinePin {
                    sha: "d13d90b724bf8a3c".into(),
                    path: baseline.to_string_lossy().into_owned(),
                }),
            },
        )
        .unwrap();
    }

    fn unpin(worktree: &Path) {
        std::fs::remove_dir_all(worktree.join(".devkit")).unwrap();
    }

    /// Leave a directory's baseline marker unresolvable — a symlink loop here,
    /// a permission failure in the field — so it can be neither read nor ruled
    /// out.
    #[cfg(unix)]
    fn make_unclassifiable(dir: &Path) {
        let marker = dir.join(".devkit").join("baseline.toml");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(&marker);
        std::os::unix::fs::symlink(&marker, &marker).unwrap();
    }

    fn two_worktrees_naming_one_baseline() -> Pins {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("main");
        std::fs::create_dir_all(&repo).unwrap();
        fixture_git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("f"), "x").unwrap();
        fixture_git(&repo, &["add", "."]);
        fixture_git(&repo, &["commit", "-qm", "one"]);

        let baseline = tmp.path().join("_baselines").join("d13d90b724bf");
        mark_baseline(&baseline);

        let make = |name: &str| {
            let wt = tmp.path().join(name);
            fixture_git(
                &repo,
                &["worktree", "add", "-b", name, wt.to_str().unwrap()],
            );
            pin(&wt, &baseline);
            wt
        };
        let a = make("a");
        let b = make("b");
        Pins {
            _tmp: tmp,
            repo: repo.to_string_lossy().into_owned(),
            baseline,
            a,
            b,
        }
    }

    fn sole(f: &Pins, current: &Path) -> Option<PathBuf> {
        super::sole_referenced_baseline(&f.repo, current.to_str().unwrap())
    }

    #[test]
    fn a_baseline_only_this_worktree_names_is_its_own() {
        let f = two_worktrees_naming_one_baseline();
        unpin(&f.b);
        assert_eq!(sole(&f, &f.a), Some(f.baseline.clone()));
    }

    #[test]
    fn a_shared_baseline_is_nobodys_own() {
        let f = two_worktrees_naming_one_baseline();
        assert_eq!(sole(&f, &f.a), None, "b names it too");
        assert_eq!(sole(&f, &f.b), None);
    }

    /// A record that does not parse could name this baseline, so a scan that
    /// cannot read one proves nothing about who else references it.
    #[test]
    fn an_unreadable_record_leaves_the_baseline_foreign() {
        let f = two_worktrees_naming_one_baseline();
        std::fs::write(f.b.join(".devkit").join("issue.toml"), "issue = ").unwrap();
        assert_eq!(sole(&f, &f.a), None);
    }

    /// The record is a plain file inside the worktree, so a pin can be made to
    /// name a sibling worktree. Exempting whatever it names would let a session
    /// with no terminal stop another worktree's servers.
    #[test]
    fn a_pin_aimed_at_something_that_is_not_a_baseline_is_refused() {
        let f = two_worktrees_naming_one_baseline();
        pin(&f.a, &f.b);
        assert_eq!(sole(&f, &f.a), None);
    }

    /// Being the only worktree that names a baseline is not the same as being
    /// that worktree. A caller git does not list among the repository's
    /// worktrees must not inherit the claim of the one that does.
    #[test]
    fn a_baseline_only_someone_else_names_is_not_the_callers() {
        let f = two_worktrees_naming_one_baseline();
        unpin(&f.b);
        let outside = f.a.parent().unwrap().join("outside");
        pin(&outside, &f.baseline);
        assert_eq!(sole(&f, &outside), None);
    }

    #[test]
    fn a_worktree_that_names_no_baseline_has_none() {
        let f = two_worktrees_naming_one_baseline();
        unpin(&f.a);
        assert_eq!(sole(&f, &f.a), None);
    }

    /// Worktree discovery folds a tree it cannot classify in with the
    /// baselines, so a sibling whose marker is unreadable is dropped before its
    /// record is ever read — and a reference nobody can see would otherwise
    /// read as no reference at all.
    #[cfg(unix)]
    #[test]
    fn a_sibling_that_cannot_be_classified_leaves_the_baseline_foreign() {
        let f = two_worktrees_naming_one_baseline();
        make_unclassifiable(&f.b);
        assert_eq!(sole(&f, &f.a), None, "b's record still names the baseline");
    }

    /// Each record spells the baseline as whatever resolved it for that
    /// worktree, so one directory can occupy two keys in the scan. A sibling
    /// that reaches it through a symlinked parent is still a referencer, and
    /// missing it grants the exemption over a shared baseline.
    #[cfg(unix)]
    #[test]
    fn a_sibling_naming_the_baseline_another_way_keeps_it_shared() {
        let f = two_worktrees_naming_one_baseline();
        let tmp = f.a.parent().unwrap();
        let link = tmp.join("link");
        std::os::unix::fs::symlink(tmp, &link).unwrap();
        let spelled = link.join(f.baseline.strip_prefix(tmp).unwrap());
        pin(&f.b, &spelled);

        assert_eq!(sole(&f, &f.a), None, "b still references the baseline");
    }

    /// The pin names the directory whose servers the exemption would stop, so
    /// a marker that cannot be read there is no proof of anything.
    #[cfg(unix)]
    #[test]
    fn a_pin_whose_marker_cannot_be_read_is_refused() {
        let f = two_worktrees_naming_one_baseline();
        unpin(&f.b);
        make_unclassifiable(&f.baseline);
        assert_eq!(sole(&f, &f.a), None);
    }

    #[test]
    fn picks_known_apps_from_diff() {
        let diff =
            " apps/api/server/x.ts | 2 +-\n apps/lab-os/page.tsx | 1 +\n packages/z/y.ts | 1 +\n";
        let known = vec![
            "api".to_string(),
            "lab-os".to_string(),
            "foundry-portal".to_string(),
        ];
        assert_eq!(apps_from_diff(diff, &known, "apps"), vec!["api", "lab-os"]);
    }

    #[test]
    fn available_apps_sorts_the_names() {
        let known = ["web".to_string(), "api".to_string(), "docs".to_string()];
        assert_eq!(available_apps(&known), "available apps: api, docs, web");
    }

    #[test]
    fn available_apps_says_so_when_none_are_configured() {
        assert_eq!(
            available_apps(&[]),
            "no apps configured (add [apps.<name>] to devkit.toml)"
        );
    }

    fn status_urls_app(url: Option<&str>) -> devkit_ports::apps::App {
        devkit_ports::apps::App {
            name: "app".into(),
            base_port: 0,
            path: ".".into(),
            launch: vec![],
            url: url.map(String::from),
            url_env: None,
            provides_url: false,
            static_env: HashMap::new(),
            prep_files: vec![],
            setup: vec![],
        }
    }

    fn status_urls_entry(
        app: &str,
        holder: &str,
        role: devkit_ports::registry::Role,
    ) -> devkit_ports::registry::Entry {
        devkit_ports::registry::Entry {
            app: app.into(),
            holder: holder.into(),
            role,
            pid: None,
            logfile: None,
            ts: 0,
        }
    }

    #[test]
    fn status_urls_uses_the_localhost_default_for_an_app_with_no_configured_url() {
        use super::status_urls;
        let mut data = devkit_ports::registry::Data::default();
        data.entries.insert(
            4100,
            status_urls_entry("api", "/wt", devkit_ports::registry::Role::Issue),
        );
        let mut catalog = HashMap::new();
        catalog.insert("api".to_string(), status_urls_app(None));

        let urls = status_urls(&data, None, &catalog, &BTreeMap::new());
        assert_eq!(
            urls.get(&4100).map(String::as_str),
            Some("http://localhost:4100")
        );
    }

    #[test]
    fn status_urls_resolves_a_sibling_reference_within_its_own_group() {
        use super::status_urls;
        let mut data = devkit_ports::registry::Data::default();
        // Group 1: front on 4100, peer references front's port.
        data.entries.insert(
            4100,
            status_urls_entry("front", "/wt1", devkit_ports::registry::Role::Issue),
        );
        data.entries.insert(
            4101,
            status_urls_entry("peer", "/wt1", devkit_ports::registry::Role::Issue),
        );
        // Group 2: front on a different port, so a peer resolving the wrong
        // group's port would give a different answer.
        data.entries.insert(
            4200,
            status_urls_entry("front", "/wt2", devkit_ports::registry::Role::Issue),
        );
        data.entries.insert(
            4201,
            status_urls_entry("peer", "/wt2", devkit_ports::registry::Role::Issue),
        );

        let mut catalog = HashMap::new();
        catalog.insert("front".to_string(), status_urls_app(None));
        catalog.insert(
            "peer".to_string(),
            status_urls_app(Some("http://localhost:{{ ports['front'] }}/peer")),
        );

        let urls = status_urls(&data, None, &catalog, &BTreeMap::new());
        assert_eq!(
            urls.get(&4101).map(String::as_str),
            Some("http://localhost:4100/peer")
        );
        assert_eq!(
            urls.get(&4201).map(String::as_str),
            Some("http://localhost:4200/peer")
        );
    }

    #[test]
    fn status_urls_skips_a_row_whose_app_is_not_in_the_catalog() {
        use super::status_urls;
        let mut data = devkit_ports::registry::Data::default();
        data.entries.insert(
            4100,
            status_urls_entry("ghost", "/wt", devkit_ports::registry::Role::Issue),
        );

        let urls = status_urls(&data, None, &HashMap::new(), &BTreeMap::new());
        assert!(urls.is_empty());
    }

    #[test]
    fn status_urls_scopes_to_only_holder() {
        use super::status_urls;
        let mut data = devkit_ports::registry::Data::default();
        data.entries.insert(
            4100,
            status_urls_entry("api", "/wt1", devkit_ports::registry::Role::Issue),
        );
        data.entries.insert(
            4200,
            status_urls_entry("api", "/wt2", devkit_ports::registry::Role::Issue),
        );
        let mut catalog = HashMap::new();
        catalog.insert("api".to_string(), status_urls_app(None));

        let urls = status_urls(&data, Some("/wt1"), &catalog, &BTreeMap::new());
        assert_eq!(urls.len(), 1);
        assert!(urls.contains_key(&4100));
        assert!(!urls.contains_key(&4200));
    }
}
