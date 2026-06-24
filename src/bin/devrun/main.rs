mod baseline;
mod config;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use devkit_common::supervise;
use devkit_common::{cmd::git, ui};
use devkit_ports::config::expand_tilde;
use devkit_ports::load;
use devkit_ports::registry::{self, Role};
use devkit_ports::run;
use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(about = "Run local dev servers for an issue worktree (with optional baseline A/B)")]
struct Cli {
    #[arg(short = 'C', long = "dir", global = true)]
    dir: Option<String>,
    #[arg(long, global = true)]
    config: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Bring up dev servers for the selected apps.
    Up {
        apps: Vec<String>,
        #[arg(long, value_enum, default_value = "issue")]
        role: RoleSelector,
        #[arg(long = "env", value_name = "K=V")]
        env: Vec<String>,
        #[arg(long = "env-file")]
        env_file: Option<String>,
        #[arg(long)]
        dry_run: bool,
        /// Hand servers to the supervisor daemon (autostarting it) so they restart on crash.
        #[arg(long)]
        supervise: bool,
    },
    /// Stop servers and release ports. Defaults to this worktree; reaching another
    /// worktree needs --all/--others/--holder and prompts (requires a terminal).
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
        #[arg(long)]
        all: bool,
    },
    /// Print (or follow) the log for one app.
    Logs {
        app: String,
        #[arg(long, value_enum)]
        role: Option<RoleSelector>,
        #[arg(short = 'f', long)]
        follow: bool,
    },
    /// Show the effective merged config, or list configured apps.
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Print a shell-completion script (bash, zsh, fish, …) to stdout.
    Completions { shell: Shell },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Print the effective merged config (TOML by default).
    Show {
        /// Annotate each value with the file it was resolved from.
        #[arg(long)]
        origin: bool,
        /// Emit JSON instead of TOML.
        #[arg(long)]
        json: bool,
    },
    /// List the configured apps from the merged config.
    Apps {
        /// Emit JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
}

/// CLI selector over registry roles. `Both` runs/affects the issue branch and a
/// fresh baseline side-by-side; it is not itself a registry `Role`.
#[derive(Clone, Copy, ValueEnum, PartialEq)]
enum RoleSelector {
    Issue,
    Baseline,
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

fn cwd_of(cli: &Cli) -> String {
    cli.dir.clone().unwrap_or_else(|| ".".to_string())
}

fn toplevel(cwd: &str) -> Result<String> {
    Ok(git(&["rev-parse", "--show-toplevel"], cwd)?
        .trim()
        .to_string())
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
fn build_selector(a: &DownArgs, current: &str) -> registry::DownSelector {
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
    pid: Option<u32>,
    log: PathBuf,
    ready: Option<bool>,
}

fn print_summary(rows: &[Row]) {
    let mut t = ui::table(&["ROLE", "APP", "PORT", "URL", "PID", "READY", "LOG"]);
    for r in rows {
        let url = format!("http://localhost:{}", r.port);
        t.add_row(vec![
            r.role.to_string(),
            r.app.clone(),
            r.port.to_string(),
            ui::link(&url, &url),
            r.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            match r.ready {
                Some(true) => "yes".into(),
                Some(false) => "NO".into(),
                None => "-".into(),
            },
            r.log.display().to_string(),
        ]);
    }
    println!("{t}");
}

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("devrun");
    devkit_common::paths::migrate_legacy_state();
    let cli = Cli::parse();
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
        Cmd::Logs { app, role, follow } => {
            cmd_logs(&cwd, app, role.and_then(RoleSelector::filter), *follow)
        }
        Cmd::Config { cmd } => match cmd {
            ConfigCmd::Show { origin, json } => config::show(&cli, &cwd, *origin, *json),
            ConfigCmd::Apps { json } => config::apps(&cli, &cwd, *json),
        },
        Cmd::Completions { shell } => {
            clap_complete::generate(
                *shell,
                &mut Cli::command(),
                "devrun",
                &mut std::io::stdout(),
            );
            Ok(())
        }
    }
}

fn cmd_up(
    cli: &Cli,
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

    let mut apps: Vec<String> = if !apps_arg.is_empty() {
        apps_arg.to_vec()
    } else {
        let diff = git(
            &[
                "diff",
                &format!("{}...HEAD", cfg.defaults.baseline_ref),
                "--stat",
            ],
            cwd,
        )
        .unwrap_or_default();
        apps_from_diff(&diff, &known, &cfg.defaults.apps_dir)
    };
    for a in &apps {
        anyhow::ensure!(catalog.contains_key(a), "unknown app `{a}`");
    }
    anyhow::ensure!(
        !apps.is_empty(),
        "no apps to run (none given and none detected in diff vs {})",
        cfg.defaults.baseline_ref
    );
    run::ensure_provider(catalog, &mut apps);

    let user = parse_user_env(env_pairs, env_file)?;
    let issue_holder = toplevel(cwd)?;

    // (role, holder, base_dir) — base_dir is where <app.path> is rooted.
    let groups: Vec<(Role, String, PathBuf)> = {
        let baseline_path = expand_tilde(&cfg.defaults.baseline_path);
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
                    let bp = baseline_path
                        .to_str()
                        .context("baseline_path not UTF-8")?
                        .to_string();
                    baseline::ensure_fresh(&issue_holder, &bp, &cfg.defaults.baseline_ref)?;
                    g.push((Role::Baseline, bp.clone(), baseline_path.clone()));
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
        let reqs: Vec<(String, u16)> = apps
            .iter()
            .map(|a| (a.clone(), catalog[a].base_port))
            .collect();
        let ports: BTreeMap<String, u16> = registry::alloc(holder, &reqs, *grp_role)?
            .into_iter()
            .collect();
        let plans = run::plan_group(
            catalog,
            &apps,
            &ports,
            provider.as_deref(),
            base_dir,
            *grp_role,
            &user,
        );

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
                    pid: None,
                    log: p.log.clone(),
                    ready: None,
                });
            }
            continue;
        }

        let statuses = run::launch(&plans, holder, *grp_role, supervise, true)?;
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

/// True if any matched row belongs to a holder other than `current`.
fn touches_foreign(matched: &[(u16, &registry::Entry)], current: &str) -> bool {
    matched.iter().any(|(_, e)| e.holder != current)
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

fn cmd_down(cwd: &str, args: &DownArgs) -> Result<()> {
    let current = toplevel(cwd)?;
    let selector = build_selector(args, &current);
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
    if !touches_foreign(&matched, &current) {
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
        let holders = foreign_holders(&matched, &current);
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
        for holder in foreign_holders(&matched, &current) {
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

fn cmd_status(cwd: &str, all: bool) -> Result<()> {
    let data = registry::snapshot()?;
    if all {
        println!("{}", registry::status_table(&data, None));
    } else {
        // Outside a git repo there's no worktree to scope to; show nothing.
        match toplevel(cwd).ok() {
            Some(h) => println!("{}", registry::status_table(&data, Some(&h))),
            None => println!(
                "{}",
                registry::status_table(&registry::Data::default(), None)
            ),
        }
    }
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
        std::process::exit(status.code().unwrap_or(1));
    }
    println!("{}", run::read_log(&holder, app, role, 200)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::apps_from_diff;

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

    #[test]
    fn build_selector_maps_scope_and_filter() {
        use super::{DownArgs, build_selector};
        use devkit_ports::registry::{Filter, Scope};

        // Default: current worktree, no filter.
        let a = DownArgs::default();
        let s = build_selector(&a, "/wt/cur");
        assert!(matches!(s.scope, Scope::Current(ref h) if h == "/wt/cur"));
        assert!(matches!(s.filter, Filter::All));

        // --all + positional token.
        let a = DownArgs {
            all: true,
            selectors: vec!["api".into()],
            ..Default::default()
        };
        let s = build_selector(&a, "/wt/cur");
        assert!(matches!(s.scope, Scope::All));
        assert!(matches!(s.filter, Filter::Tokens(ref t) if t == &vec!["api".to_string()]));

        // --others + column filter.
        let a = DownArgs {
            others: true,
            app: vec!["web".into()],
            ..Default::default()
        };
        let s = build_selector(&a, "/wt/cur");
        assert!(matches!(s.scope, Scope::Others(ref h) if h == "/wt/cur"));
        match s.filter {
            Filter::Columns(c) => assert_eq!(c.app, vec!["web".to_string()]),
            _ => panic!("expected Columns filter"),
        }
    }

    #[test]
    fn touches_foreign_detects_other_holders() {
        use super::touches_foreign;
        use devkit_ports::registry::{Entry, Role};
        let e = |holder: &str| Entry {
            app: "api".into(),
            holder: holder.into(),
            role: Role::Issue,
            pid: None,
            logfile: None,
            ts: 0,
        };
        let cur = e("/wt/cur");
        let other = e("/wt/other");
        assert!(!touches_foreign(&[(1, &cur)], "/wt/cur"));
        assert!(touches_foreign(&[(1, &cur), (2, &other)], "/wt/cur"));
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
}
