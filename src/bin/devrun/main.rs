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
    /// Stop servers and release ports for this worktree.
    Down {
        #[arg(long, value_enum)]
        role: Option<RoleSelector>,
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
        Cmd::Down { role } => cmd_down(&cwd, role.and_then(RoleSelector::filter)),
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

fn cmd_down(cwd: &str, role: Option<Role>) -> Result<()> {
    let holder = toplevel(cwd)?;
    let out = run::bring_down(&holder, role)?;
    if out.via_daemon {
        println!("stopped via daemon; released ports {:?}", out.freed);
    } else {
        println!(
            "stopped {} process(es); released ports {:?}",
            out.stopped, out.freed
        );
    }
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
