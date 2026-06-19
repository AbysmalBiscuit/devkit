mod env;
mod supervise;
mod baseline;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use devkit_common::{cmd::git, paths, ui};
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
        role: RoleArg,
        #[arg(long = "env", value_name = "K=V")]
        env: Vec<String>,
        #[arg(long = "env-file")]
        env_file: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Stop servers and release ports for this worktree.
    Down {
        #[arg(long, value_enum)]
        role: Option<RoleArg>,
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
        role: Option<RoleArg>,
        #[arg(short = 'f', long)]
        follow: bool,
    },
}

#[derive(Clone, Copy, ValueEnum, PartialEq)]
enum RoleArg { Issue, Baseline, Both }

fn to_role(r: RoleArg) -> Role {
    match r { RoleArg::Baseline => Role::Baseline, _ => Role::Issue }
}

fn role_str(r: Role) -> &'static str {
    match r { Role::Issue => "issue", Role::Baseline => "baseline" }
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
pub fn apps_from_diff(diff_stat: &str, known: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    for line in diff_stat.lines() {
        if let Some(rest) = line.trim().strip_prefix("apps/") {
            if let Some(name) = rest.split('/').next() {
                if known.iter().any(|k| k == name) && !found.contains(&name.to_string()) {
                    found.push(name.to_string());
                }
            }
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
            role_str(r.role).to_string(),
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
    let cli = Cli::parse();
    let cwd = cwd_of(&cli);
    match &cli.cmd {
        Cmd::Up { apps, role, env, env_file, dry_run } =>
            cmd_up(&cli, &cwd, apps, *role, env, env_file.as_deref(), *dry_run),
        Cmd::Down { role } => cmd_down(&cwd, role.map(to_role)),
        Cmd::Status { all } => cmd_status(&cwd, *all),
        Cmd::Logs { app, role, follow } => cmd_logs(&cwd, app, role.map(to_role), *follow),
    }
}

fn cmd_up(
    cli: &Cli, cwd: &str, apps_arg: &[String], role: RoleArg,
    env_pairs: &[String], env_file: Option<&str>, dry_run: bool,
) -> Result<()> {
    let loaded = load::load(cli.config.as_deref().map(Path::new), Path::new(cwd))?;
    let cfg = &loaded.config;
    let catalog = &loaded.catalog;
    let known: Vec<String> = catalog.keys().cloned().collect();

    let mut apps: Vec<String> = if !apps_arg.is_empty() {
        apps_arg.to_vec()
    } else {
        let diff = git(&["diff", &format!("{}...HEAD", cfg.defaults.baseline_ref), "--stat"], cwd)
            .unwrap_or_default();
        apps_from_diff(&diff, &known)
    };
    for a in &apps { anyhow::ensure!(catalog.contains_key(a), "unknown app `{a}`"); }
    anyhow::ensure!(
        !apps.is_empty(),
        "no apps to run (none given and none detected in diff vs {})",
        cfg.defaults.baseline_ref
    );
    // Ensure api is present whenever a webapp consumer is selected, so it can be wired.
    let needs_api = apps.iter().any(|a| catalog[a].url_env.is_some() && a != "api");
    if needs_api && catalog.contains_key("api") && !apps.iter().any(|a| a == "api") {
        apps.insert(0, "api".to_string());
    }

    let user = parse_user_env(env_pairs, env_file)?;
    let issue_holder = toplevel(cwd)?;

    // (role, holder, base_dir) — base_dir is where <app.path> is rooted.
    let groups: Vec<(Role, String, PathBuf)> = {
        let baseline_path = expand_tilde(&cfg.defaults.baseline_path);
        let mut g = Vec::new();
        if matches!(role, RoleArg::Issue | RoleArg::Both) {
            g.push((Role::Issue, issue_holder.clone(), PathBuf::from(&issue_holder)));
        }
        if matches!(role, RoleArg::Baseline | RoleArg::Both) {
            let bp = baseline_path.to_str().context("baseline_path not UTF-8")?.to_string();
            baseline::ensure_fresh(&issue_holder, &bp, &cfg.defaults.baseline_ref)?;
            g.push((Role::Baseline, bp.clone(), baseline_path));
        }
        g
    };

    let mut rows: Vec<Row> = Vec::new();
    for (grp_role, holder, base_dir) in &groups {
        let mut ports: BTreeMap<String, u16> = BTreeMap::new();
        registry::with_lock(|d| {
            d.prune();
            for a in &apps {
                let base = catalog[a].base_port;
                ports.insert(a.clone(), d.alloc_one(holder, a, base, *grp_role));
            }
            Ok(())
        })?;
        let api_port = ports.get("api").copied();

        for a in &apps {
            let app = &catalog[a];
            let port = ports[a];
            let mut argv = env::doppler_prefix(app, &cfg.defaults.doppler_config);
            argv.extend(env::launch_argv(app, port));
            let app_cwd = base_dir.join(&app.path);
            let envmap = env::env_for(app, api_port, &user);
            let log = paths::logs_dir()
                .join(slug(holder))
                .join(format!("{}-{}.log", role_str(*grp_role), a));

            if dry_run {
                println!("[{}] {a} :{port}", role_str(*grp_role));
                println!("  cwd:  {}", app_cwd.display());
                println!("  argv: {}", argv.join(" "));
                let envs: Vec<String> = envmap.iter().map(|(k, v)| format!("{k}={v}")).collect();
                println!("  env:  {}", envs.join(" "));
                println!("  log:  {}", log.display());
                rows.push(Row { role: *grp_role, app: a.clone(), port, pid: None, log, ready: None });
                continue;
            }

            let pid = supervise::spawn_detached(&argv, app_cwd.to_str().context("app cwd not UTF-8")?, &envmap, &log)?;
            registry::with_lock(|d| { d.record_pid(port, pid, log.clone()); Ok(()) })?;
            let ready = supervise::wait_ready(port, Duration::from_secs(120));
            if !ready {
                eprintln!("--- {a} ({}) did not become ready; last 30 log lines: ---", role_str(*grp_role));
                eprintln!("{}", supervise::tail(&log, 30));
            }
            rows.push(Row { role: *grp_role, app: a.clone(), port, pid: Some(pid), log, ready: Some(ready) });
        }
    }

    print_summary(&rows);
    Ok(())
}

fn cmd_down(cwd: &str, role: Option<Role>) -> Result<()> {
    let holder = toplevel(cwd)?;
    let data = registry::snapshot()?;
    let mut stopped = 0;
    for e in data.entries.values() {
        if e.holder == holder && role.map_or(true, |r| e.role == r) {
            if let Some(pid) = e.pid { supervise::stop(pid); stopped += 1; }
        }
    }
    let freed = registry::with_lock(|d| Ok(d.release(&holder, role)))?;
    println!("stopped {stopped} process(es); released ports {freed:?}");
    Ok(())
}

fn cmd_status(cwd: &str, all: bool) -> Result<()> {
    let holder = toplevel(cwd).ok();
    let data = registry::snapshot()?;
    let mut t = ui::table(&["PORT", "APP", "ROLE", "HOLDER", "PID", "LISTENING", "AGE"]);
    for (port, e) in &data.entries {
        if !all {
            match &holder {
                Some(h) if &e.holder == h => {}
                _ => continue,
            }
        }
        t.add_row(vec![
            port.to_string(),
            e.app.clone(),
            role_str(e.role).to_string(),
            slug(&e.holder),
            e.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            if registry::listening(*port) { "yes".into() } else { "no".into() },
            format!("{}s", registry::now().saturating_sub(e.ts)),
        ]);
    }
    println!("{t}");
    Ok(())
}

fn cmd_logs(cwd: &str, app: &str, role: Option<Role>, follow: bool) -> Result<()> {
    let holder = toplevel(cwd)?;
    let data = registry::snapshot()?;
    let log = data.entries.values()
        .find(|e| e.holder == holder && e.app == app && role.map_or(true, |r| e.role == r))
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
        assert_eq!(apps_from_diff(diff, &known), vec!["api", "lab-os"]);
    }
}
