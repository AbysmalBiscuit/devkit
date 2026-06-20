mod env;
mod baseline;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use devkit_common::{cmd::git, paths, ui};
use devkit_common::supervise;
use devkit_ports::config::expand_tilde;
use devkit_ports::load;
use devkit_ports::registry::{self, Role};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    /// Print a shell-completion script (bash, zsh, fish, …) to stdout.
    Completions { shell: Shell },
}

/// CLI selector over registry roles. `Both` runs/affects the issue branch and a
/// fresh baseline side-by-side; it is not itself a registry `Role`.
#[derive(Clone, Copy, ValueEnum, PartialEq)]
enum RoleSelector { Issue, Baseline, Both }

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
    Ok(git(&["rev-parse", "--show-toplevel"], cwd)?.trim().to_string())
}

fn slug(holder: &str) -> String {
    Path::new(holder).file_name().and_then(|s| s.to_str()).unwrap_or("wt").to_string()
}

/// Pick known apps whose files appear in a `git diff --stat` against the baseline.
pub fn apps_from_diff(diff_stat: &str, known: &[String], apps_dir: &str) -> Vec<String> {
    let prefix = format!("{apps_dir}/");
    let mut found = Vec::new();
    for line in diff_stat.lines() {
        if let Some(rest) = line.trim().strip_prefix(&prefix)
            && let Some(name) = rest.split('/').next()
                && known.iter().any(|k| k == name) && !found.contains(&name.to_string()) {
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
            if line.is_empty() || line.starts_with('#') { continue; }
            if let Some((k, v)) = line.split_once('=') {
                m.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    for p in pairs {
        let (k, v) = p.split_once('=').with_context(|| format!("--env must be K=V, got `{p}`"))?;
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
            match r.ready { Some(true) => "yes".into(), Some(false) => "NO".into(), None => "-".into() },
            r.log.display().to_string(),
        ]);
    }
    println!("{t}");
}

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("devrun");
    let cli = Cli::parse();
    let cwd = cwd_of(&cli);
    match &cli.cmd {
        Cmd::Up { apps, role, env, env_file, dry_run, supervise } =>
            cmd_up(&cli, &cwd, apps, *role, env, env_file.as_deref(),
                   UpFlags { dry_run: *dry_run, supervise: *supervise }),
        Cmd::Down { role } => cmd_down(&cwd, role.and_then(RoleSelector::filter)),
        Cmd::Status { all } => cmd_status(&cwd, *all),
        Cmd::Logs { app, role, follow } => cmd_logs(&cwd, app, role.and_then(RoleSelector::filter), *follow),
        Cmd::Completions { shell } => {
            clap_complete::generate(*shell, &mut Cli::command(), "devrun", &mut std::io::stdout());
            Ok(())
        }
    }
}

fn cmd_up(
    cli: &Cli, cwd: &str, apps_arg: &[String], role: RoleSelector,
    env_pairs: &[String], env_file: Option<&str>, flags: UpFlags,
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
        let diff = git(&["diff", &format!("{}...HEAD", cfg.defaults.baseline_ref), "--stat"], cwd)
            .unwrap_or_default();
        apps_from_diff(&diff, &known, &cfg.defaults.apps_dir)
    };
    for a in &apps { anyhow::ensure!(catalog.contains_key(a), "unknown app `{a}`"); }
    anyhow::ensure!(
        !apps.is_empty(),
        "no apps to run (none given and none detected in diff vs {})",
        cfg.defaults.baseline_ref
    );
    // Ensure the URL-providing app (the API) is present whenever a consumer is
    // selected, so it can be wired. The provider is identified by config, not by name.
    let provider = catalog.iter().find(|(_, a)| a.provides_url).map(|(n, _)| n.clone());
    let needs_provider = apps.iter().any(|a| catalog[a].url_env.is_some() && !catalog[a].provides_url);
    if needs_provider && let Some(p) = &provider && !apps.contains(p) {
        apps.insert(0, p.clone());
    }

    let user = parse_user_env(env_pairs, env_file)?;
    let issue_holder = toplevel(cwd)?;

    // (role, holder, base_dir) — base_dir is where <app.path> is rooted.
    let groups: Vec<(Role, String, PathBuf)> = {
        let baseline_path = expand_tilde(&cfg.defaults.baseline_path);
        let mut g = Vec::new();
        for r in role.roles() {
            match r {
                Role::Issue => {
                    g.push((Role::Issue, issue_holder.clone(), PathBuf::from(&issue_holder)));
                }
                Role::Baseline => {
                    let bp = baseline_path.to_str().context("baseline_path not UTF-8")?.to_string();
                    baseline::ensure_fresh(&issue_holder, &bp, &cfg.defaults.baseline_ref)?;
                    g.push((Role::Baseline, bp.clone(), baseline_path.clone()));
                }
            }
        }
        g
    };

    let mut rows: Vec<Row> = Vec::new();
    for (grp_role, holder, base_dir) in &groups {
        let reqs: Vec<(String, u16)> =
            apps.iter().map(|a| (a.clone(), catalog[a].base_port)).collect();
        let ports: BTreeMap<String, u16> =
            registry::alloc(holder, &reqs, *grp_role)?.into_iter().collect();
        let provider_port = provider.as_ref().and_then(|p| ports.get(p).copied());

        // Build each app's launch plan up front so dry-run and real spawns share it.
        let mut plans = Vec::with_capacity(apps.len());
        for a in &apps {
            let app = &catalog[a];
            let port = ports[a];
            let mut argv = env::doppler_prefix(app, &cfg.defaults.doppler_config);
            argv.extend(env::launch_argv(app, port));
            let app_cwd = base_dir.join(&app.path);
            let envmap = env::env_for(app, provider_port, &user);
            let log = paths::logs_dir()
                .join(slug(holder))
                .join(format!("{}-{}.log", grp_role.as_str(), a));
            plans.push((a.clone(), port, argv, app_cwd, envmap, log));
        }

        if dry_run {
            for (a, port, argv, app_cwd, envmap, log) in &plans {
                println!("[{}] {a} :{port}", grp_role.as_str());
                println!("  cwd:  {}", app_cwd.display());
                println!("  argv: {}", argv.join(" "));
                let envs: Vec<String> = envmap.iter().map(|(k, v)| format!("{k}={v}")).collect();
                println!("  env:  {}", envs.join(" "));
                println!("  log:  {}", log.display());
                rows.push(Row { role: *grp_role, app: a.clone(), port: *port, pid: None, log: log.clone(), ready: None });
            }
            continue;
        }

        #[cfg(feature = "daemon")]
        if supervise {
            let mut client = devkit_ports::daemon::client::ensure_running()
                .context("starting supervisor daemon")?;
            for (a, port, argv, app_cwd, envmap, log) in &plans {
                let resp = client.request(&devkit_ports::daemon::proto::Request::Supervise {
                    holder: holder.clone(),
                    app: a.clone(),
                    role: *grp_role,
                    argv: argv.clone(),
                    cwd: app_cwd.to_str().context("app cwd not UTF-8")?.to_string(),
                    env: envmap.clone(),
                    logfile: log.clone(),
                    base_port: catalog[a].base_port,
                })?;
                let ready = match &resp {
                    devkit_ports::daemon::proto::Response::Supervised(v) =>
                        v.first().map(|(_, r)| *r).unwrap_or(false),
                    devkit_ports::daemon::proto::Response::Err(msg) => {
                        eprintln!("daemon could not supervise {a}: {msg}");
                        false
                    }
                    _ => false,
                };
                rows.push(Row { role: *grp_role, app: a.clone(), port: *port, pid: None, log: log.clone(), ready: Some(ready) });
            }
            continue; // skip the direct-spawn path for this group
        }

        // Spawn every app in the group, then poll all their ports concurrently so
        // readiness waits overlap instead of summing one 120s timeout per app.
        let mut spawned = Vec::with_capacity(plans.len());
        for (a, port, argv, app_cwd, envmap, log) in &plans {
            let pid = supervise::spawn_detached(argv, app_cwd.to_str().context("app cwd not UTF-8")?, envmap, log)?;
            registry::record_pid(*port, a, holder, *grp_role, pid, log.clone())?;
            spawned.push((a.clone(), *port, log.clone(), pid));
        }

        let ready: BTreeMap<String, bool> = std::thread::scope(|s| {
            let handles: Vec<_> = spawned
                .iter()
                .map(|(a, port, _, _)| {
                    let (a, port) = (a.clone(), *port);
                    s.spawn(move || (a, supervise::wait_ready(port, Duration::from_secs(120))))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        for (a, port, log, pid) in spawned {
            let is_ready = ready[&a];
            if !is_ready {
                eprintln!("--- {a} ({}) did not become ready; last 30 log lines: ---", grp_role.as_str());
                eprintln!("{}", supervise::tail(&log, 30));
            }
            rows.push(Row { role: *grp_role, app: a, port, pid: Some(pid), log, ready: Some(is_ready) });
        }
    }

    print_summary(&rows);
    Ok(())
}

fn cmd_down(cwd: &str, role: Option<Role>) -> Result<()> {
    let holder = toplevel(cwd)?;
    #[cfg(feature = "daemon")]
    if let Some(mut client) = devkit_ports::daemon::client::try_existing() {
        let resp = client.request(&devkit_ports::daemon::proto::Request::Down {
            holder: holder.clone(), role,
        })?;
        if let devkit_ports::daemon::proto::Response::Freed(freed) = resp {
            println!("stopped via daemon; released ports {freed:?}");
            return Ok(());
        }
    }
    // Stop and release under one lock, without pruning first: a still-running server
    // whose reservation looks stale must still receive SIGTERM and be released.
    let mut stopped = 0;
    let freed = registry::with_lock(|d| {
        for e in d.entries.values() {
            if e.holder == holder && role.is_none_or(|r| e.role == r)
                && let Some(pid) = e.pid { supervise::stop(pid); stopped += 1; }
        }
        Ok(d.release(&holder, role))
    })?;
    println!("stopped {stopped} process(es); released ports {freed:?}");
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
            None => println!("{}", registry::status_table(&registry::Data::default(), None)),
        }
    }
    Ok(())
}

fn cmd_logs(cwd: &str, app: &str, role: Option<Role>, follow: bool) -> Result<()> {
    let holder = toplevel(cwd)?;
    let data = registry::snapshot()?;
    let log = data.entries.values()
        .find(|e| e.holder == holder && e.app == app && role.is_none_or(|r| e.role == r))
        .and_then(|e| e.logfile.clone())
        .ok_or_else(|| anyhow::anyhow!("no tracked log for app `{app}` in this worktree"))?;
    if follow {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new("tail").arg("-f").arg(&log).exec();
        anyhow::bail!("failed to exec `tail -f`: {err}");
    }
    println!("{}", supervise::tail(&log, 200));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::apps_from_diff;
    #[test]
    fn picks_known_apps_from_diff() {
        let diff = " apps/api/server/x.ts | 2 +-\n apps/lab-os/page.tsx | 1 +\n packages/z/y.ts | 1 +\n";
        let known = vec!["api".to_string(), "lab-os".to_string(), "foundry-portal".to_string()];
        assert_eq!(apps_from_diff(diff, &known, "apps"), vec!["api", "lab-os"]);
    }
}
