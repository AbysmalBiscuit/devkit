use anyhow::Result;
use clap::{Parser, Subcommand};
use devkit_common::ui;
use devkit_ports::registry::{self, Data, Role};

#[derive(Parser)]
#[command(about = "Port registry for local dev servers")]
struct Cli {
    #[arg(short = 'C', long = "dir")]
    dir: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    Status,
    Alloc { #[arg(long)] holder: String, #[arg(long, value_enum, default_value = "issue")] role: RoleArg, apps: Vec<String> },
    Release { #[arg(long)] holder: String, #[arg(long, value_enum)] role: Option<RoleArg> },
    Prune,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum RoleArg { Issue, Baseline }
impl From<RoleArg> for Role {
    fn from(r: RoleArg) -> Self { match r { RoleArg::Issue => Role::Issue, RoleArg::Baseline => Role::Baseline } }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Status) {
        Cmd::Status => status()?,
        Cmd::Prune => { registry::with_lock(|d| { d.prune(); Ok(()) })?; println!("pruned"); }
        Cmd::Release { holder, role } => {
            let freed = registry::with_lock(|d| Ok(d.release(&holder, role.map(Into::into))))?;
            println!("released: {freed:?}");
        }
        Cmd::Alloc { holder, role, apps } => {
            let start = cli.dir.clone().unwrap_or_else(|| ".".into());
            let loaded = devkit_ports::load::load(None, std::path::Path::new(&start))?;
            let role: Role = role.into();
            let mut out = Vec::new();
            registry::with_lock(|d| {
                d.prune();
                for app in &apps {
                    let base = loaded.catalog.get(app)
                        .ok_or_else(|| anyhow::anyhow!("unknown app `{app}`"))?.base_port;
                    out.push((app.clone(), d.alloc_one(&holder, app, base, role)));
                }
                Ok(())
            })?;
            for (app, port) in out { println!("{app}={port}"); }
        }
    }
    Ok(())
}

fn status() -> Result<()> {
    let data: Data = registry::snapshot()?;
    let mut t = ui::table(&["PORT", "APP", "ROLE", "HOLDER", "PID", "LISTENING", "AGE"]);
    for (port, e) in &data.entries {
        let id = holder_label(&e.holder);
        t.add_row(vec![
            port.to_string(), e.app.clone(),
            format!("{:?}", e.role).to_lowercase(), id,
            e.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            if registry::listening(*port) { "yes".into() } else { "no".into() },
            format!("{}s", registry::now().saturating_sub(e.ts)),
        ]);
    }
    println!("{t}");
    Ok(())
}

fn holder_label(h: &str) -> String {
    std::path::Path::new(h).file_name().and_then(|s| s.to_str()).unwrap_or(h).to_string()
}
